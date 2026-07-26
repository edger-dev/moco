//! Two views of one job, and which one a read came from.
//!
//! A supervised process has two audiences. A human wants to see what it drew; an
//! agent wants to know whether it found errors. Serving both from the same raw
//! byte stream serves neither: the stream is enormous, ANSI-laden and mostly
//! redraw, and an agent that must read tens of thousands of tokens to answer
//! "did it compile?" learns to stop asking.
//!
//! **The machine lens is what makes an agent read cheap**, and cheapness is not
//! an optimization here — it decides whether the supervisor gets used at all.
//!
//! It is **declared, never inferred**. Auto-structuring arbitrary output is a
//! research project; a declared sidecar plus a format name is three lines of
//! config and is always right.
//!
//! implements: dual-lens-human-and-machine

use facet::Facet;

/// Which channel a read actually came from.
///
/// Always reported, because the fallback would otherwise be a lie: handing back
/// scrollback labelled as though it were structured is worse than handing back
/// nothing, since a caller would parse it.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LensSource {
    /// The declared sidecar.
    Machine,
    /// The job's own output, because no machine lens was declared.
    Scrollback,
}

/// One read through the machine lens.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct MachineRead {
    /// Where these bytes came from.
    pub source: LensSource,
    /// The declared format, so a caller knows how to read them. Empty when the
    /// source is scrollback — there is no declared format to report, and
    /// inventing one would invite parsing that cannot work.
    pub format: String,
    pub bytes: Vec<u8>,
    /// Resume here to read only what is new.
    pub next_offset: u64,
}

/// Which view a human gets of this job.
///
/// A job runs under a **pty** when this is `Terminal`, so `isatty` is true on
/// its stdio. That is not cosmetic: many tools change what they emit the moment
/// they detect a pipe — colour, progress, interactive redraw — so a checker
/// watched through a pipe is not the same program as one watched through a
/// terminal.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum HumanView {
    /// An append-only stream. Its stdio is an ordinary file, so tools that
    /// check for a terminal will take their non-interactive path.
    #[default]
    Logs,
    /// Pty-backed, so the job draws as it would for a person.
    ///
    /// **The pty is daemon-owned and does not survive a daemon restart** — a
    /// deliberate boundary, not an oversight. The job's record, id, scrollback
    /// and ownership all do; only the live terminal goes. Making it durable is a
    /// per-job holder, decided and deferred.
    ///
    /// The job gets a pty on its stdio but **not a controlling terminal**: no
    /// `setsid` + `TIOCSCTTY`, so `isatty` is true and redraw works, while job
    /// control and signalling a foreground process group do not. Enough for the
    /// lens; short of a real login session, and said out loud rather than
    /// discovered.
    Terminal,
}
