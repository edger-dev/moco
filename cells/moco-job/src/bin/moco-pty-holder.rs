//! The per-job PTY holder.
//!
//! Spawned by the supervisor for a `terminal`-lens job; owns that job's pty and
//! its screen, and outlives the supervisor. Packaging installs it alongside the
//! node daemon — the daemon finds it by path.
//!
//! implements: pty-holder-owns-the-terminal

fn main() -> std::process::ExitCode {
    let config = match moco_job::holder::config_from_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("moco-pty-holder: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    match moco_job::holder::run(config) {
        // Exit with the *job's* code, so the daemon reads the job's outcome
        // rather than the holder's.
        Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("moco-pty-holder: {e}");
            std::process::ExitCode::from(1)
        }
    }
}
