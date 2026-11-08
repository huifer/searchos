//! End-to-end CLI smoke tests (RFC-016 Phase 8: production acceptance).
//!
//! These spawn the real `searchos` binary with an isolated data dir and
//! exercise the Part-18 command contract. They require `cargo build` to
//! have produced target/debug/searchos; CI runs them after the build.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    // target/debug/deps/searchos-e2e-<hash> → target/debug/searchos
    let deps = exe.parent().unwrap();
    let target = deps.parent().unwrap();
    let bin = target.join("searchos");
    assert!(
        bin.exists(),
        "searchos binary not found at {}",
        bin.display()
    );
    bin
}

fn cmd(bin: &PathBuf, data_dir: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .env("SEARCHOS_DATA_DIR", data_dir)
        .env("RUST_LOG", "warn")
        .output()
        .expect("spawn searchos")
}

fn assert_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not JSON: {e}\nstdout={stdout}"))
}

#[test]
fn version_and_init_doctor_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let bin = binary();

    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "version"],
    );
    assert!(out.status.success());
    let v = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert!(v["name"] == "searchos");
    assert!(v["version"].is_string());

    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "init"],
    );
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let i = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(i["status"], "initialized");
    assert!(i["schema_version"].as_i64().unwrap() >= 1);

    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "doctor"],
    );
    assert!(out.status.success());
    let d = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(d["database_ok"], true);
    assert_eq!(d["index"]["consistent"], true);
    assert_eq!(
        d["schema_version"].as_i64().unwrap(),
        i["schema_version"].as_i64().unwrap()
    );
}

#[test]
fn search_local_only_records_trace_and_cost() {
    let dir = tempfile::tempdir().unwrap();
    let bin = binary();

    // Seed local knowledge through the fetch pipeline (offline page via
    // fixtures is not reachable; use an inline data: URL? No — SSRF blocks
    // non-http. Use the fixture-based local search with a pre-seeded DB
    // built by a Rust helper is heavy; instead validate the command
    // contract + decision path with the empty corpus: local_only on an
    // empty corpus returns a response with a trace id and recorded cost.)
    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &[
            "--format",
            "json",
            "search",
            "tokio async runtime",
            "--mode",
            "local-only",
            "--workspace",
            "ws_default",
        ],
    );
    assert!(
        out.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resp = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert!(resp["trace_id"].as_str().is_some(), "trace id returned");
    assert!(resp["decision"]["path"].is_string());
    assert!(resp["quality"]["status"].is_string());

    // The trace must be retrievable (Decision Trace 覆盖率 100%).
    let trace_id = resp["trace_id"].as_str().unwrap();
    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "trace", "show", trace_id],
    );
    assert!(out.status.success());
    let trace = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        trace["selected_path"].as_str().unwrap(),
        resp["decision"]["path"].as_str().unwrap()
    );
    assert!(!trace["intents"].as_array().unwrap().is_empty());

    // Cost summary reflects the recorded task.
    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "cost", "summary"],
    );
    assert!(out.status.success());
    let cost = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert!(cost["summary"]["event_count"].as_u64().unwrap() >= 1);
    assert_eq!(cost["is_estimate"], true);
}

#[test]
fn output_formats_and_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let bin = binary();

    // Human format (default) prints a human block on stdout.
    let out = cmd(&bin, &dir.path().to_path_buf(), &["version"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("version"));

    // Markdown format renders a markdown block.
    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "markdown", "version"],
    );
    assert!(out.status.success());
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("**version**"), "markdown output: {md}");

    // Bad mode → validation error, non-zero exit, stderr carries the error.
    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["search", "q", "--mode", "bogus"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error"), "stderr: {stderr}");
}

#[test]
fn index_status_and_crawler_extension_commands() {
    let dir = tempfile::tempdir().unwrap();
    let bin = binary();

    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "index", "status"],
    );
    assert!(out.status.success());
    let st = assert_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(st["consistent"], true);

    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "crawler", "list"],
    );
    assert!(out.status.success());
    let crawlers = assert_json(&String::from_utf8_lossy(&out.stdout));
    // The repo extensions/crawlers directory is discovered by default.
    assert!(crawlers["crawlers"].as_array().is_some());

    let out = cmd(
        &bin,
        &dir.path().to_path_buf(),
        &["--format", "json", "extension", "validate"],
    );
    assert!(out.status.success());
}
