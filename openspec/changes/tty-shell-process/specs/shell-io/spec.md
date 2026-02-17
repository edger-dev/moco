## ADDED Requirements

### Requirement: Write data to process stdin
The system SHALL accept byte data and write it to a managed process's stdin via the PTY master file descriptor. The write MUST be atomic from the caller's perspective — either all bytes are written or an error is returned.

#### Scenario: Write input to a running process
- **WHEN** a write-stdin request is issued with byte data for a running process
- **THEN** the system writes the data to the PTY master fd and the process receives it as stdin input

#### Scenario: Write to a stopped process
- **WHEN** a write-stdin request is issued for a process that has already exited
- **THEN** the system returns an error indicating the process is not running

### Requirement: Read data from process stdout
The system SHALL read byte data from a managed process's stdout via the PTY master file descriptor. Reads MUST return raw bytes including any terminal escape sequences. A dedicated reader thread SHALL continuously read from the master fd and buffer the output.

#### Scenario: Read output from a running process
- **WHEN** a read-stdout request is issued for a running process that has produced output
- **THEN** the system returns the buffered output bytes, including any ANSI escape sequences

#### Scenario: Read when no output available
- **WHEN** a read-stdout request is issued and no new output is available
- **THEN** the system returns an empty byte sequence (non-blocking)

#### Scenario: Read from a stopped process
- **WHEN** a read-stdout request is issued for a process that has exited
- **THEN** the system returns any remaining buffered output, or an empty sequence if already drained

### Requirement: Timestamped I/O chunks
All captured I/O data SHALL be represented as `IoChunk` structs containing a timestamp, a direction indicator (stdin or stdout), and the raw byte data. This enables downstream consumers to reconstruct the I/O timeline.

#### Scenario: Stdout chunk creation
- **WHEN** data is read from the PTY master fd
- **THEN** an `IoChunk` is created with the current timestamp, direction set to `Stdout`, and the raw bytes

#### Scenario: Stdin chunk creation
- **WHEN** data is written to the PTY master fd
- **THEN** an `IoChunk` is created with the current timestamp, direction set to `Stdin`, and the raw bytes

### Requirement: I/O forwarding via callback
The system SHALL support registering a callback function on a managed process. When registered, every `IoChunk` (both stdin and stdout) SHALL be passed to the callback. This enables other cells to receive real-time I/O data for logging, replay, or transformation.

#### Scenario: Register a forwarding callback
- **WHEN** a callback is registered on a running process
- **THEN** all subsequent `IoChunk`s are passed to the callback in addition to normal buffering

#### Scenario: No callback registered
- **WHEN** no callback has been registered on a process
- **THEN** I/O data is buffered normally and no forwarding occurs

#### Scenario: Callback receives both directions
- **WHEN** a callback is registered and data flows through both stdin and stdout
- **THEN** the callback receives `IoChunk`s for both directions with correct timestamps and direction indicators

### Requirement: Func integration for I/O
The `moco-tty` crate SHALL expose `Func` implementations for `WriteStdin` and `ReadStdout`, each with a corresponding `FuncSpec`. These functions operate on a process identified by its process ID.

#### Scenario: WriteStdin Func metadata
- **WHEN** the `WriteStdin` `FuncSpec` is inspected
- **THEN** it provides the name `"write_stdin"`, title, and description

#### Scenario: ReadStdout Func metadata
- **WHEN** the `ReadStdout` `FuncSpec` is inspected
- **THEN** it provides the name `"read_stdout"`, title, and description
