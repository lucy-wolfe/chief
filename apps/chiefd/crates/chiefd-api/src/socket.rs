//! Socket and runtime-target resolution, shared by the daemon and `chiefctl`.
//!
//! Two rules from the plan live here because both binaries need them and
//! neither may implement them differently:
//!
//! * The daemon listens on `$CHIEFD_SOCKET` if set, otherwise
//!   `~/.local/share/tribe-launcher/chiefd.sock` (plan §0).
//! * `--socket` and `--session` are a **pair**. Half-set is an error, never a
//!   partial fallback (plan §1, D17). Attach/switch-client and `$RUNTIME`
//!   resolution stay entirely on the client side; the client resolves ambient
//!   context and sends the socket name, and chiefd enforces
//!   durable-ownership-wins (plan §4).

use std::path::PathBuf;

/// Environment variable that overrides the daemon socket path.
pub const SOCKET_ENV: &str = "CHIEFD_SOCKET";

/// Path under `$HOME` used when [`SOCKET_ENV`] is unset.
pub const DEFAULT_SOCKET_RELATIVE: &str = ".local/share/tribe-launcher/chiefd.sock";

/// Why a socket path could not be determined, or a target was half-specified.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TargetError {
    /// `$CHIEFD_SOCKET` was unset and `$HOME` was not usable.
    #[error("neither {SOCKET_ENV} nor a usable HOME is set")]
    NoSocketPath,
    /// `--socket` was given without `--session`, or vice versa.
    #[error("--socket and --session must be given together; {given} was set without {missing}")]
    HalfSpecifiedTarget {
        /// The flag that was provided.
        given: &'static str,
        /// The flag that was omitted.
        missing: &'static str,
    },
}

/// Resolve the daemon socket path from the environment.
///
/// `socket_env` and `home` are passed in rather than read here so tests do not
/// mutate process-global environment.
///
/// # Errors
/// [`TargetError::NoSocketPath`] when neither source yields a path.
pub fn resolve_socket_path(
    socket_env: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, TargetError> {
    if let Some(explicit) = socket_env.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(explicit));
    }
    let home = home.filter(|value| !value.is_empty()).ok_or(TargetError::NoSocketPath)?;
    Ok(PathBuf::from(home).join(DEFAULT_SOCKET_RELATIVE))
}

/// A resolved runtime target: either fully specified, or deliberately absent so
/// chiefd falls back to durable ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeTarget {
    /// No target supplied; durable ownership decides.
    Durable,
    /// Both halves supplied.
    Explicit {
        /// runtime server socket name (`runtime -L <socket>`).
        socket: String,
        /// runtime session name.
        session: String,
    },
}

impl RuntimeTarget {
    /// Apply the pair rule to a pair of optional flags.
    ///
    /// # Errors
    /// [`TargetError::HalfSpecifiedTarget`] when exactly one is present. There
    /// is deliberately no fallback for the missing half: guessing a session
    /// for a caller-named socket is how a command lands on the wrong company.
    pub fn resolve(socket: Option<&str>, session: Option<&str>) -> Result<Self, TargetError> {
        match (socket, session) {
            (None, None) => Ok(Self::Durable),
            (Some(socket), Some(session)) => {
                Ok(Self::Explicit { socket: socket.to_string(), session: session.to_string() })
            }
            (Some(_), None) => {
                Err(TargetError::HalfSpecifiedTarget { given: "--socket", missing: "--session" })
            }
            (None, Some(_)) => {
                Err(TargetError::HalfSpecifiedTarget { given: "--session", missing: "--socket" })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_socket_env_wins_over_home() {
        let path = resolve_socket_path(Some("/run/chiefd.sock"), Some("/home/user"))
            .expect("explicit path resolves");
        assert_eq!(path, PathBuf::from("/run/chiefd.sock"));
    }

    #[test]
    fn empty_socket_env_is_treated_as_unset() {
        let path = resolve_socket_path(Some(""), Some("/home/user")).expect("falls back to HOME");
        assert_eq!(path, PathBuf::from("/home/user/.local/share/tribe-launcher/chiefd.sock"));
    }

    #[test]
    fn no_socket_and_no_home_is_an_error_not_a_relative_path() {
        assert_eq!(resolve_socket_path(None, None), Err(TargetError::NoSocketPath));
        assert_eq!(resolve_socket_path(None, Some("")), Err(TargetError::NoSocketPath));
    }

    #[test]
    fn both_halves_present_resolves_explicitly() {
        let target = RuntimeTarget::resolve(Some("cobalt"), Some("cobalt-main")).expect("pair");
        assert_eq!(
            target,
            RuntimeTarget::Explicit { socket: "cobalt".into(), session: "cobalt-main".into() }
        );
    }

    #[test]
    fn neither_half_present_falls_back_to_durable_ownership() {
        assert_eq!(RuntimeTarget::resolve(None, None).expect("durable"), RuntimeTarget::Durable);
    }

    #[test]
    fn half_specified_targets_are_errors_in_both_directions() {
        assert_eq!(
            RuntimeTarget::resolve(Some("cobalt"), None),
            Err(TargetError::HalfSpecifiedTarget { given: "--socket", missing: "--session" })
        );
        assert_eq!(
            RuntimeTarget::resolve(None, Some("cobalt-main")),
            Err(TargetError::HalfSpecifiedTarget { given: "--session", missing: "--socket" })
        );
    }
}
