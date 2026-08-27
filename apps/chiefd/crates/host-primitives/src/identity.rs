//! What a process is, to the two actuators.
//!
//! Both crates declared these byte-identically, which is the only day a move
//! like this is free. Nothing here is new — [`Pid`] and [`ProcIdentity`] are
//! the definitions that were in `chiefd_host::executor` and
//! `chief_cli::actuate::host`, verbatim.

use std::fmt;

/// An OS process id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pid(pub i32);

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Kernel-attested process identity: pid plus `/proc/<pid>/stat` field 20.
///
/// Start-time matching is what defeats pid recycling on the *observed* pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcIdentity {
    /// The pid observed.
    pub pid: Pid,
    /// Kernel start time (clock ticks since boot).
    pub start_time: u64,
}
