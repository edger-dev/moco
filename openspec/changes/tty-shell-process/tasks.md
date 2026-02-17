## 1. Dependencies and Module Structure

- [ ] 1.1 Replace `faketty` dependency with `nix = { version = "0.31", features = ["term", "process", "signal"] }` in `cells/moco-tty/Cargo.toml`
- [ ] 1.2 Add `rootcause` workspace dependency to `cells/moco-tty/Cargo.toml` for error handling
- [ ] 1.3 Create module structure: `lib.rs`, `process.rs`, `io.rs` in `cells/moco-tty/src/`
- [ ] 1.4 Remove placeholder `add` function and `it_works` test from `lib.rs`

## 2. I/O Data Types

- [ ] 2.1 Define `IoDirection` enum (`Stdin`, `Stdout`) in `io.rs`
- [ ] 2.2 Define `IoChunk` struct (timestamp, direction, data) in `io.rs`
- [ ] 2.3 Define the forwarding callback type (`Box<dyn Fn(IoChunk) + Send>`) in `io.rs`

## 3. Process Management

- [ ] 3.1 Define `ProcessId` type and `ProcessStatus` enum (Running, Exited with code) in `process.rs`
- [ ] 3.2 Implement `ShellProcess` struct holding PTY master fd, child PID, process state, and I/O callback
- [ ] 3.3 Implement `ShellProcess::spawn` — allocate PTY via `openpty`, fork, set up slave as controlling terminal in child, exec the command
- [ ] 3.4 Implement stdout reader thread that continuously reads from master fd, creates `IoChunk`s, buffers output, and invokes callback if registered
- [ ] 3.5 Implement `ShellProcess::stop` — send SIGTERM, wait with timeout, send SIGKILL if needed, clean up resources
- [ ] 3.6 Implement `ShellProcess::status` — return current `ProcessStatus`
- [ ] 3.7 Implement `ShellProcess::write_stdin` — write bytes to master fd, create stdin `IoChunk`, invoke callback if registered
- [ ] 3.8 Implement `ShellProcess::read_stdout` — return and drain buffered output bytes
- [ ] 3.9 Implement `ShellProcess::set_callback` — register I/O forwarding callback

## 4. Cell and Func Integration

- [ ] 4.1 Define `TtyCell` struct implementing `Cell` trait with `CellSpec` (name: "tty")
- [ ] 4.2 Implement `SpawnProcess` Func with `FuncSpec` (name: "spawn_process")
- [ ] 4.3 Implement `StopProcess` Func with `FuncSpec` (name: "stop_process")
- [ ] 4.4 Implement `ProcessStatus` Func with `FuncSpec` (name: "process_status")
- [ ] 4.5 Implement `WriteStdin` Func with `FuncSpec` (name: "write_stdin")
- [ ] 4.6 Implement `ReadStdout` Func with `FuncSpec` (name: "read_stdout")

## 5. Error Handling

- [ ] 5.1 Define `TtyError` enum using `rootcause` covering: spawn failure, process not found, process not running, I/O error, PTY allocation failure

## 6. Tests

- [ ] 6.1 Test spawning a process and verifying it is running
- [ ] 6.2 Test stopping a process (graceful and forced)
- [ ] 6.3 Test writing to stdin and reading from stdout (round-trip through PTY)
- [ ] 6.4 Test `IoChunk` creation with correct timestamps and directions
- [ ] 6.5 Test callback forwarding receives both stdin and stdout chunks
- [ ] 6.6 Test querying status of unknown process returns error
- [ ] 6.7 Test Cell and Func specs have correct metadata
