//! The structured observability stream: one JSONL line per event, one file per
//! service.
//!
//! Every chiefd program emits the *same* line shape, because the consumer is
//! one reader grepping one directory. A per-program format would mean two
//! parsers and two mental models, and the correlation ids would stop joining —
//! which is precisely the failure that let a P0 hide for nineteen hours.
//!
//! Deliberately hand-rolled rather than layered onto `tracing-subscriber`'s
//! JSON formatter: that formatter nests fields under `fields` and adds its own
//! metadata keys, so the emitted line would not match this schema. The console
//! `tracing` layer is untouched and keeps serving humans; [`crate::layer`] is
//! what feeds this sink from the same `tracing` calls.
//!
//! Everything here is best-effort by construction. A logger that can fail a
//! request it was only supposed to observe is worse than no logger.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// Bumped only on a breaking key change; must track `ORG_LOG_SCHEMA_VERSION`.
pub const SCHEMA_VERSION: u32 = 1;

/// Organization-agnostic processes still emit the field, spelled explicitly, so
/// that the presence of a real slug stays meaningful.
pub const NO_ORGANIZATION: &str = "-";

const DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_HEARTBEAT_SECS: u64 = 60;

/// Where the observability streams live.
///
/// TWO answers, both named, neither guessed:
///
/// 1. **`ORG_LAUNCHER_ORG_DIR`** — the company directory a pane or a company
///    daemon is stamped with. Its logs go to `<dir>/.chief/log/`, beside the
///    store whose story they tell.
/// 2. **`$HOME/.chief/log/`** — for a BOX-WIDE process, which is a real
///    category and not a fallback: `beacond` serves every company on the
///    machine and belongs to none of them, and `chief` itself spends the
///    minutes before any company exists with no directory to write into.
///
/// # Why the four-tier ladder this replaces was a defect
///
/// It tried `ORG_LAUNCHER_DATA_ROOT`, then `dirname(CHIEFD_DATA_ROOT)`, then
/// `$HOME/.chiefd`, then the literal `/root/.chiefd`. Tier 2 RECONSTRUCTED a
/// directory by walking up from another one — correct only while the launcher
/// derived the data root as exactly `dirname(orgs)` — and tier 4 was only ever
/// right on one Linux box running as root. Because this logger is best-effort
/// by construction, guessing wrong is SILENT: the daemon simply stops leaving
/// evidence. That was reachable, not theoretical — measured against a live
/// macOS daemon, `ORG_LAUNCHER_DATA_ROOT` was absent while `CHIEFD_DATA_ROOT`
/// was present, so every sink in that process resolved under `/root` and wrote
/// nothing. A module whose own header says it exists because a correlation gap
/// "let a P0 hide for nineteen hours" is a bad place for a silent guess.
///
/// With the company IN the directory there is nothing left to reconstruct: a
/// process either knows its company directory or is box-wide, and both cases
/// have a name.
///
/// # Why a missing `$HOME` writes nothing rather than picking a directory
///
/// The deleted last resort was `/root/.chiefd`, and on any host where `$HOME`
/// is unset that is a directory the process almost certainly cannot write —
/// so the ladder's final rung produced the same silence it existed to prevent,
/// while looking like it had an answer. `None` says the honest thing, and the
/// console layer still carries every line.
fn log_root_from_env(company_dir: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    fn usable(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    if let Some(dir) = usable(company_dir) {
        return Some(Path::new(dir).join(CHIEF_DIR).join("log"));
    }
    usable(home).map(|home| Path::new(home).join(CHIEF_DIR).join("log"))
}

/// The directory chief owns, inside a company directory and inside `$HOME`
/// alike. One name for both, because they hold the same kind of thing.
///
/// `pub` so the daemon and the operator client can spell it the same way. They
/// did not: the install pointer `~/.chief/launcher-root` is WRITTEN by the
/// client's `paths::install_home` and READ by `chiefd-daemon`, and after the
/// install tree moved, the read still said `~/.chiefd`. A missing pointer is
/// deliberately ABSENT rather than an error there, so the daemon simply
/// resolved no launcher root, materialized every person with an empty
/// `pi-home/extensions/`, and a company came up whose CEO had no `org_*` tools
/// at all — over a genesis that reported success. `chiefd-log` is the one leaf
/// both sides already link, so this is the cheapest place for the name to have
/// a single spelling.
pub const CHIEF_DIR: &str = ".chief";

/// The variable a pane and a company daemon are stamped with: the company
/// DIRECTORY. It replaces `ORG_LAUNCHER_DATA_ROOT` and `CHIEFD_DATA_ROOT`,
/// which between them named a global tree and a slug-keyed subdirectory of it.
///
/// # One name, and why it is this one
///
/// This briefly had a second spelling. `ORG_LAUNCHER_ORG_DIR` was already the
/// name `chiefd-host` stamped panes with and the name every TypeScript reader
/// used — `organization-intercom.ts`, `team-ui.ts`, and two skills that ship
/// to agents; `ORG_LAUNCHER_DIR` was a shorter name invented here for the same
/// fact. Two spellings of one variable is exactly the duplication this stage
/// removes, and the tie breaks toward the readers: nine against two. The
/// awkward `ORG_LAUNCHER_` prefix is retired vocabulary and is swept whole in
/// the founder-to-chief rename; splitting that sweep in half here would have
/// meant renaming this twice.
pub const COMPANY_DIR_ENV: &str = "ORG_LAUNCHER_ORG_DIR";

/// Sequence numbers are per *stream*, not per process: one process writes
/// several streams, so a process-wide counter would leave gaps in every file and
/// a gap would prove nothing. Per-stream, a gap is positive evidence that a line
/// was lost or the file was truncated.
fn next_sequence(path: &Path) -> u64 {
    static SEQUENCES: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
    let mut guard = match SEQUENCES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let counter = guard.entry(path.to_path_buf()).or_insert(0);
    let value = *counter;
    *counter += 1;
    value
}

static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Lines this process failed to write. Surfaced by the heartbeat, because a
/// logger that fails invisibly recreates the defect it exists to prevent.
pub fn dropped_lines() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

fn env_u64(key: &str, fallback: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

/// Per-stream byte cap before rotation (`ORG_LOG_MAX_BYTES`).
pub fn max_bytes() -> u64 {
    env_u64("ORG_LOG_MAX_BYTES", DEFAULT_MAX_BYTES)
}

/// Interval between liveness lines (`ORG_LOG_HEARTBEAT_MS`, default 60s).
pub fn heartbeat_interval() -> Duration {
    Duration::from_millis(env_u64("ORG_LOG_HEARTBEAT_MS", DEFAULT_HEARTBEAT_SECS * 1_000))
}

pub use crate::isotime::timestamp;

/// The closed set of top-level keys, mirroring `ORG_LOG_TOP_LEVEL_KEYS`. Free-form
/// payload is confined to `detail` so the watcher's queries cannot be broken by a
/// careless call site.
pub const TOP_LEVEL_KEYS: &[&str] = &[
    "schemaVersion",
    "at",
    "level",
    "service",
    "event",
    "organization",
    "pid",
    "seq",
    "personId",
    "effectId",
    "assignmentId",
    "messageId",
    "detail",
];

/// A structured log sink bound to one service's file.
#[derive(Clone, Debug)]
pub struct OrgLog {
    path: PathBuf,
    service: String,
    organization: String,
    /// Resolved once at construction. Tests override it here rather than by
    /// setting `ORG_LOG_MAX_BYTES`, because environment variables are
    /// process-global: one test mutating the cap would silently change the
    /// rotation behaviour of every other test running beside it.
    max_bytes: u64,
}

impl OrgLog {
    /// `log_root` is the directory the `.jsonl` streams sit in directly —
    /// `<dir>/.chief/log` for a company process, `$HOME/.chief/log` for a
    /// box-wide one. It is joined verbatim: the caller has already decided
    /// which of the two this process is, and a second `logs` segment appended
    /// here would put the answer in two places.
    pub fn new(log_root: impl AsRef<Path>, service: &str, organization: &str) -> Self {
        Self {
            path: log_root.as_ref().join(format!("{service}.jsonl")),
            service: service.to_string(),
            organization: organization.to_string(),
            max_bytes: max_bytes(),
        }
    }

    /// Override the rotation cap for this sink.
    #[must_use]
    pub fn with_max_bytes(mut self, cap: u64) -> Self {
        self.max_bytes = cap;
        self
    }

    /// Resolve the sink from the same environment the TypeScript side reads, so
    /// the two runtimes land in one directory without a second configuration
    /// mechanism to keep in sync.
    ///
    /// `None` when nothing names a directory to write into — see
    /// [`log_root_from_env`] for why that is an honest answer rather than a
    /// missing fallback.
    pub fn from_env(service: &str, organization: Option<&str>) -> Option<Self> {
        let log_root = log_root_from_env(
            std::env::var(COMPANY_DIR_ENV).ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )?;
        Some(Self::new(log_root, service, organization.unwrap_or(NO_ORGANIZATION)))
    }

    /// The stream this sink appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Emit one line. `detail` is pre-serialized JSON object body (without the
    /// enclosing braces) so callers can use `serde_json` or a plain string
    /// without this module taking a serde dependency stance for them.
    pub fn emit(&self, level: &str, event: &str, detail: &str) {
        self.emit_for(None, level, event, detail);
    }

    /// [`OrgLog::emit`] naming a company this one line is about.
    ///
    /// The sink's own `organization` is fixed when it is built, which is right
    /// for a per-company stream and wrong for the daemon-level one: the process
    /// that owns the slow part of a launch has no company when it starts and
    /// one by the time it finishes. `None` keeps the sink's own value.
    pub fn emit_for(
        &self,
        organization: Option<&str>,
        level: &str,
        event: &str,
        detail: &str,
    ) -> bool {
        let line = self.render(organization, level, event, detail);
        if self.append(&line).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn render(&self, organization: Option<&str>, level: &str, event: &str, detail: &str) -> String {
        let organization = organization
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.organization.as_str());
        let seq = next_sequence(&self.path);
        let mut line = format!(
            r#"{{"schemaVersion":{SCHEMA_VERSION},"at":"{at}","level":"{level}","service":"{service}","event":"{event}","organization":"{organization}","pid":{pid},"seq":{seq}"#,
            at = timestamp(SystemTime::now()),
            level = escape(level),
            service = escape(&self.service),
            event = escape(event),
            organization = escape(organization),
            pid = std::process::id(),
        );
        if !detail.is_empty() {
            line.push_str(r#","detail":{"#);
            line.push_str(detail);
            line.push('}');
        }
        line.push('}');
        line.push('\n');
        line
    }

    /// The fallible core of [`OrgLog::emit`], exposed so a test can assert what
    /// actually happened to one line rather than inferring it from a global
    /// counter every other concurrent test can also move.
    pub fn try_emit(&self, level: &str, event: &str, detail: &str) -> std::io::Result<()> {
        let line = self.render(None, level, event, detail);
        self.append(&line)
    }

    /// [`OrgLog::emit`] with the detail body given as a JSON object rather than
    /// as a pre-serialized fragment.
    ///
    /// This is what [`crate::layer`] uses. A `tracing` event's fields are
    /// arbitrary — a `Debug` rendering can contain a quote, a newline or a
    /// brace — so the fragment form would put the framing of every line at the
    /// mercy of a call site's formatting. `serde_json` escapes it once,
    /// correctly, and an empty object emits no `detail` key at all so a line
    /// with nothing to say stays the same shape it always had.
    pub fn emit_object(&self, level: &str, event: &str, detail: &serde_json::Map<String, Value>) {
        self.emit_object_for(None, level, event, detail);
    }

    /// [`OrgLog::emit_object`] naming a company this one line is about — see
    /// [`OrgLog::emit_for`] for why one sink writes lines about several.
    pub fn emit_object_for(
        &self,
        organization: Option<&str>,
        level: &str,
        event: &str,
        detail: &serde_json::Map<String, Value>,
    ) {
        if detail.is_empty() {
            self.emit_for(organization, level, event, "");
            return;
        }
        // The braces are the sink's, so the caller's object is rendered and
        // then unwrapped rather than concatenated by hand.
        let rendered = Value::Object(detail.clone()).to_string();
        let body = rendered.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')).unwrap_or("");
        self.emit_for(organization, level, event, body);
    }

    /// Append, rotating first when the file would exceed the cap. Exactly one
    /// previous generation is retained, so on-disk bytes for a stream never
    /// exceed `2 * max_bytes` plus one in-flight line. The rename is within one
    /// directory and therefore atomic: a reader never sees a half-rotated file.
    // The workspace seam reserves filesystem effects for `chiefd_host::executor`
    // because an ad-hoc write re-creates the multi-writer world chiefd deletes.
    // An observability sink is the one thing that must NOT flow through a host
    // transaction: it has to be able to record that a transaction failed. It is
    // append-only, writes to a directory no other seam owns, and can never
    // affect the state being observed. Narrow and commented per README §5.2, so
    // the exemption stays greppable.
    #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let cap = self.max_bytes;
        if let Ok(meta) = fs::metadata(&self.path) {
            if meta.len() + line.len() as u64 > cap {
                let rotated = self.path.with_extension("jsonl.1");
                let _ = fs::rename(&self.path, rotated);
            }
        }
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        file.write_all(line.as_bytes())
    }
}

/// Minimal JSON string escaping for the fields this module controls. Detail
/// bodies are the caller's responsibility and should come from `serde_json`.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A COMPANY process writes beside the store whose story it tells.
    #[test]
    fn a_company_process_logs_into_its_own_directory() {
        let root = log_root_from_env(Some("/work/anvils"), Some("/home/x")).expect("a root");
        assert_eq!(root, Path::new("/work/anvils/.chief/log"));
        let log = OrgLog::new(&root, "chiefd", "anvil-works");
        assert_eq!(log.path(), Path::new("/work/anvils/.chief/log/chiefd.jsonl"));
    }

    /// A BOX-WIDE process is a real category, not a fallback.
    ///
    /// `beacond` serves every company on the machine and belongs to none of
    /// them, and `chief` itself owns the minutes before any company exists —
    /// which is exactly the record the 4½-minute-launch incident could not
    /// find. Both land in one place that is nobody's company.
    #[test]
    fn a_box_wide_process_logs_under_the_operators_own_chief_directory() {
        let root = log_root_from_env(None, Some("/Users/user")).expect("a root");
        assert_eq!(root, Path::new("/Users/user/.chief/log"));
        let log = OrgLog::new(&root, "beacond", NO_ORGANIZATION);
        assert_eq!(log.path(), Path::new("/Users/user/.chief/log/beacond.jsonl"));
    }

    /// THE DEFECT THE OLD LADDER PRODUCED, now unreachable by construction.
    ///
    /// It ended in the literal `/root/.chiefd`, correct only on one Linux box
    /// running as root; anywhere else that is a directory the process cannot
    /// write, and because this logger is best-effort the failure was SILENT.
    /// Nothing resolves under `/root` unless `$HOME` says so.
    #[test]
    fn nothing_ever_resolves_under_a_guessed_root_directory() {
        for (dir, home) in [
            (Some("/work/anvils"), None),
            (Some("/work/anvils"), Some("/Users/user")),
            (None, Some("/Users/user")),
        ] {
            let resolved = log_root_from_env(dir, home).expect("a root");
            assert!(!resolved.starts_with("/root"), "resolved under /root: {}", resolved.display());
        }
        // And `/root` IS reachable — when the environment actually says so,
        // which is what proves the assertions above were driven by the inputs
        // rather than by an unreachable branch.
        assert_eq!(
            log_root_from_env(None, Some("/root")).expect("a root"),
            Path::new("/root/.chief/log")
        );
    }

    /// NO DIRECTORY IS AN ANSWER, and it is the honest one.
    ///
    /// The ladder's last rung used to invent `/root/.chiefd` here, producing
    /// exactly the silence it existed to prevent while looking like it had
    /// succeeded. `None` leaves the console layer carrying every line.
    #[test]
    fn a_process_with_no_directory_and_no_home_writes_no_file_at_all() {
        assert_eq!(log_root_from_env(None, None), None);
        assert_eq!(log_root_from_env(Some("   "), Some("")), None);
    }

    #[test]
    fn treats_blank_values_as_absent() {
        // A blank env var is a broken environment, not an instruction to write
        // to the filesystem root.
        assert_eq!(
            log_root_from_env(Some("  "), Some("/home/x")).expect("a root"),
            Path::new("/home/x/.chief/log")
        );
    }

    #[test]
    fn every_line_carries_a_millisecond_timestamp() {
        let dir = tempdir("timestamp");
        let log = OrgLog::new(&dir, "chiefd", "acme");
        log.emit("info", "heartbeat", "");
        let body = fs::read_to_string(log.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let at = value["at"].as_str().unwrap();
        assert_eq!(at.len(), 24, "timestamp must be ISO-8601 with millis: {at}");
        assert!(at.ends_with('Z'));
        assert!(at.contains('T') && at.contains('.'));
    }

    #[test]
    fn top_level_keys_match_the_typescript_schema() {
        let dir = tempdir("keys");
        let log = OrgLog::new(&dir, "chiefd", "acme");
        log.emit("warn", "request-failed", r#""error":"boom""#);
        let body = fs::read_to_string(log.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                TOP_LEVEL_KEYS.contains(&key.as_str()),
                "emitted an unexpected top-level key: {key}"
            );
        }
        assert_eq!(value["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(value["service"], "chiefd");
        assert_eq!(value["organization"], "acme");
        assert_eq!(value["detail"]["error"], "boom");
    }

    #[test]
    fn sequence_numbers_are_gapless_so_loss_is_detectable() {
        let dir = tempdir("sequence");
        let log = OrgLog::new(&dir, "write-db", NO_ORGANIZATION);
        for _ in 0..5 {
            log.emit("info", "heartbeat", "");
        }
        let body = fs::read_to_string(log.path()).unwrap();
        let sequences: Vec<u64> = body
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["seq"].as_u64().unwrap()
            })
            .collect();
        for pair in sequences.windows(2) {
            assert_eq!(pair[1], pair[0] + 1);
        }
    }

    #[test]
    fn a_stream_is_bounded_by_rotation_and_keeps_one_generation() {
        let dir = tempdir("rotation");
        let log = OrgLog::new(&dir, "chiefd", "acme").with_max_bytes(4096);
        let filler = "x".repeat(200);
        for _ in 0..200 {
            log.emit("info", "tick", &format!(r#""filler":"{filler}""#));
        }
        let live = fs::metadata(log.path()).unwrap().len();
        let rotated = fs::metadata(log.path().with_extension("jsonl.1")).unwrap().len();
        assert!(live <= 4096 + 512, "live stream exceeded its cap: {live}");
        assert!(rotated <= 4096 + 512);
        // A third generation must never accumulate.
        assert!(!log.path().with_extension("jsonl.2").exists());
    }

    #[test]
    fn an_unwritable_sink_drops_the_line_instead_of_panicking() {
        // The log path itself is a directory, so the append cannot succeed.
        let dir = tempdir("unwritable");
        fs::create_dir_all(dir.join("chiefd.jsonl")).unwrap();
        let log = OrgLog::new(&dir, "chiefd", "acme");
        // Assert the outcome of this one line directly. `dropped_lines()` is a
        // process-global counter that every concurrently running test can also
        // move, so sampling it before and after is a race, not a measurement.
        assert!(log.try_emit("info", "heartbeat", "").is_err());
        // And the infallible entry point must swallow that failure whole: a
        // logger that can break the runtime it observes is worse than none.
        log.emit("info", "heartbeat", "");
        assert!(dropped_lines() >= 1);
    }

    #[test]
    fn control_characters_cannot_break_the_line_framing() {
        let dir = tempdir("framing");
        let log = OrgLog::new(&dir, "chiefd", "acme");
        log.emit("info", "weird\nevent\"name", "");
        let body = fs::read_to_string(log.path()).unwrap();
        assert_eq!(body.lines().count(), 1, "an event name must never split a line");
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(value["event"], "weird event\"name");
    }

    /// The layer's entry point. A field value carrying a quote, a brace or a
    /// newline must not be able to change the framing or the key set of the
    /// line it lands in — the fragment form put exactly that at the mercy of
    /// the call site.
    #[test]
    fn an_object_detail_is_escaped_once_and_cannot_break_the_line() {
        let dir = tempdir("emit-object");
        let log = OrgLog::new(&dir, "chiefd", "acme");
        let mut detail = serde_json::Map::new();
        detail.insert("durationMs".to_owned(), serde_json::json!(274_091_u64));
        detail.insert("note".to_owned(), serde_json::json!("has \"quotes\",\n a brace } and {"));
        log.emit_object("info", "step.exit", &detail);

        let body = fs::read_to_string(log.path()).unwrap();
        assert_eq!(body.lines().count(), 1, "a field value must never split a line");
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(value["detail"]["durationMs"], 274_091_u64);
        assert_eq!(value["detail"]["note"], "has \"quotes\",\n a brace } and {");
        for key in value.as_object().unwrap().keys() {
            assert!(TOP_LEVEL_KEYS.contains(&key.as_str()), "unexpected top-level key: {key}");
        }
    }

    /// An empty object emits no `detail` key at all, so a line with nothing to
    /// say keeps the shape it has always had.
    #[test]
    fn an_empty_object_detail_omits_the_key_rather_than_writing_an_empty_one() {
        let dir = tempdir("emit-object-empty");
        let log = OrgLog::new(&dir, "chiefd", "acme");
        log.emit_object("info", "heartbeat", &serde_json::Map::new());
        let body = fs::read_to_string(log.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert!(value.get("detail").is_none(), "an empty detail must not be written");
    }

    /// Named per test rather than derived from pid/thread identity. Tests run
    /// concurrently and a directory two tests can both resolve to is a test
    /// that fails on somebody else's machine for reasons unrelated to the code.
    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("orglog-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
