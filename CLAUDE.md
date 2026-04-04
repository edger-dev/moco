# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

moco is a general-purpose plugin framework where **cells** provide composable functionality through typed **functions**, with specs defined via Styx schemas. Early-stage (v0.1.0-dev), Rust 2024 edition, requires rustc 1.93.0+.

## Build & Development Commands

```bash
# Build
cargo build
cargo check

# Test
cargo test                          # all tests
cargo test -p moco-core             # single crate
cargo test test_name                # single test by name

# Lint
cargo clippy --all-targets

# Development with bacon (hot-reload)
bacon                               # default: cargo check
bacon clippy                        # clippy with --deny warnings
bacon test                          # run tests with hot-reload

# Nix
nix build                           # reproducible build
nix flake check                     # build + clippy + fmt

# Mise tasks
mise run check                      # type-check workspace
mise run fmt                        # format all Rust code
mise run _bacon-claude-diagnostics  # bacon background checker
mise run _docs-serve                # mdbook live-reload dev server
mise run docs-build                 # build mdbook static site
mise run jig-update                 # update jig to latest
mise run jig-update-local           # override jig with local checkout
```

## Workspace Structure

```
crates/
  moco-internals/   # Foundation layer: Env trait, dependency re-exports
  moco-macros/      # Procedural macros (proc-macro crate)
  moco-core/        # Core traits and specs: Cell, Func, CellSpec, FuncSpec
cells/
  moco-styx/        # Styx cell implementation (depends on moco-core)
  moco-tty/         # TTY cell implementation (depends on moco-core)
```

**Dependency flow:** `moco-internals` → `moco-macros` → `moco-core` → cells (`moco-styx`, `moco-tty`)

## Architecture

**Core design:** trait-based plugin system with static metadata specs.

- **Cell** — a plugin unit implementing the `Cell` trait, carrying `CellSpec` (name, version, title, description). Each cell exposes one or more Funcs.
- **Func** — a typed operation exposed by a cell. Depending on input/output types, it can act as an API endpoint, data transformer, or event handler. Carries `FuncSpec` metadata.
- **Env** — an execution context providing runtime dependencies/configuration for cells. Defined in `moco-internals`.
- **Spec structs** (`CellSpec`, `FuncSpec`, `Spec`) derive `Facet` for schema/reflection.
- **Styx** is used for serialization (instead of serde) and schema definition. The `moco-styx` cell will read Styx specs and convert them to other formats (Polars schemas, database DDL, JSON Schema, etc.).
- `moco-internals` re-exports shared dependencies (facet, roam, semver) via its `deps` module; downstream crates access them through `moco_core::deps`.

Key external libraries:
- **facet** (+ facet-styx): Schema derivation, reflection, and Styx serialization
- **rootcause**: Error handling — used project-wide for error types, context, and propagation
- **fastrace**: Tracing — used project-wide for tracing
- **roam**: From `github.com/bearcove/roam` - as RPC framework for communication between cells and runtime
- **rstest**: Parameterized tests

## Workflow

- **Do not commit.** The user handles all git commits manually.
- Uses Nix flakes (`flake.nix`) + direnv (`.envrc`) for reproducible dev environments
- Uses `mise` as task runner and `bacon` for hot-reload development
- Jujutsu (`.jj/`) is used alongside Git for version control

<!-- jig:rust -->
## Rust Workflow

bacon is running in the background and continuously writes compiler
diagnostics to `.bacon-claude-diagnostics` in the project root.

Before attempting to fix compiler errors, read `.bacon-claude-diagnostics` to see
current errors and warnings with their exact file/line/column locations.
Prefer reading this file over running `cargo check` yourself — it's
already up to date and costs no compile time.

Each line in `.bacon-claude-diagnostics` uses a pipe-delimited format:

```
level|:|file|:|line_start|:|line_end|:|message|:|rendered
```

- `level` — severity: `error`, `warning`, `note`, `help`
- `file` — relative path to the source file
- `line_start` / `line_end` — affected line range
- `message` — short diagnostic message
- `rendered` — full cargo-rendered output including code context and suggestions

After making changes, wait a moment for bacon to recompile, then re-read
`.bacon-claude-diagnostics` to verify the fix.

**All compiler warnings must be fixed before committing.** Zero warnings is the
standard. Check `.bacon-claude-diagnostics` for warnings (not just errors) and
resolve them as part of every change.

If `.bacon-claude-diagnostics` is absent or clearly stale (e.g. the file doesn't
exist after the first save), warn the user that bacon does not appear to
be running and ask them to start it in a Zellij pane with `mise run _bacon-claude-diagnostics`.

## Test-Driven Development

Write tests **before** implementation. The sequence:

1. Write tests that capture the expected behavior from the spec
2. Run `cargo test --workspace` — confirm tests fail for the right reasons (not compilation errors from missing types, but assertion failures or missing functionality)
3. Implement the minimum code to make tests pass
4. Verify all tests pass (not just the new ones)
<!-- /jig:rust -->

<!-- jig:beans -->
## Planning

Do NOT write design docs or plans to `docs/plans/`. All planning and design
work should be captured directly in beans (description + body). Beans are the
single source of truth for tracking work.

Do NOT start implementation during the planning stage. The outcome of planning
is beans with clear specs — enough detail for a clean design and implementation
stage later.

## Commit Granularity

Each task should produce 2–3 focused commits:

1. **Tests commit** — the failing tests that define the expected behavior
2. **Implementation commit** — the code that makes them pass, plus any warning fixes
3. **Review fixes commit** (if needed) — issues caught during code review

Each commit should include updated bean files (checked-off todo items, status changes).

## Code Review

After the implementation commit, do a code review before considering the task done.
Prefer spawning a subagent for a fresh perspective — it should review the last 1–2
commits looking for: logic errors, missed edge cases, violations of existing code
patterns, missing test coverage, and clippy-level issues (unnecessary clones, unused
imports, etc.). If a subagent isn't available, self-review by re-reading the full diff.

Fix any real issues found, then commit the fixes separately.

## Acceptance Criteria

Every task must pass before being marked complete:

- All tests pass
- Zero compiler warnings
- Bean todo items all checked off
- Bean marked as `completed` with a `## Summary of Changes` section
- Changes committed with descriptive messages
<!-- /jig:beans -->

<!-- jig:docs -->
## Documentation

This project uses [mdbook](https://rust-lang.github.io/mdBook/) for documentation.
Source files are in `docs/src/`, built output goes to `docs/dist/`.

- `mise run _docs-serve` — start live-reload dev server
- `mise run docs-build` — build static site

When making changes that affect user-facing behavior, update the relevant
documentation in `docs/src/` as part of the same commit.
<!-- /jig:docs -->
