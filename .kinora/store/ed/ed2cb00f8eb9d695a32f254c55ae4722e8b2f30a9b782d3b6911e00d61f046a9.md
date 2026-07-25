# Rule: prefer `foo.rs` over `foo/mod.rs`

A module with children is declared as `foo.rs` alongside a `foo/` directory —
never as `foo/mod.rs`. This is the Rust 2018+ path convention: it keeps every
module's source in a distinctly-named file, so an editor's open-tabs and a
file search show `parser.rs` instead of a wall of identical `mod.rs` tabs.

This is a **structural** rule — it constrains the shape of the crate, not a
line of code — and it is the worked example for the architectural-test
mechanism: a rule clippy cannot see, enforced by a plain `#[test]` that walks
the source tree.

**Rejected:** `mod.rs` "because that's how it's always been." The flat-file
convention has been idiomatic since the 2018 edition; the only cost of holding
the line is one test.

**Mechanism:** architectural test (rung 1) — `tests/architecture.rs` asserts no
file under `src/` is named `mod.rs`. It fails as a red `cargo test` in the
normal TDD loop, pointing at the offending path. See the architecture-test
example shipped with the rust jig.

**Escape hatch:** `#[ignore]` the assertion with a comment, or scope the walk to
exclude a vendored subtree that must keep upstream's layout.
