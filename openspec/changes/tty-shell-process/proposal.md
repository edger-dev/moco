## Why

The moco-tty crate exists as a placeholder with a `faketty` dependency but no functionality. To be useful, it needs the ability to manage shell processes — starting and stopping them, capturing all stdin/stdout through faketty (so programs behave as if connected to a real terminal), and forwarding that I/O data to other cells in the moco framework. This is foundational for any terminal-based workflow: remote shells, command execution, session recording, or interactive tool integration.

## What Changes

- Implement a shell process manager in `moco-tty` that can spawn and stop shell processes with faketty-based PTY allocation
- Capture stdin/stdout streams from managed processes using faketty, preserving terminal behaviors (colors, line discipline, etc.)
- Define `Cell` and `Func` implementations for `moco-tty` so it integrates with the moco plugin framework
- Expose typed functions for: starting a process, stopping a process, writing to stdin, and reading stdout
- Provide a mechanism to forward captured I/O data to other cells for downstream processing (logging, replay, transformation)

## Capabilities

### New Capabilities

- `shell-process`: Managing shell process lifecycle (spawn, stop, status) with faketty-based PTY allocation for transparent terminal emulation
- `shell-io`: Capturing and forwarding stdin/stdout data streams from managed processes to other cells

### Modified Capabilities

_(none — no existing specs)_

## Impact

- **Code**: `cells/moco-tty/src/` — replace placeholder with full implementation; add new modules for process management and I/O handling
- **Dependencies**: `faketty` (already present), may need `tokio` or similar for async process management, `nix`/`libc` for PTY operations
- **APIs**: New `Cell` and `Func` implementations exposed by `moco-tty`
- **Systems**: Linux/macOS only (PTY is platform-specific); no Windows support initially
