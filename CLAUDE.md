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
bacon clippy-all                    # clippy on all targets
bacon test                          # run tests with hot-reload
bacon test -- test_name             # specific test
bacon nextest run                   # parallel test runner

# Nix
nix build                           # reproducible build
nix flake check                     # build + clippy + coverage

# Infrastructure tools
just install-tools                  # install rustfs, s2-cli, styx-cli, etc.
just start-rustfs                   # start RustFS (S3-compatible storage)
just start-s2lite                   # start S2Lite server
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
- Uses `just` as task runner and `bacon` for hot-reload development
- Jujutsu (`.jj/`) is used alongside Git for version control
