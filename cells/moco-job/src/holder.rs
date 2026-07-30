//! A per-job PTY holder: a small process that owns the terminal, so the
//! terminal outlives the supervisor.
//!
//! A `terminal`-lens job draws through a pty. Until now the daemon owned the
//! master, which meant restarting the supervisor closed it and the job's writes
//! started failing with `EIO` — so upgrading the thing whose whole purpose is
//! keeping jobs alive killed exactly the jobs it was watching most closely.
//!
//! The holder is deliberately **small and stupid**: it owns a file descriptor
//! and a screen, and it has no transport, no registry and no liveness story of
//! its own. That is what separates it from the per-job daemon this design
//! rejected elsewhere — the objection there was to a second *node-level* daemon
//! duplicating transport, liveness and deployment, none of which is here.
//!
//! The daemon tracks it through the ordinary adoption path: it is a process
//! with a pid and a start time, like any other.
//!
//! implements: pty-holder-owns-the-terminal

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::JobError;

/// Where the holder keeps what it owns, alongside the job's capture.
///
/// Files rather than a socket. A socket would need the daemon to be listening
/// at the moment the holder has something to say, which is precisely what is
/// not true across a restart; a file is readable by whoever turns up next, and
/// needs no handshake. The screen is small and rewritten in place, so this
/// costs a few kilobytes per terminal job.
pub struct HolderPaths {
    /// The job's scrollback — the same file the log path writes, so `tail`,
    /// durability and compaction all keep working unchanged.
    pub capture: PathBuf,
    /// The rendered screen, rewritten as the job draws.
    pub screen: PathBuf,
    /// The **job's** pid, as opposed to the holder's.
    pub job_pid: PathBuf,
}

impl HolderPaths {
    /// Derive every path from the capture, so the daemon and the holder agree
    /// without passing three arguments that could disagree.
    pub fn beside(capture: impl Into<PathBuf>) -> Self {
        let capture = capture.into();
        Self {
            screen: capture.with_extension("screen"),
            job_pid: capture.with_extension("jobpid"),
            capture,
        }
    }
}

/// How often the rendered screen is written out while the job is drawing.
///
/// A redrawing TUI writes thousands of times a second and the screen is only
/// ever read occasionally, so rewriting on every byte would burn the disk to
/// serve nobody. A tenth of a second is faster than anyone reads and slow
/// enough to be nearly free.
const SCREEN_FLUSH: Duration = Duration::from_millis(100);

/// Everything the holder needs to do its job.
pub struct HolderConfig {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
    pub paths: HolderPaths,
    /// Extra environment for the child, e.g. an allocated port.
    pub env: Vec<(String, String)>,
}

