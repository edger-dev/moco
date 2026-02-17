## ADDED Requirements

### Requirement: Spawn a shell process with PTY
The system SHALL spawn a shell process under a pseudoterminal (PTY) allocated via `nix::pty::openpty`. The spawned process MUST have the PTY slave as its controlling terminal, with stdin, stdout, and stderr connected to it. The parent process SHALL retain the PTY master file descriptor for I/O operations.

#### Scenario: Spawn a default shell
- **WHEN** a spawn request is issued without specifying a command
- **THEN** the system spawns the user's default shell (from `$SHELL`, falling back to `/bin/sh`) under a new PTY and returns a unique process identifier

#### Scenario: Spawn a specific command
- **WHEN** a spawn request is issued with a command and arguments (e.g., `["python3", "-i"]`)
- **THEN** the system spawns the specified command under a new PTY with the given arguments and returns a unique process identifier

#### Scenario: PTY terminal behavior preserved
- **WHEN** a process is spawned under the PTY
- **THEN** the process detects a terminal on its stdout (i.e., `isatty(1)` returns true) and terminal-dependent behaviors (colors, line editing) are enabled

### Requirement: Stop a running process
The system SHALL stop a running shell process by sending `SIGTERM`. If the process does not exit within a configurable timeout, the system SHALL send `SIGKILL` to force termination. After termination, all associated resources (PTY file descriptors, reader threads) MUST be cleaned up.

#### Scenario: Graceful stop
- **WHEN** a stop request is issued for a running process
- **THEN** the system sends `SIGTERM` to the process and waits for it to exit, returning the exit status

#### Scenario: Forced stop after timeout
- **WHEN** a stop request is issued and the process does not exit within the timeout period after `SIGTERM`
- **THEN** the system sends `SIGKILL` to force termination and returns the exit status

#### Scenario: Stop an already-exited process
- **WHEN** a stop request is issued for a process that has already exited
- **THEN** the system returns the previously recorded exit status without error

### Requirement: Query process status
The system SHALL provide the current status of a managed process, including whether it is running or exited and its exit code if applicable.

#### Scenario: Query a running process
- **WHEN** a status query is issued for a running process
- **THEN** the system returns a status indicating the process is running

#### Scenario: Query an exited process
- **WHEN** a status query is issued for a process that has exited
- **THEN** the system returns a status indicating the process has exited along with its exit code

#### Scenario: Query an unknown process ID
- **WHEN** a status query is issued with an unknown process identifier
- **THEN** the system returns an error indicating the process was not found

### Requirement: Cell and Func integration
The `moco-tty` crate SHALL implement the `Cell` trait with a `CellSpec` providing name, version, title, and description. It SHALL expose `Func` implementations for `SpawnProcess`, `StopProcess`, and `ProcessStatus`, each with a corresponding `FuncSpec`.

#### Scenario: Cell metadata
- **WHEN** the `TtyCell`'s `CellSpec` is inspected
- **THEN** it provides the name `"tty"`, the current crate version, and a descriptive title and description

#### Scenario: Func metadata
- **WHEN** a `Func`'s `FuncSpec` is inspected (e.g., `SpawnProcess`)
- **THEN** it provides a name (e.g., `"spawn_process"`), title, and description matching the function's purpose
