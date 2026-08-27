//! How a host operation failed.
//!
//! Moved here from `chiefd_host::executor` and `chief_cli::actuate::host`,
//! which held one copy each. The copies had already drifted:
//! [`HostErr::Untrusted`]'s `reason` was `&'static str` on the client side and
//! `String` on the backend side. `String` is the shape that survives — see the
//! field's own doc for why the backend needs an owned value — so the client's
//! literals now convert at the construction site. Every message text is
//! unchanged.

/// Failure of a host operation.
///
/// Deliberately separate from `chiefd_core::ChiefdError`: the store layer
/// decides which host failures are refusals, which are `Unavailable`, and
/// which are retried. A host error is never returned to a caller verbatim —
/// stderr passes through `redact()` first.
///
/// [`HostErr::Untrusted`] is the load-bearing variant and the reason this is
/// an enum rather than a string: a transient condition must never be read as
/// permission to take over. It is what an actuator reports to chiefd as
/// `observationTrusted: false`, and chiefd withholds every action on it rather
/// than concluding that nothing is running.
#[derive(Debug, thiserror::Error)]
pub enum HostErr {
    /// The tool (the runtime, pi, …) could not be run at all.
    #[error("host tool {tool} unavailable: {detail}")]
    ToolUnavailable {
        /// Which tool.
        tool: &'static str,
        /// Redacted detail.
        detail: String,
    },
    /// The tool ran and reported failure.
    #[error("host tool {tool} failed: {detail}")]
    ToolFailed {
        /// Which tool.
        tool: &'static str,
        /// Redacted detail.
        detail: String,
    },
    /// A transient condition the trust rules say must NOT be read as
    /// permission to take over (plan §4: the 20×25 ms "server exited
    /// unexpectedly" retry, the "invalid option" no-tag response).
    #[error("host observation untrusted: {reason}")]
    Untrusted {
        /// Why the observation cannot be relied on.
        ///
        /// Owned, not `&'static str`: the untrusted observations chiefd now
        /// reasons about are the ACTUATOR's, and the actuator sends its reason
        /// as prose on the wire. A fixed set of compile-time strings could only
        /// have carried that by discarding it and substituting one of its own.
        reason: String,
    },
    /// Filesystem step failed; a host transaction rolls back from backups.
    #[error("host filesystem step failed: {detail}")]
    Filesystem {
        /// Redacted detail.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_observation_is_its_own_variant_not_a_generic_failure() {
        // The trust rules depend on callers being able to *distinguish* "runtime
        // hiccuped" from "runtime answered no". Collapsing them is how a
        // transient error becomes takeover permission.
        let untrusted = HostErr::Untrusted { reason: "server exited unexpectedly".into() };
        let failed = HostErr::ToolFailed { tool: "runtime", detail: "no such session".into() };
        assert!(matches!(untrusted, HostErr::Untrusted { .. }));
        assert!(!matches!(failed, HostErr::Untrusted { .. }));
        assert!(untrusted.to_string().contains("untrusted"));
    }
}
