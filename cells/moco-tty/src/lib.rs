pub mod error;
pub mod io;
pub mod process;

use moco_core::{Cell, CellSpec, Func, FuncSpec};

pub struct TtyCell;

impl Cell for TtyCell {
    const SPEC: &'static CellSpec = &CellSpec {
        name: "tty",
        version: "0.1.0-dev",
        title: "TTY Cell",
        description: "TTY/terminal cell providing shell process management via PTY",
    };
}

pub struct SpawnProcess;

impl Func for SpawnProcess {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "spawn_process",
        title: "Spawn Process",
        description: "Spawn a new shell process with a PTY",
    };
}

pub struct StopProcess;

impl Func for StopProcess {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "stop_process",
        title: "Stop Process",
        description: "Stop a running shell process (SIGTERM then SIGKILL)",
    };
}

pub struct QueryProcessStatus;

impl Func for QueryProcessStatus {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "process_status",
        title: "Process Status",
        description: "Query the status of a shell process",
    };
}

pub struct WriteStdin;

impl Func for WriteStdin {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "write_stdin",
        title: "Write Stdin",
        description: "Write bytes to the stdin of a shell process",
    };
}

pub struct ReadStdout;

impl Func for ReadStdout {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "read_stdout",
        title: "Read Stdout",
        description: "Read buffered stdout output from a shell process",
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{IoChunk, IoDirection};
    use crate::process::{ProcessStatus, ShellProcess};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn test_spawn_and_running() {
        let mut proc = ShellProcess::spawn("sh", &["-c", "sleep 10"]).expect("spawn failed");
        assert_eq!(proc.status(), ProcessStatus::Running);
        proc.stop().expect("stop failed");
    }

    #[test]
    fn test_stop_graceful() {
        let mut proc = ShellProcess::spawn("sh", &["-c", "sleep 10"]).expect("spawn failed");
        let status = proc.stop().expect("stop failed");
        match status {
            ProcessStatus::Exited(_) => {}
            _ => panic!("expected Exited after stop"),
        }
    }

    #[test]
    fn test_stop_forced() {
        // trap SIGTERM to ignore it, forcing SIGKILL path
        let mut proc = ShellProcess::spawn("sh", &["-c", "trap '' TERM; sleep 60"])
            .expect("spawn failed");
        let status = proc.stop().expect("stop failed");
        match status {
            ProcessStatus::Exited(_) => {}
            _ => panic!("expected Exited after forced stop"),
        }
    }

    #[test]
    fn test_stdin_stdout_roundtrip() {
        let proc = ShellProcess::spawn("cat", &[]).expect("spawn failed");

        // Give cat a moment to start
        std::thread::sleep(Duration::from_millis(200));

        proc.write_stdin(b"hello\n").expect("write failed");

        // Wait for output to be read
        std::thread::sleep(Duration::from_millis(500));

        let output = proc.read_stdout();
        // PTY echoes input back, so we should see "hello" in the output
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("hello"),
            "expected 'hello' in output, got: {output_str:?}"
        );

        let mut proc = proc;
        proc.stop().expect("stop failed");
    }

    #[test]
    fn test_io_chunk_creation() {
        let chunk = IoChunk::new(IoDirection::Stdin, b"test data".to_vec());
        assert_eq!(chunk.direction, IoDirection::Stdin);
        assert_eq!(chunk.data, b"test data");

        let chunk2 = IoChunk::new(IoDirection::Stdout, b"output".to_vec());
        assert_eq!(chunk2.direction, IoDirection::Stdout);
        assert_eq!(chunk2.data, b"output");

        // Timestamps should be monotonically increasing
        assert!(chunk2.timestamp >= chunk.timestamp);
    }

    #[test]
    fn test_callback_forwarding() {
        let chunks: Arc<Mutex<Vec<(IoDirection, Vec<u8>)>>> = Arc::new(Mutex::new(Vec::new()));
        let chunks_clone = Arc::clone(&chunks);

        let proc = ShellProcess::spawn("cat", &[]).expect("spawn failed");

        proc.set_callback(Box::new(move |chunk: IoChunk| {
            let mut c = chunks_clone.lock().unwrap();
            c.push((chunk.direction, chunk.data));
        }));

        std::thread::sleep(Duration::from_millis(200));

        // Write triggers stdin chunk via write_stdin, and cat echoes back triggering stdout chunk
        proc.write_stdin(b"ping\n").expect("write failed");

        std::thread::sleep(Duration::from_millis(500));

        let captured = chunks.lock().unwrap();

        // We should have at least a stdin chunk and a stdout chunk
        let has_stdin = captured.iter().any(|(d, _)| *d == IoDirection::Stdin);
        let has_stdout = captured.iter().any(|(d, _)| *d == IoDirection::Stdout);
        assert!(has_stdin, "expected stdin chunk in callback");
        assert!(has_stdout, "expected stdout chunk in callback");

        let mut proc = proc;
        proc.stop().expect("stop failed");
    }

    #[test]
    fn test_read_stdout_drains_buffer() {
        let proc = ShellProcess::spawn("sh", &["-c", "echo hello"]).expect("spawn failed");
        std::thread::sleep(Duration::from_millis(500));

        let first = proc.read_stdout();
        assert!(!first.is_empty(), "first read should have data");

        let second = proc.read_stdout();
        assert!(second.is_empty(), "second read should be empty (drained)");

        let mut proc = proc;
        proc.stop().ok();
    }

    #[test]
    fn test_cell_spec() {
        assert_eq!(TtyCell::SPEC.name, "tty");
        assert_eq!(TtyCell::SPEC.version, "0.1.0-dev");
        assert_eq!(TtyCell::SPEC.title, "TTY Cell");
    }

    #[test]
    fn test_func_specs() {
        assert_eq!(SpawnProcess::SPEC.name, "spawn_process");
        assert_eq!(StopProcess::SPEC.name, "stop_process");
        assert_eq!(QueryProcessStatus::SPEC.name, "process_status");
        assert_eq!(WriteStdin::SPEC.name, "write_stdin");
        assert_eq!(ReadStdout::SPEC.name, "read_stdout");
    }
}
