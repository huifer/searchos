//! Backup and restore drill (RFC-018 18.5 / 18.6).
//!
//! Spawns the real `searchos` binary through a full round trip:
//! ingest → backup create → wipe the data dir → backup restore → verify the
//! knowledge survived and is searchable again.
//!
//! This is the operational rehearsal the deploy runbook references; it runs in
//! CI so a broken restore path fails a pull request instead of a recovery
//! attempt.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let target = deps.parent().unwrap();
    let bin = target.join("searchos");
    assert!(
        bin.exists(),
        "searchos binary not found at {} — run `cargo build --bin searchos` first",
        bin.display()
    );
    bin
}

fn run(bin: &Path, data_dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .env("SEARCHOS_DATA_DIR", data_dir)
        .env("RUST_LOG", "warn")
        .output()
        .unwrap_or_else(|e| panic!("spawn searchos {args:?}: {e}"))
}

/// Runs a command and returns its parsed JSON stdout.
fn json(bin: &Path, data_dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = run(bin, data_dir, args);
    assert!(
        output.status.success(),
        "searchos {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The CLI may emit config warnings on stderr only; stdout is one JSON doc.
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "searchos {args:?} returned non-JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn backup_restore_roundtrip_preserves_knowledge() {
    let bin = binary();
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // 1. A workspace with one ingested document.
    json(&bin, &data_dir, &["--format", "json", "init"]);
    let ingested = json(
        &bin,
        &data_dir,
        &[
            "--format",
            "json",
            "ingest",
            "--text",
            "backup drill content about reciprocal rank fusion",
            "--source-key",
            "notes/backup-drill",
            "--title",
            "Backup drill note",
        ],
    );
    let document_id = ingested["document_id"]
        .as_str()
        .unwrap_or_else(|| panic!("ingest returned no document_id: {ingested}"))
        .to_string();

    // 2. A consistent backup with a checksum manifest.
    let backup = json(&bin, &data_dir, &["--format", "json", "backup", "create"]);
    let backup_dir = PathBuf::from(
        backup["backup"]
            .as_str()
            .unwrap_or_else(|| panic!("backup create returned no path: {backup}")),
    );
    assert!(
        backup_dir.join("metadata.sqlite").exists(),
        "metadata.sqlite missing"
    );
    assert!(
        backup_dir.join("manifest.json").exists(),
        "manifest.json missing"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(backup_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["sqlite_sha256"].as_str().map(str::len),
        Some(64),
        "manifest must carry a sha256: {manifest}"
    );
    assert_eq!(
        manifest["secrets"], "[never included]",
        "backups must never contain secrets: {manifest}"
    );

    // 3. Destroy the live metadata, then restore into the same data dir.
    std::fs::remove_file(data_dir.join("data").join("searchos.db")).expect("remove live database");
    let restored = json(
        &bin,
        &data_dir,
        &[
            "--format",
            "json",
            "backup",
            "restore",
            backup_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(restored["hash_verified"], true, "{restored}");
    assert_eq!(restored["integrity_check"], "ok", "{restored}");

    // 4. The document is back, with its version history.
    let doc = json(
        &bin,
        &data_dir,
        &["--format", "json", "document", "inspect", &document_id],
    );
    assert_eq!(
        doc["document"]["title"].as_str(),
        Some("Backup drill note"),
        "restored document must keep its title: {doc}"
    );
    assert!(
        doc["current_version"].is_object(),
        "restored document must keep its version: {doc}"
    );

    // 5. Indexes are rebuildable from the restored SQLite (ADR-022).
    let rebuild = json(&bin, &data_dir, &["--format", "json", "index", "rebuild"]);
    assert!(
        rebuild["rebuilt_documents"].as_u64().unwrap_or(0) >= 1,
        "rebuild must re-index the restored chunks: {rebuild}"
    );
    let status = json(&bin, &data_dir, &["--format", "json", "index", "status"]);
    assert_eq!(
        status["consistent"], true,
        "index must be consistent after rebuild: {status}"
    );
}

/// A tampered backup must be refused, not restored.
#[test]
fn restore_refuses_a_tampered_backup() {
    let bin = binary();
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    json(&bin, &data_dir, &["--format", "json", "init"]);
    json(
        &bin,
        &data_dir,
        &[
            "--format",
            "json",
            "ingest",
            "--text",
            "tamper drill content",
            "--source-key",
            "notes/tamper",
        ],
    );
    let backup = json(&bin, &data_dir, &["--format", "json", "backup", "create"]);
    let backup_dir = PathBuf::from(backup["backup"].as_str().unwrap());

    // Flip one byte in the snapshot, leaving the manifest checksum intact.
    let db = backup_dir.join("metadata.sqlite");
    let mut bytes = std::fs::read(&db).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&db, &bytes).unwrap();

    let output = run(
        &bin,
        &data_dir,
        &[
            "--format",
            "json",
            "backup",
            "restore",
            backup_dir.to_str().unwrap(),
        ],
    );
    assert!(
        !output.status.success(),
        "restore must fail on a checksum mismatch\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("checksum"),
        "the failure must name the checksum: {stderr}"
    );
}
