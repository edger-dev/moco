# moco

A general-purpose plugin framework where **cells** provide composable functionality through typed **functions**, with specs defined via [Styx](https://github.com/nicholasgasior/styx) schemas.

## Core Concepts

### Cell

A cell is a plugin/extension unit in the moco framework. Each cell carries static metadata (`CellSpec`: name, version, title, description) and exposes one or more **functions**. Cells are defined by implementing the `Cell` trait from `moco-core`.

### Func

A function is a typed operation exposed by a cell. Depending on its input and output types, a function can act as an API endpoint, a data transformer, or an event handler. Each function carries its own metadata (`FuncSpec`: name, title, description) via the `Func` trait.

### Env

An execution context that provides the runtime dependencies and configuration cells operate within. Defined as the `Env` trait in `moco-internals`, each environment also carries a `Spec` with identity and version metadata.

### Styx & Facet

Styx is used as the serialization format (replacing serde) and as the schema definition language for cell specs. The [facet](https://docs.rs/facet) library provides schema derivation and reflection via `#[derive(Facet)]` on spec structs. This enables:

- Structured spec definitions for cells, functions, and environments
- Schema conversion to other formats (Polars schemas, database DDL, JSON Schema, etc.) via the planned `moco-styx` cell

## Workspace Structure

```
crates/
  moco-internals/   Foundation: Env trait, shared dependency re-exports
  moco-macros/      Procedural macros
  moco-core/        Core traits (Cell, Func) and spec structs (CellSpec, FuncSpec)
cells/
  moco-styx/        Planned: reads Styx specs and converts to other formats
  moco-tty/         TTY/terminal cell
```

**Dependency flow:** `moco-internals` -> `moco-macros` -> `moco-core` -> cells

Shared external dependencies (facet, roam, semver) are re-exported through `moco-internals::deps` and accessed downstream via `moco_core::deps`.

## Getting Started

Requires Rust 1.93.0+ (2024 edition). The project uses Nix flakes for reproducible environments.

```bash
# With Nix + direnv (recommended)
direnv allow        # sets up the dev environment automatically

# Build and test
cargo build
cargo test

# Development with hot-reload
bacon              # default: cargo check
bacon test         # run tests on file change
bacon clippy-all   # lint on file change

# Infrastructure (optional backends)
just install-tools          # install rustfs, s2-cli, styx-cli
just start-rustfs           # S3-compatible local storage
just start-s2lite           # S2Lite server
```

## License

MIT OR Apache-2.0
