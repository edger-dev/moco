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

<!-- jig:kinora -->
## Engineering with kinora: specs · tasks · plans

This repo runs its engineering intent out of **kinora** (the `.kinora/` ledger),
not loose markdown. There is **no `docs/plans/`**. This is the *workflow*;
`kinora <cmd> --help` is the reference. The `kinora` CLI is on `PATH` in the dev
shell (provided by the kinora jig).

### The model

Engineering intent is version-controlled alongside the code. Three roots are
declared in `.kinora/config.styx`, all `policy "never"` (nothing auto-swept):

```
roots {
  specs { policy "never" }   # durable contracts — the source of truth
  tasks { policy "never" }   # the open work + its plan — transient
  inbox { policy "never" }   # findings / rough edges for the tooling itself
}
```

A **kino** is one content-addressed, versioned document with a **stable id**
(identity hash — survives renames and re-versions) and a per-version hash. You
reference one kino from another by **stable id**, never by title; in the body an
`kino://<id>` token renders as a link. Three shapes, one job each:

| Kino | Root | Holds |
|------|------|-------|
| **spec** | `specs` | one atomic contract — a single decision + its rationale (durable) |
| **task** | `tasks` | the goal, the spec ids it realizes, its plan id, the done-when |
| **plan** | `tasks` | the phases of one task + status notes in prose |

**One spec = one atomic contract.** If a spec needs the word *and* to describe
what it covers, split it. A spec states what was chosen, what was rejected and
why, and the escape hatch. `kinora resolve "<name>"` / `kinora resolve <id>`
prints one kino; `kinora render` builds the whole ledger into an mdbook.

### The iron rule: render reads *committed* state

`kinora render` and `kinora resolve` read git-**committed** `.kinora/` state, not
the working tree. The full loop for any kino change is always:

```
kinora store …      # write the new kino version into the ledger (staged)
kinora commit       # fold the declared roots into a kinora commit  (main only)
git commit          # (or at least `git add .kinora`) — now it's real
kinora render       # optional: rebuild the mdbook from committed state
```

Skipping `git commit` is the most common mistake. If a render looks stale, you
forgot it.

### Concurrent worktrees: stage on the branch, `kinora commit` only on `main`

kinora has no ledger merge yet. `store`/`assign` write only additive, hash-named
files under `.kinora/staged/` and `.kinora/store/` (union-merge, zero conflict);
only `commit` touches `roots/*`. So in a feature branch: **stage, never commit**
— `kinora store …` then `git commit` the staged files (no `kinora commit` on a
branch). **Promote on `main`**: after the branch merges, run a single
`kinora commit` on `main`, then `git commit` the `.kinora` change. A staged id is
content-derived and stable, so it is valid for a `kino://`/`// implements:` ref
immediately and survives promotion.

### Bracket every code change with spec + task maintenance — before *and* after

**Before coding:** identify the spec(s) the change realizes and the open task
that tracks it. If none exists, **write the atomic spec kino + the task/plan kino
first** — intent leads the work.

**After coding:** reconcile intent with what you built — re-version the affected
spec if behavior changed and fix any drifted `// implements:` ref; update the
plan kino's status note; when the task is done mark **both the task and the plan**
`-m status::resolved=true`; then `kinora commit` (on main) → `git commit`.

### Writing code (TDD), linked to intent

1. Write tests **first** that capture the spec; confirm they fail for the right
   reason (assertion/behavior, not a missing type).
2. Implement the minimum to pass. Whole suite green; linter clean (zero warnings).
3. **Link code back to intent** — each module/fn that realizes a spec cites it:
   ```
   // implements: <spec-id>@<version>   (strong — this code fulfils that contract)
   // task: <task-id>@<version>         (weak — audit trail to the work item)
   ```
   Get `<version>` from `kinora resolve` (at v1 the version equals the id). When a
   spec is re-versioned and its behavior changed, grep its `// implements:` refs
   and bump the version so the link doesn't silently drift.

### Commit cadence per phase

1. **tests** commit
2. **implementation** commit (code + warning fixes)
3. **review-fixes** commit (if any) — after the impl commit, code-review the last
   1–2 commits (prefer a fresh subagent) and fix in a separate commit.

As a phase finishes, update the plan kino's status note (re-store → `kinora
commit` on main → `git commit`). A task is **done** when: tests pass, warnings
are zero, the spec kinos are satisfied, the plan notes mark its phases done, both
the task and plan kinos are `-m status::resolved=true`, and everything —
including `.kinora` — is committed.

### Re-versioning (the mechanics)

```
kinora store <kind> <path> --id <stable-id> --parents <prev-head-hash>
#            ^ markdown|text|…   ^ pin identity   ^ current head
```

- A bare re-version with no `--root` inherits the kino's existing root (not swept
  to inbox). **Re-pass `--name` and any `-m` metadata** — metadata is per-version,
  not inherited.
- Re-versioning an already-committed kino resolves to a single head (no fork).
- A re-version with *identical bytes* may be rejected as a same-bytes self-parent
  — change something real, or set metadata in the same store as a content change.
- Merge a stray two-head fork with `--parents <newhead>,<oldhead>`.

### Cross-referencing

A task lists the **spec ids** it realizes and its **plan id**; a plan back-links
its **task id** — always by stable id (`kino://<id>`). The **inbox** root is for
findings/rough edges about the tooling itself, one kino per finding, so they feed
back into the backlog.

### TL;DR checklist

- [ ] Before code: the spec(s) and task/plan exist. If not, write them **first**.
- [ ] Tests first, from the spec. Fail for the right reason.
- [ ] Minimum code to pass. Zero warnings.
- [ ] `// implements: <spec-id>@<ver>` on the code that realizes a spec.
- [ ] After code: re-version drifted specs; fix drifted refs; update the plan note.
- [ ] Per phase: tests commit → impl commit → review-fixes commit.
- [ ] Task done: both task **and** plan `-m status::resolved=true`.
- [ ] Every kino change: `kinora store` → `kinora commit` (main) → `git commit` `.kinora`.
<!-- /jig:kinora -->

<!-- jig:docs -->
## Documentation

This project uses [mdbook](https://rust-lang.github.io/mdBook/) for documentation.
Source files are in `docs/src/`, built output goes to `docs/dist/`.

- `mise run _docs-serve` — start live-reload dev server
- `mise run docs-build` — build static site

When making changes that affect user-facing behavior, update the relevant
documentation in `docs/src/` as part of the same commit.
<!-- /jig:docs -->
