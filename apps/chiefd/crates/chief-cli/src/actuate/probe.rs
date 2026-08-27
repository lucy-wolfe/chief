//! P1-only tmux single-writer evidence capture.
//!
//! This module is deliberately a one-way observer: when the explicit probe
//! environment is absent it returns before allocating context, reading a clock,
//! or opening a file. When enabled, each event is one small `O_APPEND` write so
//! a Rust duty process and the TypeScript launcher can append to the same JSONL
//! artifact without a shared in-memory coordinator.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::actuate::host::Socket;

const ENABLED: &str = "TMUX_SINGLE_WRITER_PROBE";
const PATH: &str = "TMUX_SINGLE_WRITER_PROBE_PATH";
const TEST_ID: &str = "TMUX_SINGLE_WRITER_PROBE_TEST_ID";
const CORRELATION_ID: &str = "TMUX_SINGLE_WRITER_PROBE_CORRELATION_ID";
const TOPOLOGY_VERBS: &[&str] = &[
    "join-pane",
    "kill-pane",
    "kill-session",
    "kill-window",
    "move-window",
    "new-session",
    "new-window",
    "respawn-pane",
    "select-layout",
    "set-option",
    "split-window",
];

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Context for one interpreter mutation. Construction is the probe gate: an
/// unset probe returns before cloning runtime identity or scanning command
/// arguments, preserving the production path's behavior and cost.
pub(super) struct MutationContext {
    config: Config,
    socket: String,
    organization: String,
    session: String,
    target: Option<String>,
    verb: String,
}

#[derive(Clone)]
struct Config {
    path: String,
    test_id: String,
    correlation_id: String,
}

#[derive(Serialize)]
struct Event<'a> {
    version: u8,
    writer: &'static str,
    process_id: u32,
    phase: &'static str,
    timestamp_ms: u128,
    sequence: u64,
    test_id: &'a str,
    correlation_id: &'a str,
    socket: &'a str,
    organization: &'a str,
    session: &'a str,
    target: Option<&'a str>,
    verb: &'a str,
    topology_affecting: bool,
    outcome: Option<Outcome>,
}

#[derive(Serialize)]
struct Outcome {
    kind: &'static str,
    exit_status: Option<i32>,
}

impl MutationContext {
    /// Build probe context only for a topology mutation and only with all
    /// explicit test-owned inputs present. A misconfigured probe stays
    /// behaviorally inert; the P1 verifier then fails on the missing evidence.
    pub(super) fn for_command(
        socket: &Socket,
        organization: &str,
        session: &str,
        verb: &str,
        argv: &[String],
    ) -> Option<Self> {
        if std::env::var_os(ENABLED).as_deref() != Some(std::ffi::OsStr::new("1")) {
            return None;
        }
        if !is_topology_verb(verb) {
            return None;
        }
        let config = Config {
            path: required(PATH)?,
            test_id: required(TEST_ID)?,
            correlation_id: required(CORRELATION_ID)?,
        };
        Some(Self {
            config,
            socket: socket.0.clone(),
            organization: organization.to_owned(),
            session: session.to_owned(),
            target: tmux_target(argv, verb),
            verb: verb.to_owned(),
        })
    }

    pub(super) fn attempt(&self) {
        self.write("attempt", None);
    }

    pub(super) fn result(&self, exit_status: Option<i32>, kind: &'static str) {
        self.write("result", Some(Outcome { kind, exit_status }));
    }

    fn write(&self, phase: &'static str, outcome: Option<Outcome>) {
        let timestamp_ms =
            SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis());
        let event = Event {
            version: 1,
            writer: "rust",
            process_id: std::process::id(),
            phase,
            timestamp_ms,
            sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
            test_id: &self.config.test_id,
            correlation_id: &self.config.correlation_id,
            socket: &self.socket,
            organization: &self.organization,
            session: &self.session,
            target: self.target.as_deref(),
            verb: &self.verb,
            topology_affecting: true,
            outcome,
        };
        let Ok(mut line) = serde_json::to_vec(&event) else {
            return;
        };
        line.push(b'\n');
        // The filesystem-effect boundary owns the file handle. It performs
        // one O_APPEND write; any error (including a short write) remains
        // probe-only and is made visible by missing/malformed CI evidence.
        let _ = crate::files::append_record_once(std::path::Path::new(&self.config.path), &line);
    }
}

fn required(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    (!value.trim().is_empty()).then_some(value)
}

fn is_topology_verb(verb: &str) -> bool {
    TOPOLOGY_VERBS.contains(&verb)
}

fn tmux_target(argv: &[String], verb: &str) -> Option<String> {
    let command_start = argv.iter().position(|arg| arg == verb)?;
    let command = &argv[command_start..];
    let command_end = command.iter().position(|arg| arg == ";").unwrap_or(command.len());
    let target = command[..command_end]
        .windows(2)
        .find_map(|pair| (pair[0] == "-t" || pair[0] == "-s").then(|| pair[1].as_str()))?;
    // Targets are tmux identifiers, not command payload. Keep the probe
    // defensive anyway: no free-form command material crosses this boundary.
    (target.len() <= 256
        && target.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-:%@/".contains(&byte)))
    .then(|| target.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{is_topology_verb, tmux_target};

    #[test]
    fn only_topology_verbs_are_eligible_for_probe_observation() {
        assert!(is_topology_verb("new-session"));
        assert!(is_topology_verb("set-option"));
        assert!(!is_topology_verb("show-options"));
        assert!(!is_topology_verb("display-message"));
    }

    #[test]
    fn target_capture_is_limited_to_safe_tmux_identifiers() {
        assert_eq!(
            tmux_target(
                &["kill-session".into(), "-t".into(), "probe-session".into()],
                "kill-session"
            ),
            Some("probe-session".into())
        );
        assert_eq!(
            tmux_target(
                &["new-session".into(), "-s".into(), "probe-session".into()],
                "new-session"
            ),
            Some("probe-session".into())
        );
        assert_eq!(
            tmux_target(&["new-session".into(), "-t".into(), "unsafe value".into()], "new-session"),
            None
        );
    }

    #[test]
    fn target_capture_is_scoped_to_the_selected_command_in_a_queue() {
        let argv = [
            "start-server",
            ";",
            "set-option",
            "-s",
            "extended-keys",
            "on",
            ";",
            "new-session",
            "-d",
            "-s",
            "probe-session",
            ";",
            "set-option",
            "-t",
            "probe-session",
            "@organization_id",
            "probe-org",
        ]
        .map(str::to_owned);
        assert_eq!(tmux_target(&argv, "new-session"), Some("probe-session".into()));
    }
}