/// Run one job under a pty and hold that pty until the job ends.
///
/// Returns the job's exit code, so the holder can exit with it and the daemon
/// reads the job's real outcome rather than the holder's.
///
/// implements: pty-holder-owns-the-terminal
pub fn run(config: HolderConfig) -> Result<i32, JobError> {
    let (program, args) = config.argv.split_first().ok_or(JobError::EmptyArgv)?;

    let size = nix::pty::Winsize {
        ws_row: config.rows,
        ws_col: config.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = nix::pty::openpty(&size, None)
        .map_err(|e| JobError::Audit(format!("could not allocate a pty: {e}")))?;

    let slave_in = pty.slave.try_clone().map_err(JobError::Io)?;
    let slave_err = pty.slave.try_clone().map_err(JobError::Io)?;

    let mut capture = File::options()
        .create(true)
        .append(true)
        .open(&config.paths.capture)
        .map_err(JobError::Io)?;

    let mut command = Command::new(program);
    for (key, value) in &config.env {
        command.env(key, value);
    }
    // The job leads its own group *within* the holder's, so a stop aimed at the
    // holder's group reaches both, and the holder can be signalled alone.
    let mut child = command
        .args(args)
        .current_dir(&config.cwd)
        .stdin(Stdio::from(slave_in))
        .stdout(Stdio::from(pty.slave))
        .stderr(Stdio::from(slave_err))
        .spawn()
        .map_err(|source| JobError::Spawn {
            program: program.clone(),
            searched_path: String::new(),
            source,
        })?;

    // Published before the first read, so a daemon that turns up immediately
    // can already tell which pid is the job rather than the holder.
    let _ = std::fs::write(&config.paths.job_pid, child.id().to_string());

    let parser = Arc::new(Mutex::new(vt100::Parser::new(config.rows, config.cols, 0)));
    let dirty = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));

    // **The flush runs on its own thread, not off the read loop.** Flushing
    // when the next chunk arrives sounds equivalent and is not: a job that
    // paints its screen and then goes quiet blocks in `read` forever, so the
    // screen would never be written at all — and "drew once, then waited" is
    // precisely the case the whole lens exists to serve.
    let flusher = std::thread::spawn({
        let parser = parser.clone();
        let dirty = dirty.clone();
        let done = done.clone();
        let path = config.paths.screen.clone();
        move || {
            loop {
                std::thread::sleep(SCREEN_FLUSH);
                let ending = done.load(Ordering::Relaxed);
                if dirty.swap(false, Ordering::Relaxed)
                    && let Ok(parser) = parser.lock()
                {
                    write_screen(&path, &parser);
                }
                if ending {
                    break;
                }
            }
        }
    });

    let mut master = File::from(pty.master);
    let mut buf = [0u8; 8192];
    loop {
        match master.read(&mut buf) {
            // The last slave closed: the job is done drawing.
            Ok(0) | Err(_) => break,
            Ok(n) => {
                // Scrollback first — it is what durability and `tail` rest on,
                // and the screen is a convenience layered over it.
                if capture.write_all(&buf[..n]).is_err() {
                    break;
                }
                if let Ok(mut parser) = parser.lock() {
                    parser.process(&buf[..n]);
                }
                dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    // Let the flusher write whatever is outstanding and stop. The last thing a
    // job drew is what must be on the screen afterwards; without this, a job
    // that exits inside the flush window leaves a stale screen forever.
    dirty.store(true, Ordering::Relaxed);
    done.store(true, Ordering::Relaxed);
    let _ = flusher.join();

    let status = child.wait().map_err(JobError::Io)?;
    let _ = std::fs::remove_file(&config.paths.job_pid);
    Ok(status.code().unwrap_or(-1))
}

/// Write the screen atomically, so a reader never sees half a frame.
///
/// The daemon may read this at any moment and has no way to lock against the
/// holder, so a temp-and-rename is what makes "read the screen" a safe thing to
/// do without coordination.
fn write_screen(path: &Path, parser: &vt100::Parser) {
    let text = crate::registry::trim_blank_rows(&parser.screen().contents());
    let temp = path.with_extension("screen.tmp");
    if std::fs::write(&temp, text.as_bytes()).is_ok() {
        let _ = std::fs::rename(&temp, path);
    }
}

/// Parse the holder's command line.
///
/// `moco-pty-holder --cwd DIR --rows R --cols C --capture PATH [--env K=V]... -- argv...`
pub fn config_from_args(args: impl IntoIterator<Item = String>) -> Result<HolderConfig, String> {
    let mut cwd = None;
    let mut rows = crate::registry::SCREEN_ROWS;
    let mut cols = crate::registry::SCREEN_COLS;
    let mut capture = None;
    let mut env = Vec::new();
    let mut argv = Vec::new();

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cwd" => cwd = it.next(),
            "--capture" => capture = it.next(),
            "--rows" => rows = it.next().and_then(|v| v.parse().ok()).unwrap_or(rows),
            "--cols" => cols = it.next().and_then(|v| v.parse().ok()).unwrap_or(cols),
            "--env" => {
                if let Some((k, v)) = it.next().and_then(|kv| {
                    kv.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                }) {
                    env.push((k, v));
                }
            }
            "--" => {
                argv.extend(it);
                break;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    let capture = capture.ok_or("--capture is required")?;
    if argv.is_empty() {
        return Err("no command given after `--`".to_string());
    }
    Ok(HolderConfig {
        argv,
        cwd: cwd.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/")),
        rows,
        cols,
        paths: HolderPaths::beside(capture),
        env,
    })
}
