//! Architectural invariants enforced as plain tests — the mechanism for
//! structural rules that clippy cannot express. Drop this file into a crate's
//! `tests/` directory; it walks that crate's `src/` and asserts the invariant,
//! failing as a red `cargo test` in the normal TDD loop.
//!
//! implements: jig::rust::prefer-file-modules

use std::path::{Path, PathBuf};

/// Recursively collect every file named `mod.rs` under `dir`.
fn find_mod_rs(dir: &Path, hits: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_mod_rs(&path, hits);
        } else if path.file_name().is_some_and(|name| name == "mod.rs") {
            hits.push(path);
        }
    }
}

/// rule: jig::rust::prefer-file-modules — prefer `foo.rs` over `foo/mod.rs`.
#[test]
fn no_mod_rs_files() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    find_mod_rs(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "rule jig::rust::prefer-file-modules: use `foo.rs` beside `foo/`, not \
         `foo/mod.rs`. Offending files: {hits:#?}"
    );
}
