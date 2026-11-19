//! Shared helpers for the e2e suites.
//!
//! Cargo does not guarantee where a test executable lives: the classic
//! `target/<profile>/deps/<test>` layout and the newer
//! `target/<profile>/build/<pkg>/<hash>/out/<test>` layout both occur. These
//! suites spawn the REAL `searchos` binary, so they must resolve it the same
//! portable way instead of deriving a path from `current_exe()` alone (that
//! assumption failed on CI while passing locally, where the binary already
//! existed).

use std::path::PathBuf;

/// Locates the `searchos` binary.
///
/// Order:
/// 1. `SEARCHOS_BIN` — set by CI, authoritative when present;
/// 2. the workspace `target/<profile>/searchos`, found by walking up from the
///    test executable to the `target` component;
/// 3. `target/debug/searchos` or `target/release/searchos` relative to the
///    current directory.
pub fn binary() -> PathBuf {
    if let Ok(explicit) = std::env::var("SEARCHOS_BIN") {
        let path = PathBuf::from(explicit);
        assert!(
            path.exists(),
            "SEARCHOS_BIN points at {} but the file does not exist — run `cargo build --bin searchos` first",
            path.display()
        );
        return path;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let components: Vec<_> = exe.components().collect();
    let mut candidates: Vec<PathBuf> = Vec::new();
    for (index, component) in components.iter().enumerate() {
        if component.as_os_str() == "target" {
            if let Some(profile) = components.get(index + 1) {
                let mut dir = PathBuf::new();
                for c in &components[..=index] {
                    dir.push(c.as_os_str());
                }
                candidates.push(dir.join(profile.as_os_str()).join("searchos"));
            }
        }
    }
    candidates.push(PathBuf::from("target/debug/searchos"));
    candidates.push(PathBuf::from("target/release/searchos"));
    for bin in &candidates {
        if bin.exists() {
            return bin.clone();
        }
    }
    panic!(
        "searchos binary not found (looked at {candidates:?}) — run `cargo build --bin searchos` first, or set SEARCHOS_BIN",
    );
}
