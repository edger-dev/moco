## Context

The `moco-tty` crate is a placeholder with no real implementation. It currently depends on `faketty`, which is a CLI binary (not a library). The actual PTY primitives come from the `nix` crate (`nix::pty`), which `faketty` uses internally. The moco framework's `Cell`/`Func` traits are simple static-spec carriers — `Cell` has a `CellSpec` and exposes `Func`s, each with a `FuncSpec`.

The goal is to make `moco-tty` a functional cell that can spawn shell processes under a PTY, capture their I/O, and make that data available to other cells.

## Goals / Non-Goals

**Goals:**

- Spawn shell processes with PTY allocation using `nix::pty::openpty` so programs behave as if connected to a real terminal
- Manage process lifecycle: start, stop, query status
- Capture stdout data from managed processes via the PTY master fd
- Forward stdin data to managed processes via the PTY master fd
- Implement `Cell` and `Func` traits for integration with the moco plugin framework
- Provide a clean API for other cells to consume I/O data

**Non-Goals:**

- Windows support (PTY is Unix-specific)
- Terminal emulation / ANSI parsing (raw bytes only; consumers interpret them)
- Session multiplexing (one process per `ShellProcess` instance)
- Async runtime — keep synchronous initially; async can be layered later
- The `faketty` binary itself is not used as a library dependency

## Decisions

### 1. Replace `faketty` dependency with `nix`

**Decision**: Remove the `faketty` crate dependency; depend on `nix` (with feature `term`) directly.

**Rationale**: `faketty` is a CLI tool, not a library. Its internal PTY logic is simple and based on `nix::pty`. We need `nix::pty::openpty` and standard `nix::unistd::fork`/`nix::unistd::execvp` — using `nix` directly gives us full control without depending on a binary crate's internals. The `nix` crate is already a transitive dependency.

**Alternatives considered**:
- *Use `portable-pty`*: Higher-level, cross-platform, but adds a heavy dependency and we don't need Windows support.
- *Fork `faketty` as a library*: Unnecessary complexity; the relevant code is ~50 lines of `nix` calls.

### 2. Module structure

**Decision**: Organize `moco-tty` into three modules:

```
cells/moco-tty/src/
  lib.rs          # Cell impl, re-exports
  process.rs      # ShellProcess: spawn, stop, status, PTY management
  io.rs           # I/O capture: read/write through PTY master fd, data forwarding types
```

**Rationale**: Separates concerns cleanly. `process.rs` owns the lifecycle (fork, PTY setup, signal handling). `io.rs` owns the data flow (reading/writing the master fd, buffering, forwarding hooks).

### 3. Process management approach

**Decision**: Use `nix::pty::openpty` + `nix::unistd::fork` (not `forkpty`) for spawning.

**Rationale**: `openpty` + manual fork gives us explicit control over both the master and slave fds before and after forking. `forkpty` is marked deprecated in newer `nix` versions. After fork, the child sets up the slave as its controlling terminal (via `setsid` + `ioctl`), then execs the shell command.

The parent holds the master fd for reading stdout and writing stdin.

### 4. I/O data model

**Decision**: I/O data is represented as timestamped byte chunks:

```rust
pub struct IoChunk {
    pub timestamp: std::time::Instant,
    pub direction: IoDirection, // Stdin or Stdout
    pub data: Vec<u8>,
}
```

**Rationale**: Raw bytes preserve terminal escape sequences, colors, etc. Timestamps enable replay and ordering. The `IoDirection` enum distinguishes input from output. Consumers (other cells) receive `IoChunk`s and decide how to interpret them.

### 5. Forwarding mechanism

**Decision**: Use a callback-based approach — a `Box<dyn Fn(IoChunk)>` registered on the `ShellProcess`. This is the simplest forwarding mechanism; more sophisticated approaches (channels, streams) can be added later.

**Rationale**: Keeps the initial implementation simple. No async runtime needed. The callback can be set by other cells that want to receive I/O data. A channel-based approach would require choosing between `std::sync::mpsc` and `tokio::sync::mpsc` prematurely.

### 6. Func implementations

**Decision**: Expose four `Func` implementations from the `TtyCell`:

- `SpawnProcess` — start a new shell process, returns a process handle/ID
- `StopProcess` — stop a running process by ID (sends SIGTERM, then SIGKILL after timeout)
- `WriteStdin` — write bytes to a process's stdin
- `ReadStdout` — read available bytes from a process's stdout

**Rationale**: Maps directly to the proposal's required operations. Each is a distinct typed function with clear input/output, fitting the moco `Func` pattern.

## Risks / Trade-offs

- **Blocking I/O on PTY master fd** → Initial implementation uses blocking reads. Mitigation: Use a dedicated reader thread per process; can migrate to async later.
- **Process cleanup on unexpected termination** → If the parent crashes, child processes may become orphans. Mitigation: Set `PR_SET_PDEATHSIG` on Linux; use process groups for cleanup.
- **No Windows support** → Limits portability. Mitigation: Acceptable for v0.1; PTY is inherently Unix. Windows ConPTY support can be added as a separate capability later.
- **Single-threaded callback forwarding** → Callback runs on the reader thread; slow consumers block reading. Mitigation: Document this; move to channels if it becomes a problem.
- **`nix` crate API stability** → `nix` occasionally changes APIs across versions. Mitigation: Pin to `nix 0.31.x`; the PTY API is stable and well-established.
