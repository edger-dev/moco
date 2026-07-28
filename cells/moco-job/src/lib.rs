//! moco-job — the agent-first job substrate.
//!
//! Every consequential action is an addressable, observable, controllable,
//! durable **job** — never a blocking RPC that hides its state until it returns.
//! v1 realizes this single-node and in-process, as a moco cell riding (later) on
//! the hub for transport.
//!
//! implements: job-substrate-is-a-moco-cell-layer

pub mod admission;
pub mod audit;
pub mod error;
pub mod job;
pub mod lens;
pub mod lifecycle;
pub mod manifest;
pub mod port;
pub mod preflight;
pub mod procfs;
pub mod record;
pub mod registry;
pub mod rules;
pub mod scope;
pub mod stats;
pub mod wire;

pub use admission::WorktreePolicy;
pub use audit::{AuditRecord, AuditSink, FileAuditLog, MemoryAuditLog, Verdict};
pub use error::JobError;
pub use job::{DeniedReason, JobId, JobRequest, JobStatus, Outcome, Tail};
pub use lens::{HumanView, LensSource, MachineRead};
pub use lens::{ScreenRead, ScreenSource};
pub use lifecycle::{Autostart, Lifetime, RestartPolicy};
pub use manifest::{MANIFEST_FILE, Manifest, ProcEntry};
pub use port::{PortRange, PortRequest};
pub use preflight::Preflight;
pub use procfs::{Liveness, liveness};
pub use record::{JobRecord, RecordStore};
pub use registry::JobRegistry;
pub use registry::{SAMPLE_HISTORY, SCREEN_COLS, SCREEN_ROWS};
pub use rules::{Decision, Disposition, NodePolicy, RuleSet, SeedConfig};
pub use scope::{Caller, Scope};
pub use stats::{Breach, Limits, Sample, Stats};
pub use wire::{METHODS, WireError, dispatch};

use moco_core::{Cell, CellSpec, Func, FuncSpec};

/// The job cell: exposes the job surface as moco functions.
pub struct JobCell;

impl Cell for JobCell {
    const SPEC: &'static CellSpec = &CellSpec {
        name: "job",
        version: "0.1.0-dev",
        title: "Job Cell",
        description: "Agent-first job substrate: run commands as observable, controllable, durable jobs",
    };
}

pub struct StartJob;

impl Func for StartJob {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "start",
        title: "Start Job",
        description: "Spawn a command as a detached, file-backed job and return its id",
    };
}

pub struct TailJob;

impl Func for TailJob {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "tail",
        title: "Tail Job",
        description: "Read a job's output incrementally, with its live status",
    };
}

pub struct WaitJob;

impl Func for WaitJob {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "wait",
        title: "Wait for Job",
        description: "Block until a job is terminal and return its outcome (resumable by id)",
    };
}

pub struct KillJob;

impl Func for KillJob {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "kill",
        title: "Kill Job",
        description: "Terminate a running job",
    };
}

pub struct RunJob;

impl Func for RunJob {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "run",
        title: "Run Job",
        description: "Blocking sugar over start + wait, for quick commands",
    };
}

pub struct PreflightJob;

impl Func for PreflightJob {
    const SPEC: &'static FuncSpec = &FuncSpec {
        name: "preflight",
        title: "Preflight Command",
        description: "Read-only: resolve the disposition, program, PATH and confined cwd a run would get",
    };
}

// `decide` is deliberately NOT declared as a cell Func here. It is the human
// console's capability, not the requesting agent's: listing it beside StartJob
// would advertise the approval gate on the very surface the gate exists to
// constrain. It stays an in-process method until the console slice gives it an
// operator principal.
//
// implements: intervention-is-the-only-human-write
