//! The job substrate's remote surface: bytes in, bytes out.
//!
//! This is what a daemon exposes when it offers the job substrate over a
//! transport — and deliberately **nothing more than bytes**. The engine names no
//! transport type and links no transport crate, so it can be hosted by whatever
//! daemon already exists rather than requiring one of its own. The adapter that
//! registers this with a particular daemon lives with that daemon's assembly,
//! not here.
//!
//! Owning both sides of the encoding is also what makes a version mismatch
//! between components a non-event: only bytes cross the boundary, never a
//! `Facet`-derived type.
//!
//! implements: job-connector-is-engine-owned-byte-dispatch
//! implements: job-substrate-is-a-moco-cell-layer

use std::path::PathBuf;
use std::time::Duration;

use facet::Facet;

use crate::error::JobError;
use crate::job::{JobId, JobRequest, JobStatus};
use crate::registry::JobRegistry;

/// Start a job.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub argv: Vec<String>,
    pub cwd: String,
    /// Execution deadline in milliseconds; 0 means unbounded.
    pub deadline_ms: u64,
}

/// The id a started job was given.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct StartReply {
    pub id: String,
}

/// Read a job's output from an offset.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct TailRequest {
    pub id: String,
    pub offset: u64,
}

/// Output plus the job's live status.
///
/// `bytes` is carried verbatim rather than as text: scrollback is whatever the
/// process wrote, and lossily converting it here would corrupt exactly the
/// output someone is reading to debug. (It costs roughly four characters per
/// byte in this encoding — a real cost, and a purely local one to change, since
/// both sides of this codec are the engine's.)
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct TailReply {
    pub bytes: Vec<u8>,
    pub next_offset: u64,
    pub status: JobStatus,
}

/// Wait for a job to reach a terminal state.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct WaitRequest {
    pub id: String,
}

/// A job's terminal record, echoing where it ran and under what authority.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct WaitReply {
    pub status: JobStatus,
    pub verdict: crate::audit::Verdict,
    pub resolved_cwd: String,
}

/// Terminate a job.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct KillRequest {
    pub id: String,
}

/// Acknowledgement that a kill was requested.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct KillReply {
    pub id: String,
}

/// One job in a listing.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
    pub id: String,
    pub status: JobStatus,
}

/// Every job this registry knows about.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct ListReply {
    pub jobs: Vec<JobSummary>,
}

/// Why a dispatched call could not be answered.
#[derive(Debug)]
pub enum WireError {
    /// No such method on this surface.
    UnknownMethod(String),
    /// The request bytes could not be read as this method's request type.
    ///
    /// Distinct from a job failure on purpose: a request nobody can read has
    /// not been attempted, and reporting it as a failed job would blame the
    /// wrong thing.
    Request { method: String, detail: String },
    /// The reply could not be encoded — an engine-side fault, not the caller's.
    Reply { method: String, detail: String },
    /// The engine ran the call and it failed.
    Job(JobError),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::UnknownMethod(method) => write!(
                f,
                "unknown method `{method}` on the job connector; \
                 known methods are: start, tail, wait, kill, list"
            ),
            WireError::Request { method, detail } => {
                write!(f, "could not decode the request for `{method}`: {detail}")
            }
            WireError::Reply { method, detail } => {
                write!(f, "could not encode the reply for `{method}`: {detail}")
            }
            WireError::Job(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<JobError> for WireError {
    fn from(e: JobError) -> Self {
        WireError::Job(e)
    }
}

/// Encode a wire value to bytes.
pub fn encode<T: Facet<'static>>(value: &T) -> Result<Vec<u8>, String> {
    facet_styx::to_string_compact(value)
        .map(String::into_bytes)
        .map_err(|e| e.to_string())
}

/// Decode a wire value from bytes.
///
/// Records are braced *expressions*, not document roots, so they are read with
/// `from_str_expr`.
pub fn decode<T: Facet<'static>>(bytes: &[u8]) -> Result<T, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    facet_styx::from_str_expr::<T>(text.trim()).map_err(|e| e.to_string())
}

fn read<T: Facet<'static>>(method: &str, bytes: &[u8]) -> Result<T, WireError> {
    decode(bytes).map_err(|detail| WireError::Request {
        method: method.to_string(),
        detail,
    })
}

fn write<T: Facet<'static>>(method: &str, value: &T) -> Result<Vec<u8>, WireError> {
    encode(value).map_err(|detail| WireError::Reply {
        method: method.to_string(),
        detail,
    })
}

/// Dispatch one call against a registry.
///
/// An unrecognised method is **refused, never ignored**: a caller must never be
/// able to believe work happened when it did not.
pub fn dispatch(
    registry: &JobRegistry,
    method: &str,
    request: &[u8],
) -> Result<Vec<u8>, WireError> {
    match method {
        "start" => {
            let req: StartRequest = read(method, request)?;
            let mut job = JobRequest::new(req.argv, PathBuf::from(req.cwd));
            if req.deadline_ms > 0 {
                job = job.with_deadline(Duration::from_millis(req.deadline_ms));
            }
            let id = registry.start(job)?;
            write(method, &StartReply { id: id.0 })
        }
        "tail" => {
            let req: TailRequest = read(method, request)?;
            let tail = registry.tail(&JobId(req.id), req.offset)?;
            write(
                method,
                &TailReply {
                    bytes: tail.bytes,
                    next_offset: tail.next_offset,
                    status: tail.status,
                },
            )
        }
        "wait" => {
            let req: WaitRequest = read(method, request)?;
            let outcome = registry.wait(&JobId(req.id))?;
            write(
                method,
                &WaitReply {
                    status: outcome.status,
                    verdict: outcome.verdict,
                    resolved_cwd: outcome.resolved_cwd.to_string_lossy().into_owned(),
                },
            )
        }
        "kill" => {
            let req: KillRequest = read(method, request)?;
            registry.kill(&JobId(req.id.clone()))?;
            write(method, &KillReply { id: req.id })
        }
        "list" => {
            let jobs = registry
                .list()
                .into_iter()
                .map(|(id, status)| JobSummary { id: id.0, status })
                .collect();
            write(method, &ListReply { jobs })
        }
        other => Err(WireError::UnknownMethod(other.to_string())),
    }
}

/// The methods this surface answers, in a stable order.
///
/// `decide` is deliberately absent: approval is the human's write, and putting
/// it on a remotely-callable surface would advertise the gate on the very
/// channel the gate exists to constrain.
///
/// implements: intervention-is-the-only-human-write
pub const METHODS: &[&str] = &["start", "tail", "wait", "kill", "list"];
