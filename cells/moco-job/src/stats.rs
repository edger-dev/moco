//! What a job is consuming, and whether anyone said that was too much.
//!
//! This exists to answer the question that motivated a machine-global
//! supervisor in the first place: **what is wedging this machine?** A runaway
//! compiler can freeze a whole box, and before this nothing here had a view of
//! the process set wide enough to point at the culprit.
//!
//! **Monitor-first, and advisory.** Samples are taken, history is kept, a
//! declared threshold being crossed is reported — and nothing is ever throttled
//! or killed. Enforcement is a different feature wearing the same word, and it
//! needs cgroup delegation this does not have. The limit fields exist from the
//! start so that adding enforcement later is purely additive rather than a
//! schema change.
//!
//! implements: resource-limits-report-never-enforce

use facet::Facet;

/// One reading of a job's resource use.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub at_unix_ms: u64,
    /// Percent of one core, averaged over the interval since the previous
    /// sample. Absent a previous sample there is no rate to report, so the
    /// first sample of a job reads zero rather than something invented.
    pub cpu_pct: u32,
    pub rss_bytes: u64,
}

/// What a declaration says is too much.
///
/// Zero means unstated. Both are **advisory**: crossing one is reported, never
/// acted on.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    /// Percent of one core; 400 means four cores' worth.
    #[facet(default)]
    pub cpu_pct: u32,
    #[facet(default)]
    pub mem_mb: u64,
}

impl Limits {
    pub fn is_unset(&self) -> bool {
        self.cpu_pct == 0 && self.mem_mb == 0
    }

    /// Which declared limits this sample exceeds.
    pub fn breached_by(&self, sample: &Sample) -> Breach {
        Breach {
            cpu: self.cpu_pct > 0 && sample.cpu_pct > self.cpu_pct,
            memory: self.mem_mb > 0 && sample.rss_bytes > self.mem_mb * 1024 * 1024,
        }
    }
}

/// Which limits a sample crossed. Reported; never acted on.
#[derive(Facet, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breach {
    pub cpu: bool,
    pub memory: bool,
}

impl Breach {
    pub fn any(&self) -> bool {
        self.cpu || self.memory
    }
}

/// A job's recent resource history.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Oldest first. Short on purpose — this answers "what is happening now",
    /// and a supervisor is not a metrics store.
    pub samples: Vec<Sample>,
    pub limits: Limits,
    /// Whether the most recent sample crosses a declared limit.
    pub breach: Breach,
}
