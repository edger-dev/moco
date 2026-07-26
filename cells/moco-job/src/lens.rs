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
