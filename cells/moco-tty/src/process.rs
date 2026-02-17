use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::thread;

use nix::pty::openpty;
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{ForkResult, Pid, close, execvp, fork, setsid};

use crate::error::TtyError;
use crate::io::{IoCallback, IoChunk, IoDirection};

pub type ProcessId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Exited(i32),
}

pub struct ShellProcess {
    master_fd: OwnedFd,
    child_pid: Pid,
    status: Arc<Mutex<ProcessStatus>>,
    output_buffer: Arc<Mutex<Vec<u8>>>,
    callback: Arc<Mutex<Option<IoCallback>>>,
}

impl ShellProcess {
    pub fn spawn(command: &str, args: &[&str]) -> Result<Self, TtyError> {
        let pty = openpty(None, None).map_err(TtyError::PtyAllocation)?;

        let master_fd = pty.master;
        let slave_fd = pty.slave;

        // Safety: we immediately exec or _exit in the child
        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                // Close master in child
                drop(master_fd);

                // Create new session and set controlling terminal
                setsid().ok();

                let slave_raw = slave_fd.as_raw_fd();

                // Set up slave as stdin/stdout/stderr using libc dup2
                unsafe {
                    libc::dup2(slave_raw, 0);
                    libc::dup2(slave_raw, 1);
                    libc::dup2(slave_raw, 2);
                }

                // Close original slave fd if it's not 0, 1, or 2
                if slave_raw > 2 {
                    close(slave_raw).ok();
                }

                let c_command =
                    CString::new(command).unwrap_or_else(|_| CString::new("sh").unwrap());
                let mut c_args: Vec<CString> = vec![c_command.clone()];
                for arg in args {
                    if let Ok(a) = CString::new(*arg) {
                        c_args.push(a);
                    }
                }

                // exec replaces the process; if it fails we exit
                let _ = execvp(&c_command, &c_args);
                unsafe { libc::_exit(127) };
            }
            Ok(ForkResult::Parent { child }) => {
                // Close slave in parent
                drop(slave_fd);

                let status = Arc::new(Mutex::new(ProcessStatus::Running));
                let output_buffer = Arc::new(Mutex::new(Vec::new()));
                let callback: Arc<Mutex<Option<IoCallback>>> = Arc::new(Mutex::new(None));

                // Duplicate master fd for reader thread
                let reader_fd =
                    unsafe { OwnedFd::from_raw_fd(libc::dup(master_fd.as_raw_fd())) };

                let status_clone = Arc::clone(&status);
                let buffer_clone = Arc::clone(&output_buffer);
                let callback_clone = Arc::clone(&callback);
                let child_pid = child;

                thread::spawn(move || {
                    let mut file =
                        unsafe { std::fs::File::from_raw_fd(reader_fd.into_raw_fd()) };
                    let mut buf = [0u8; 4096];

                    loop {
                        match file.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let data = buf[..n].to_vec();

                                {
                                    let mut output = buffer_clone.lock().unwrap();
                                    output.extend_from_slice(&data);
                                }

                                let chunk = IoChunk::new(IoDirection::Stdout, data);
                                if let Ok(cb) = callback_clone.lock() {
                                    if let Some(ref f) = *cb {
                                        f(chunk);
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    // Reader ended — check child status
                    if let Ok(wait_result) =
                        waitpid(child_pid, Some(WaitPidFlag::WNOHANG))
                    {
                        if let nix::sys::wait::WaitStatus::Exited(_, code) = wait_result {
                            let mut s = status_clone.lock().unwrap();
                            *s = ProcessStatus::Exited(code);
                        }
                    }
                });

                Ok(ShellProcess {
                    master_fd,
                    child_pid: child,
                    status,
                    output_buffer,
                    callback,
                })
            }
            Err(e) => Err(TtyError::SpawnFailure(e.to_string())),
        }
    }

    pub fn pid(&self) -> ProcessId {
        self.child_pid.as_raw() as ProcessId
    }

    pub fn status(&self) -> ProcessStatus {
        // Try non-blocking waitpid to update status
        if let Ok(wait_result) = waitpid(self.child_pid, Some(WaitPidFlag::WNOHANG)) {
            if let nix::sys::wait::WaitStatus::Exited(_, code) = wait_result {
                let mut s = self.status.lock().unwrap();
                *s = ProcessStatus::Exited(code);
            }
        }
        *self.status.lock().unwrap()
    }

    pub fn write_stdin(&self, data: &[u8]) -> Result<(), TtyError> {
        let dup_fd = unsafe { libc::dup(self.master_fd.as_raw_fd()) };
        let mut file = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        file.write_all(data).map_err(TtyError::Io)?;
        file.flush().map_err(TtyError::Io)?;

        let chunk = IoChunk::new(IoDirection::Stdin, data.to_vec());
        if let Ok(cb) = self.callback.lock() {
            if let Some(ref f) = *cb {
                f(chunk);
            }
        }

        Ok(())
    }

    pub fn read_stdout(&self) -> Vec<u8> {
        let mut buffer = self.output_buffer.lock().unwrap();
        let data = buffer.clone();
        buffer.clear();
        data
    }

    pub fn set_callback(&self, callback: IoCallback) {
        let mut cb = self.callback.lock().unwrap();
        *cb = Some(callback);
    }

    pub fn stop(&mut self) -> Result<ProcessStatus, TtyError> {
        // Check if already exited
        if let ProcessStatus::Exited(code) = self.status() {
            return Ok(ProcessStatus::Exited(code));
        }

        // Send SIGTERM
        let _ = signal::kill(self.child_pid, Signal::SIGTERM);

        // Wait up to 2 seconds for graceful exit
        for _ in 0..20 {
            thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(wait_result) = waitpid(self.child_pid, Some(WaitPidFlag::WNOHANG)) {
                match wait_result {
                    nix::sys::wait::WaitStatus::Exited(_, code) => {
                        let mut s = self.status.lock().unwrap();
                        *s = ProcessStatus::Exited(code);
                        return Ok(ProcessStatus::Exited(code));
                    }
                    nix::sys::wait::WaitStatus::Signaled(_, _, _) => {
                        let mut s = self.status.lock().unwrap();
                        *s = ProcessStatus::Exited(-1);
                        return Ok(ProcessStatus::Exited(-1));
                    }
                    _ => {}
                }
            }
        }

        // Force kill
        let _ = signal::kill(self.child_pid, Signal::SIGKILL);

        // Wait for process to be reaped
        match waitpid(self.child_pid, None) {
            Ok(nix::sys::wait::WaitStatus::Exited(_, code)) => {
                let mut s = self.status.lock().unwrap();
                *s = ProcessStatus::Exited(code);
                Ok(ProcessStatus::Exited(code))
            }
            _ => {
                let mut s = self.status.lock().unwrap();
                *s = ProcessStatus::Exited(-1);
                Ok(ProcessStatus::Exited(-1))
            }
        }
    }
}
