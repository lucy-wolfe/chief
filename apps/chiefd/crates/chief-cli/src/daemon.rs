//! Per-company daemon lifecycle: start, stop, status.
//!
//! Ported from the deleted TypeScript `chiefd-process.ts` (976 lines) and
//! its adapter `chiefd-adapter.ts`, both deleted. One chiefd process per
//! company DIRECTORY (`chiefd run --dir <dir>`); this module is the ONLY place
//! the operator surface starts one.
//!
//! # THE HARD RULE, preserved
//!
//! Starting a daemon starts the supervisor, not agents. Nothing here creates a
//! tmux pane or launches a person — it has no import of the tmux module at all,
//! so that violation is unreachable from this file's own surface rather than
//! merely untested.
//!
//! # THE RENDEZVOUS REPLACED THE REGISTRY LOOKUP, and the ladder did not change
//!
//! A command used to find its company's daemon by asking beacond for a slug.
//! It does not any more: a company is a DIRECTORY, and the daemon writes its
//! own location into that directory
//! ([`host_primitives::rendezvous::DaemonRendezvous`]), so the client reads it
//! there. No registry is on the path between a command and its own company,
//! and beacond survives only as the box-wide presence registry `chief ls`
//! renders.
//!
//! **Only the SOURCE of the URL changed.** Every rung of the liveness ladder
//! the registry lookup fed is still climbed, in the same order and for the same
//! reasons: the file must describe THIS directory (a copied project carries its
//! original's rendezvous), the pid must be alive, the listener must answer
//! healthy, and it must prove it is serving THIS company's key. A rendezvous is
//! a POINTER and never authority — a stale one after a reboot is the ordinary
//! case, and it is overwritten by the next daemon rather than trusted.
//!
//! # What is gone with the TypeScript
//!
//! - **The `docstore-only` bootstrap start/stop pair.** It existed only so a
//!   pre-genesis daemon could serve a store before a company existed; the
//!   create flow stopped using it, and Mandate 0 does not keep a second way to
//!   start a daemon alive for a test harness. Genesis now lands on the same
//!   daemon that will serve the CEO ([`super::genesis`]).
//! - **Every pid-file read.** The rendezvous is not one: a pid file is a claim
//!   to be believed, and this is a hint that must be re-proved on every read.
//! - **The adapter layer.** The narrow controller interface existed to let two
//!   TypeScript modules share a stub. There is one implementation now.

use std::path::Path;
use std::time::{Duration, Instant};

use host_primitives::rendezvous::DaemonRendezvous;

use super::http::{base, Client};
use super::{LifecycleError, Result};

/// How long a spawned daemon gets to publish a healthy company runtime.
///
/// Overridable per invocation by `CHIEFD_START_TIMEOUT_MS`: a loaded box may
/// need a slow-but-fine daemon longer to finish init before we give up on it.
const DEFAULT_START_BUDGET: Duration = Duration::from_secs(15);
/// The cadence of the start wait.
const START_INTERVAL: Duration = Duration::from_millis(250);
/// How long a daemon asked to exit gets before the signal escalation.
const STOP_BUDGET: Duration = Duration::from_secs(5);
/// The cadence of the stop wait.
const STOP_INTERVAL: Duration = Duration::from_millis(100);
/// One health probe's budget.
const HEALTH_BUDGET: Duration = Duration::from_secs(2);

/// A single observation of a daemon's health endpoints.
///
/// Not a bare boolean: preserving WHAT the endpoint said is what lets a start
/// failure name the real reason. A 503 `{"status":"schema-missing: …"}` is a
/// daemon that is alive and answering — just not ready — not a dead or wedged
/// one, and telling an operator otherwise sends them to the wrong fix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HealthProbe {
    /// True iff HTTP 200 with body `{"status":"ok"}` — the only ready state.
    pub(crate) ok: bool,
    /// The HTTP status. `None` iff nothing answered at all.
    pub(crate) http_status: Option<u16>,
    /// The body's `status` string, or the transport error text.
    pub(crate) reason: Option<String>,
    /// The process role `/v1/docs/runtime` reported; absent means unproven.
    pub(crate) runtime_mode: Option<String>,
    /// The company a full company host is serving.
    pub(crate) company: Option<String>,
    /// How long chiefd has gone without an actuator reading this company's
    /// desired set. `None` from a daemon too old to say, which reads as "no
    /// opinion" everywhere downstream rather than as "attended".
    pub(crate) actuator_silent_ms: Option<i64>,
    /// The SERVER's verdict on that silence, against its own lapse. Not
    /// re-derived here: `chief-cli` does not depend on `chiefd-core`, so the
    /// threshold lives on the side that owns it.
    pub(crate) actuator_attended: Option<bool>,
    /// The daemon's own release version, off the health body. `None` from a
    /// daemon too old to report it (before #H.6) — read as "no opinion", never
    /// as a mismatch, so a company running since before this field existed is
    /// not stranded on attach.
    pub(crate) daemon_version: Option<String>,
}

/// What the operator surface believes about one company's daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonStatus {
    /// A live, local, identity-proven company runtime.
    Running,
    /// Something is answering at the registered URL, but it is not this
    /// company's healthy runtime.
    Unhealthy,
    /// No registration, no location columns, or a stale pid.
    Stopped,
}

/// A daemon whose ONE rendezvous reading proved it is this directory's live
/// company runtime.
///
/// Consumers bind this exact URL. They never re-read the rendezvous
/// afterwards: a daemon replaced between the proof and the use is precisely the
/// race that made a launcher operate on the wrong company.
///
/// There is deliberately no `orgs_root` beside it. Every `/v1/org/*` call needs
/// the company KEY, and the key is `company_key(dir)` — a pure function of the
/// directory the caller is standing in, which cannot go stale and cannot be
/// carried from a row that has been replaced.
#[derive(Debug, Clone)]
pub(crate) struct RunningDaemon {
    /// The proven base URL.
    pub(crate) url: String,
    /// Whether this daemon was ALREADY RUNNING and was adopted, rather than
    /// spawned by this call.
    ///
    /// Load-bearing for teardown: a caller that fails after starting a daemon
    /// may stop what it started, and must never stop what it merely found. It
    /// is also the honest signal that this company pre-existed the call — a
    /// launch that adopts a daemon is a launch of something already there.
    pub(crate) adopted: bool,
}

/// What a spawned company daemon must exec in every person's pane, as an
/// environment variable name.
///
/// The daemon reads this under the same spelling
/// (`chiefd-daemon/src/run.rs`'s `PI_BINARY_ENV`) and refuses to start without
/// it. Two crates, one string, and neither links the other — so it is asserted
/// on both sides, which is the arrangement `launch_catalog` already uses for
/// the wire it cannot import. A silent rename here is the exact defect that
/// made a configured runtime socket arrive under a name nothing read (#751/P9).
pub(crate) const PI_BINARY_ENV: &str = "CHIEFD_PI_BINARY";

/// The company DIRECTORY a spawned daemon — and every pane under it — is
/// stamped with, as an environment variable name.
///
/// **IMPORTED, never restated.** `chiefd-log` is the READER — it resolves a
/// process's jsonl root from this variable, which is what puts a daemon's own
/// log in `<dir>/.chief/log/` — and a writer that spelled the name itself would
/// be the second copy of a string the two must agree on exactly. That is the
/// defect [`PI_BINARY_ENV`]'s own doc records (#751/P9 renamed a reader and not
/// its writer, so a configured runtime socket arrived under a name nothing
/// read); `chiefd-log` is a leaf both halves may link, so here it can simply be
/// the same constant.
///
/// It has to be an ENVIRONMENT variable rather than something the daemon
/// derives from its own `--dir`, because `chiefd_log::install` runs before argv
/// is parsed — a daemon cannot stamp itself with a directory it has not read
/// yet, so its parent stamps it.
pub(crate) use chiefd_log::sink::COMPANY_DIR_ENV;

/// Probe one daemon's health and runtime identity.
///
/// A single round trip decides "healthy" from "answering but not ready" from
/// "nothing there"; a second, cheap request establishes WHAT the listener is.
/// The two are separate facts and the second is only asked when the first says
/// the listener is ready, because a not-ready daemon has no role to report.
pub(crate) async fn probe_health(client: &Client, url: &str) -> HealthProbe {
    let root = base(url);
    let Ok(answer) = client.get(&format!("{root}/v1/docs/health"), HEALTH_BUDGET).await else {
        // Nothing answered: no http_status, which is what distinguishes an
        // absent daemon from an unhealthy one everywhere downstream.
        return HealthProbe {
            ok: false,
            reason: Some("no answer".to_string()),
            ..HealthProbe::default()
        };
    };
    let status = answer.status;
    let health_body = answer.json();
    let reason = health_body
        .as_ref()
        .and_then(|body| body.get("status").and_then(|v| v.as_str().map(str::to_string)));
    // #1207. Read from the health body because that is the body this probe
    // already fetches; a second round trip to learn one number would be paid by
    // every row of `chief ls`.
    let actuator_silent_ms = health_body
        .as_ref()
        .and_then(|body| body.get("actuatorSilentMs"))
        .and_then(serde_json::Value::as_i64);
    let actuator_attended = health_body
        .as_ref()
        .and_then(|body| body.get("actuatorAttended"))
        .and_then(serde_json::Value::as_bool);
    // #H.6: the daemon's release version, off the same body. Absent from a
    // daemon too old to report it, which is a "no opinion" everywhere
    // downstream rather than a mismatch.
    let daemon_version = health_body
        .as_ref()
        .and_then(|body| body.get("version"))
        .and_then(|v| v.as_str().map(str::to_string));
    let ready = status == 200 && reason.as_deref() == Some("ok");
    if !ready {
        return HealthProbe {
            ok: false,
            http_status: Some(status),
            reason,
            actuator_silent_ms,
            actuator_attended,
            ..HealthProbe::default()
        };
    }
    let Ok(runtime) = client.get(&format!("{root}/v1/docs/runtime"), HEALTH_BUDGET).await else {
        return HealthProbe {
            ok: true,
            http_status: Some(status),
            reason,
            actuator_silent_ms,
            actuator_attended,
            ..HealthProbe::default()
        };
    };
    let body = runtime.json();
    let runtime_mode = body
        .as_ref()
        .and_then(|value| value.get("mode"))
        .and_then(|value| value.as_str())
        .filter(|mode| *mode == "company" || *mode == "docstore-only")
        .map(str::to_string);
    let company = body
        .as_ref()
        .and_then(|value| value.get("company"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    HealthProbe {
        ok: true,
        http_status: Some(status),
        reason,
        runtime_mode,
        company,
        actuator_silent_ms,
        actuator_attended,
        daemon_version,
    }
}

/// Is this listener THIS company's healthy runtime?
///
/// All three facts, never two: healthy, a company host, and this exact company
/// KEY. A healthy `docstore-only` listener, or a company host serving a
/// neighbour, is never adopted — that adoption is how a launcher ends up
/// writing one company's state into another's database.
///
/// The key and not a slug, because a slug is a display word two directories may
/// share: two companies both called `acme` would have proven each other's
/// identity under the old comparison.
#[must_use]
pub(crate) fn is_expected_company_runtime(probe: &HealthProbe, key: &str) -> bool {
    probe.ok
        && probe.runtime_mode.as_deref() == Some("company")
        && probe.company.as_deref() == Some(key)
}

/// This client's own release version, for the version-skew check.
fn client_version() -> &'static str {
    env!("CHIEF_VERSION")
}

/// `major.minor` of a `major.minor.patch` version, if it parses.
fn major_minor(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether a RUNNING daemon is too far from this client to drive (#H.6.2).
///
/// A PATCH difference is compatible, and that is the common case: releases are
/// cut per green commit, so right after `chief upgrade` the client is usually
/// one patch ahead of a daemon that has been up since before the swap, and it
/// must keep driving it — the wire did not change. A MAJOR or MINOR difference
/// is a wire the client cannot assume, so it refuses and names the restart.
///
/// An ABSENT or unparseable daemon version is "no opinion", never a mismatch: a
/// daemon from before this field existed must not be stranded on attach, and a
/// version string neither side can read is not evidence of skew. This is the
/// same "too old to say → no opinion" rule the actuator-silence field takes.
///
/// Returns `Some((daemon, client))` — the two versions, for the refusal — when
/// they are major/minor-incompatible; `None` when the daemon may be driven.
pub(crate) fn incompatible_daemon(
    daemon_version: Option<&str>,
    client_version: &str,
) -> Option<(String, String)> {
    let daemon = daemon_version?;
    let (daemon_major, daemon_minor) = major_minor(daemon)?;
    let (client_major, client_minor) = major_minor(client_version)?;
    ((daemon_major, daemon_minor) != (client_major, client_minor))
        .then(|| (daemon.to_owned(), client_version.to_owned()))
}

/// A stale daemon's restart order: what to print, and both identities for the
/// log line.
struct StaleDaemon {
    /// The operator-facing sentence, printed before anything is stopped.
    line: String,
    /// What the daemon reported it is running.
    running: host_primitives::rendezvous::BuildIdentity,
    /// What the install resolves to now.
    installed: host_primitives::rendezvous::BuildIdentity,
}

/// Should this adopted daemon be replaced because it is not the installed
/// build?
///
/// `None` for every answer that is not a proven mismatch — current, a
/// development build, a daemon from before identity reporting, a broken
/// install. **Unknowable is never treated as stale**: stopping a live daemon
/// on a question nobody answered would take a working company down to satisfy
/// a check, which is a worse outcome than the staleness it is looking for.
fn stale_daemon_restart(published: &DaemonRendezvous) -> Option<StaleDaemon> {
    let installed_path = super::paths::chiefd_daemon_binary(&std::env::current_exe().ok()?);
    match super::build_identity::check(
        super::DAEMON_PROGRAM,
        published.build.as_ref(),
        &installed_path,
    ) {
        super::build_identity::BuildCheck::Current => None,
        super::build_identity::BuildCheck::Unknowable { reason } => {
            // SAID OUT LOUD, ONCE PER START, AND NEVER SILENTLY PASSED. The
            // incident this rule exists for was 41 minutes of silence, so a
            // check that cannot answer says so rather than saying nothing.
            tracing::warn!(
                event = "daemon.build.unknowable",
                pid = published.pid,
                reason = %reason,
                "this daemon's build could not be compared against the installed one"
            );
            None
        }
        super::build_identity::BuildCheck::Stale { running, installed } => {
            // A `Stale` answer is only reachable through a report, so the
            // reported path is present; the installed path is the honest
            // fallback rather than an `unwrap` that cannot be proven here.
            let exe = published
                .build
                .as_ref()
                .map_or_else(|| installed_path.clone(), |build| build.exe.clone());
            Some(StaleDaemon {
                line: super::build_identity::stale_line(
                    super::DAEMON_PROGRAM,
                    running,
                    installed,
                    &exe,
                ),
                running,
                installed,
            })
        }
    }
}

/// After ONE restart, is the fresh daemon still not the installed build?
///
/// Returns the refusal to hand back, or `None` when the restart took. This is
/// the whole loop floor: it is asked once, by the branch that just spawned,
/// and nothing retries on its answer.
fn refuse_if_still_stale(published: &DaemonRendezvous) -> Option<LifecycleError> {
    let stale = stale_daemon_restart(published)?;
    let installed_path = std::env::current_exe()
        .map(|exe| super::paths::chiefd_daemon_binary(&exe))
        .unwrap_or_default();
    let running_exe =
        published.build.as_ref().map_or_else(|| installed_path.clone(), |build| build.exe.clone());
    tracing::error!(
        event = "daemon.build.stale-after-restart",
        running = %stale.running,
        installed = %stale.installed,
        "the daemon this call just spawned still reports a different build; refusing rather than \
         restarting again"
    );
    Some(LifecycleError::refused(super::build_identity::refusal_after_one_attempt(
        super::DAEMON_PROGRAM,
        &running_exe,
        &installed_path,
    )))
}

/// The refusal a client gives rather than driving a version-skewed daemon.
fn version_skew_refusal(dir: &Path, daemon: &str, client: &str) -> LifecycleError {
    LifecycleError::refused(format!(
        "the company in {} is running chiefd {daemon}, but this chief is {client}. Restart it to \
         pick up the new version — run 'chief stop && chief attach' in that directory when \
         convenient; the company keeps running on {daemon} until you do.",
        dir.display()
    ))
}

/// Human prose for whatever was observed, for a refusal that names it.
fn observed(probe: &HealthProbe) -> String {
    if !probe.ok {
        return match probe.http_status {
            Some(status) => match &probe.reason {
                Some(reason) => format!("an unhealthy listener answering {status} ({reason})"),
                None => format!("an unhealthy listener answering {status}"),
            },
            None => "an unreachable listener".to_string(),
        };
    }
    match probe.runtime_mode.as_deref() {
        None => "an unproven listener".to_string(),
        Some("company") => {
            format!("company '{}'", probe.company.as_deref().unwrap_or("<unknown>"))
        }
        Some(other) => other.to_string(),
    }
}

/// The refusal a wrong-identity listener earns.
fn unexpected_runtime(dir: &Path, probe: &HealthProbe) -> LifecycleError {
    LifecycleError::refused(format!(
        "chiefd for {} will not adopt {} as the company runtime; this directory's company \
         requires chiefd run --dir {}",
        dir.display(),
        observed(probe),
        dir.display()
    ))
}

/// A rendezvous this directory's daemon published, and what it is worth.
///
/// The four fields are read in one pass and none of them is re-read afterwards:
/// a daemon replaced between the reading and the use is the race the whole
/// shape exists to remove.
struct Observation {
    /// A rendezvous that decoded AND names this directory. A file describing
    /// somewhere else — the copied-project case — is not this company's.
    published: Option<DaemonRendezvous>,
    /// Whether the published pid still exists.
    pid_alive: bool,
    /// The health probe, asked only of a live pid's URL.
    probe: Option<HealthProbe>,
}

/// Read the rendezvous and climb the ladder, before any caller binds its URL.
///
/// A file that cannot be read, cannot be decoded, or names another directory is
/// simply "no daemon here". None of the three is an error worth surfacing: the
/// answer to all of them is the same spawn, and a refusal would turn an
/// ordinary stale pointer into an operator-facing failure.
async fn observe(client: &Client, dir: &Path) -> Observation {
    let Some(published) = read_rendezvous(dir) else {
        return Observation { published: None, pid_alive: false, probe: None };
    };
    let pid_alive = beacond::liveness::pid_is_live(i64::from(published.pid));
    // A dead pid's URL is not probed: whatever answers there now belongs to
    // some other process that has taken the port, which is exactly the
    // adoption this ladder exists to refuse.
    let probe = if pid_alive { Some(probe_health(client, &published.url).await) } else { None };
    Observation { published: Some(published), pid_alive, probe }
}

/// This directory's published rendezvous, or `None`.
///
/// `describes` is the load-bearing check and not a formality: `.chief/` lives
/// INSIDE the company directory, so copying a project copies its rendezvous —
/// and without this the copy would point its client at the ORIGINAL's daemon.
pub(crate) fn read_rendezvous(dir: &Path) -> Option<DaemonRendezvous> {
    let body = std::fs::read_to_string(super::paths::daemon_rendezvous_path(dir)).ok()?;
    let published: DaemonRendezvous = serde_json::from_str(&body).ok()?;
    published.describes(dir).then_some(published)
}

/// One currently live, healthy, identity-proven company URL — or `None`.
pub(crate) async fn resolve_running(client: &Client, dir: &Path) -> Option<RunningDaemon> {
    let observation = observe(client, dir).await;
    let (Some(published), Some(probe)) = (&observation.published, observation.probe.as_ref())
    else {
        return None;
    };
    if !observation.pid_alive || !is_expected_company_runtime(probe, &published.key) {
        return None;
    }
    Some(RunningDaemon { url: published.url.clone(), adopted: true })
}

/// The rendezvous establishes WHERE a daemon is; `/v1/docs/runtime` proves WHAT
/// it is.
pub(crate) async fn status(client: &Client, dir: &Path) -> DaemonStatus {
    let observation = observe(client, dir).await;
    let Some(published) = &observation.published else {
        return DaemonStatus::Stopped;
    };
    if !observation.pid_alive {
        // A stale pointer left by a killed daemon. Not `Unhealthy`: nothing is
        // there to be unhealthy, and telling an operator to stop a process that
        // does not exist sends them nowhere.
        return DaemonStatus::Stopped;
    }
    match observation.probe.as_ref() {
        Some(probe) if is_expected_company_runtime(probe, &published.key) => DaemonStatus::Running,
        _ => DaemonStatus::Unhealthy,
    }
}

/// Start (or adopt) this directory's daemon, and return its proven URL.
///
/// The sequence. Every rung is a refusal somebody earned, and only the SOURCE
/// of the location changed when beacond left this path:
///
/// 1. **The rendezvous is the pointer, and it must describe THIS directory.**
///    No file, an undecodable one, or one naming somewhere else (the copied
///    project) all mean the same thing — nothing is published here — and all
///    three spawn.
/// 2. A published pid that is gone is a STALE pointer. It spawns; the new
///    daemon overwrites the file. Nothing is deleted first — a rendezvous is
///    disposable and the next publish is the repair.
/// 3. A live pid is ADOPTED only after its listener proves it is healthy AND
///    serving this directory's key. A live listener that is the wrong thing is
///    a refusal, not a restart: restarting it would tear down a daemon this
///    command was never asked about.
/// 4. A spawn starts only after beacond answers. Every company daemon must ask
///    beacond for single-writer admission before it opens storage, so spawning
///    while beacond is down can only produce a child that exits before it
///    publishes this rendezvous.
///
/// # Errors
/// [`LifecycleError`] naming the exact refusal, including the daemon log tail
/// when a child died before becoming healthy.
#[tracing::instrument(name = "daemon.start", skip_all, fields(company = %dir.display()))]
pub(crate) async fn start(
    client: &Client,
    home: &Path,
    dir: &Path,
    runtime_socket: &super::company::BootSocketRequest,
) -> Result<RunningDaemon> {
    let discovery = super::discovery::Discovery::from_env();
    start_after_admission(
        client,
        dir,
        runtime_socket,
        super::discovery::ensure_running(&discovery, home),
    )
    .await
}

/// Run the daemon start ladder with its admission precondition supplied.
///
/// The future is injected only to make the spawn boundary testable without
/// starting a real process. Production always supplies
/// [`super::discovery::ensure_running`]. It is awaited only when no healthy
/// daemon can be adopted, and always before [`spawn_daemon`].
async fn start_after_admission<F>(
    client: &Client,
    dir: &Path,
    runtime_socket: &super::company::BootSocketRequest,
    admission: F,
) -> Result<RunningDaemon>
where
    F: std::future::Future<Output = Result<()>>,
{
    let key = super::paths::company_key(dir);
    let observation = observe(client, dir).await;
    if let (Some(published), true) = (&observation.published, observation.pid_alive) {
        let probe = observation.probe.clone().unwrap_or_default();
        if !is_expected_company_runtime(&probe, &key) {
            return Err(unexpected_runtime(dir, &probe));
        }
        // #H.6.2: refuse to DRIVE a daemon whose major/minor differs from this
        // client. Only the ADOPT path checks this — a daemon this call spawned
        // is always the current binary, and `chief ls` observes without
        // driving, so neither refuses. A patch difference and an unversioned
        // (pre-#H.6) daemon both pass: see `incompatible_daemon`.
        if let Some((daemon, client)) =
            incompatible_daemon(probe.daemon_version.as_deref(), client_version())
        {
            return Err(version_skew_refusal(dir, &daemon, &client));
        }
        // #1281: IS THIS THE BUILD THAT IS INSTALLED RIGHT NOW?
        //
        // The skew guard above REFUSES a daemon whose declared version differs
        // from this client. It cannot see the operator's case at all: a
        // `0.5.0` -> `0.5.0` rebuild declares the identical version and is a
        // different binary, so the guard passes it and the fix stays installed
        // and not in effect. This asks the question the version string cannot
        // answer, and unlike the guard it REPLACES rather than refuses.
        //
        // Only on the ADOPT path, exactly like the guard: a daemon this call
        // spawned is the installed binary by construction.
        if let Some(restart) = stale_daemon_restart(published) {
            eprintln!("{}", restart.line);
            tracing::warn!(
                event = "daemon.build.stale",
                pid = published.pid,
                running = %restart.running,
                installed = %restart.installed,
                "this daemon is not the installed build; stopping it so the ladder spawns the                  installed one"
            );
            // `daemon::stop`, and NEVER the `chief stop` teardown. That path is
            // eight steps with `kill-actuator` and `kill-session` ahead of
            // `stop-daemon`: driving it here would stop every person in the
            // company to update a binary, and would override an operator's own
            // wake click. This verb touches nothing durable and no tmux object.
            stop(client, dir).await?;
            // Fall through to the spawn below. That IS the one attempt: no
            // loop, and the floor beneath it is checked once the fresh daemon
            // publishes (see `refuse_if_still_stale`).
        } else {
            // Found running, not started here.
            tracing::info!(
                event = "daemon.adopted",
                url = %published.url,
                pid = published.pid,
                "the daemon this directory published was adopted; nothing was spawned"
            );
            return Ok(RunningDaemon { url: published.url.clone(), adopted: true });
        }
    }
    if observation.published.is_some() {
        tracing::info!(
            event = "daemon.rendezvous.stale",
            "this directory's rendezvous names a pid that is gone; spawning over it"
        );
    }

    // THE OPERATOR'S INCIDENT, 2026-08-17: a release stopped the box-wide beacond, then bare
    // `chief` in an existing company came through attach and reached this
    // spawn directly. Chiefd reserved 8792, failed its first `/v1/register`
    // call to 6969, exited, and could not publish `daemon.json`. Beacond is not
    // optional startup decoration: it is the single-writer admission service
    // this child calls before it may open company storage.
    admission.await?;

    let log_path = super::paths::daemon_log_path(dir);
    let mut child = spawn_daemon(dir, runtime_socket, &log_path)?;
    let pid = i64::from(child.id());

    let start_budget = start_budget();
    let deadline = Instant::now() + start_budget;
    // THE WAIT THE INCIDENT COULD NOT SEE. `chiefd_launch_company` sat here
    // for minutes with nothing on stdout and nothing on disk, and beacond's
    // own registration landed three seconds before the tool returned — which
    // is a fact that had to be reconstructed from a pi session transcript
    // because neither side of the wait wrote it down. Every pass now says how
    // many attempts it has made, how long it has been waiting, and what it is
    // waiting FOR.
    let waiting_since = Instant::now();
    let mut attempt: u64 = 0;
    tracing::info!(
        event = "daemon.rendezvous.wait.start",
        pid,
        budget_ms = chiefd_log::duration_ms(start_budget),
        interval_ms = chiefd_log::duration_ms(START_INTERVAL),
        log_path = %log_path.display(),
        "waiting for the spawned daemon to publish its rendezvous and answer healthy"
    );
    loop {
        attempt += 1;
        // REAP, NEVER `kill(pid, 0)`. This child is OURS — `detach` gives it
        // its own process GROUP, not a new parent — so when it exits it becomes
        // a ZOMBIE until this process waits on it, and a zombie answers
        // `kill(pid, 0)` exactly as a running process does. That is why the
        // branch below, which has promised the daemon's own log tail since it
        // was written, had never once fired for a spawned child: it asked a
        // question that cannot return "gone" for a process nobody has reaped.
        //
        // Measured, and it is the whole outage: chiefd exited at +1.3 ms with a
        // precise refusal already written to `daemon.log`, this loop polled the
        // rendezvous 61 times over 15 s, and the operator was then shown a
        // guessed cause whose remedy was wrong. `try_wait` reaps and answers
        // from the KERNEL's status, so a dead child now fails in one pass with
        // the exit status and the words the daemon itself wrote.
        // `Some(status)` is "exited, and here is why"; `Some(None)` is
        // "exited, status unreadable"; `None` is "still running".
        let exited: Option<Option<std::process::ExitStatus>> = match child.try_wait() {
            Ok(Some(status)) => Some(Some(status)),
            Ok(None) => None,
            // `try_wait` failing proves nothing either way, so fall back to the
            // liveness probe rather than treating an unreadable status as a
            // healthy child and burning the whole budget on it.
            Err(error) => {
                tracing::warn!(
                    pid,
                    %error,
                    "could not read the spawned daemon's exit status; falling back to a liveness probe"
                );
                if beacond::liveness::pid_is_live(pid) {
                    None
                } else {
                    Some(None)
                }
            }
        };
        if let Some(status) = exited {
            tracing::error!(
                event = "daemon.child.exited",
                pid,
                attempt,
                waited_ms = chiefd_log::elapsed_ms(waiting_since),
                exit_status = status.map(|status| status.to_string()).unwrap_or_default(),
                "the spawned daemon exited before it published its rendezvous"
            );
            return Err(LifecycleError::host(child_exited_message(dir, pid, &log_path, status)));
        }
        // The child chooses its own port and publishes it only once its
        // listener is answerable. Never inspect a URL until the rendezvous
        // names THIS exact spawned pid: the stale file this spawn is racing
        // against carries the previous daemon's URL, and probing it would
        // adopt a corpse.
        //
        // Whatever this pass observed is what a timeout would report, so the
        // observation is this iteration's own value: every arm produces one,
        // and none of them outlives the pass that made it.
        let last = match read_rendezvous(dir) {
            Some(published) if i64::from(published.pid) == pid => {
                let probe = probe_health(client, &published.url).await;
                if is_expected_company_runtime(&probe, &key) {
                    tracing::info!(
                        event = "daemon.rendezvous.wait.done",
                        pid,
                        attempt,
                        waited_ms = chiefd_log::elapsed_ms(waiting_since),
                        url = %published.url,
                        "the spawned daemon published its rendezvous and answered healthy"
                    );
                    // #1281 THE LOOP FLOOR. The daemon we just spawned is the
                    // installed binary by construction — unless the install
                    // itself is inconsistent (a `bin/` symlink into a stale
                    // version directory, two install roots). Ask once. A rule
                    // that restarts on mismatch and never checks the result is
                    // a rule that can restart for ever.
                    if let Some(refusal) = refuse_if_still_stale(&published) {
                        return Err(refusal);
                    }
                    // Spawned by this call: this one IS ours to stop.
                    return Ok(RunningDaemon { url: published.url, adopted: false });
                }
                if probe.ok && probe.runtime_mode.is_some() {
                    // A healthy listener that proved the WRONG role will never
                    // become the right one. Stop it and refuse now rather than
                    // burning the whole budget on a decided outcome.
                    tracing::error!(
                        event = "daemon.rendezvous.wrong-runtime",
                        pid,
                        attempt,
                        waited_ms = chiefd_log::elapsed_ms(waiting_since),
                        runtime_mode = probe.runtime_mode.as_deref().unwrap_or(""),
                        "the published listener proved a role this company did not ask for"
                    );
                    signal(pid, nix::sys::signal::Signal::SIGTERM);
                    return Err(unexpected_runtime(dir, &probe));
                }
                probe
            }
            other => {
                let observed_pid =
                    other.map_or_else(|| "none".to_string(), |published| published.pid.to_string());
                HealthProbe {
                    reason: Some(format!(
                        "waiting for the daemon rendezvous of pid {pid} (observed pid \
                         {observed_pid})"
                    )),
                    ..HealthProbe::default()
                }
            }
        };
        // One line per pass, carrying the attempt number, how long the wait
        // has run and the reason THIS pass produced. A loop that can burn a
        // whole budget silently is exactly what made the incident
        // unanswerable.
        tracing::info!(
            event = "daemon.rendezvous.wait",
            pid,
            attempt,
            waited_ms = chiefd_log::elapsed_ms(waiting_since),
            budget_ms = chiefd_log::duration_ms(start_budget),
            backoff_ms = chiefd_log::duration_ms(START_INTERVAL),
            http_status = last.http_status.unwrap_or_default(),
            reason = last.reason.as_deref().unwrap_or(""),
            "still waiting for the spawned daemon"
        );
        if Instant::now() >= deadline {
            tracing::error!(
                event = "daemon.rendezvous.timeout",
                pid,
                attempt,
                waited_ms = chiefd_log::elapsed_ms(waiting_since),
                budget_ms = chiefd_log::duration_ms(start_budget),
                reason = last.reason.as_deref().unwrap_or(""),
                "the spawned daemon never became healthy inside its budget"
            );
            return Err(terminate_after_timeout(dir, pid, start_budget, &last, &log_path).await);
        }
        // os-liveness: no push channel exists for "the daemon I just forked
        // registered and became healthy yet". Bounded by `start_budget` above.
        #[allow(clippy::disallowed_methods)]
        tokio::time::sleep(START_INTERVAL).await;
    }
}

/// Stop this directory's daemon, judged from its own rendezvous.
///
/// Idempotent: nothing published is nothing to stop, and neither is a pointer
/// whose pid is gone. Nothing durable is touched — stopping a daemon is not
/// removing a company, and the two must not be reachable from one another.
///
/// The rendezvous file is deliberately NOT deleted here. A gracefully stopped
/// daemon removes its own; a killed one leaves a pointer that the next
/// [`start`] proves stale in one `kill(pid, 0)` and overwrites. A client that
/// tidied it would be asserting a process is dead at the one moment it cannot
/// know that.
///
/// # Errors
/// Nothing here refuses today; the signature keeps the caller's error channel
/// so a future rung can.
pub(crate) async fn stop(client: &Client, dir: &Path) -> Result<()> {
    let Some(published) = read_rendezvous(dir) else {
        return Ok(());
    };
    let pid = i64::from(published.pid);
    if !beacond::liveness::pid_is_live(pid) {
        return Ok(());
    }

    // Ask, then judge by the PROCESS — never by this call's own response. A
    // daemon that answered the shutdown and then wedged mid-unwind is exactly
    // the case a 200 would hide.
    let shutdown = format!("{}/v1/admin/shutdown", base(&published.url));
    let _ = client
        .post_json(&shutdown, &serde_json::json!({ "reason": "operator stop" }), HEALTH_BUDGET)
        .await;

    let waiting_since = Instant::now();
    let deadline = Instant::now() + STOP_BUDGET;
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        if !beacond::liveness::pid_is_live(pid) {
            tracing::info!(
                event = "daemon.stopped",
                company = %dir.display(),
                attempt,
                waited_ms = chiefd_log::elapsed_ms(waiting_since),
                "the daemon exited after being asked to"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tracing::info!(
            event = "daemon.stop.wait",
            company = %dir.display(),
            attempt,
            waited_ms = chiefd_log::elapsed_ms(waiting_since),
            budget_ms = chiefd_log::duration_ms(STOP_BUDGET),
            backoff_ms = chiefd_log::duration_ms(STOP_INTERVAL),
            "still waiting for the daemon to exit"
        );
        // os-liveness: no push channel exists for "the process I asked to exit
        // actually exited yet". Bounded by STOP_BUDGET above.
        #[allow(clippy::disallowed_methods)]
        tokio::time::sleep(STOP_INTERVAL).await;
    }

    // Escalation only, never the first move. The pid came out of a file inside
    // this company's own directory, so it is a local pid by construction.
    tracing::warn!(
        event = "daemon.stop.escalated",
        company = %dir.display(),
        pid,
        waited_ms = chiefd_log::elapsed_ms(waiting_since),
        "the daemon did not exit inside its budget; escalating to SIGKILL"
    );
    signal(pid, nix::sys::signal::Signal::SIGKILL);
    Ok(())
}

/// Spawn `chiefd run --dir <dir>` detached, with its own log.
///
/// The DAEMON binary, not this one. Before P6 the two were the same
/// executable and this was a re-invocation of `current_exe()`; they are now
/// separate programs and the client links none of the daemon's crates, so the
/// program name is a fact this module has to get right rather than inherit.
fn spawn_daemon(
    dir: &Path,
    runtime_socket: &super::company::BootSocketRequest,
    log_path: &Path,
) -> Result<std::process::Child> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            LifecycleError::host(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    // The log is diagnostics, not state (see `paths::daemon_log_path`); opening
    // it is the one file handle this module needs and it is opened here, at the
    // single spawn site, rather than anywhere a caller could reach.
    #[allow(clippy::disallowed_types)]
    let log =
        std::fs::OpenOptions::new().create(true).append(true).open(log_path).map_err(|error| {
            LifecycleError::host(format!("cannot open {}: {error}", log_path.display()))
        })?;
    let stderr = log.try_clone().map_err(|error| {
        LifecycleError::host(format!("cannot duplicate the daemon log handle: {error}"))
    })?;

    // What panes must exec, resolved in THIS process — the one whose
    // environment the preflight actually measured. See
    // `preflight::pi_binary_for_daemon` for the defect that makes this a
    // spawn-time argument rather than something the daemon looks up.
    let pi_binary = super::preflight::pi_binary_for_daemon()?;

    let client_executable = std::env::current_exe().map_err(|error| {
        LifecycleError::host(format!(
            "chief cannot locate its own executable, so it cannot find {}: {error}",
            super::DAEMON_PROGRAM
        ))
    })?;
    let binary = super::paths::chiefd_daemon_binary(&client_executable);
    let mut command = std::process::Command::new(&binary);
    command
        .arg("run")
        .arg("--dir")
        .arg(dir)
        // The child's OWN working directory is the company's, so anything it
        // spawns inherits it rather than this command's cwd.
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(stderr))
        // THE DAEMON CANNOT STAMP ITSELF. `chiefd_log::install` resolves its
        // sink root before argv is parsed, so a daemon told `--dir` on the
        // command line still would not know where to put its own jsonl. Its
        // parent knows, and says so here — which is why this is an environment
        // variable and not a second flag.
        .env(COMPANY_DIR_ENV, dir)
        // THE PREFERENCE, never a demand. This client cannot read the
        // company's runtime-ownership claim before this daemon serves it, so
        // this value is a guess and chiefd treats it as one: a live claim
        // outranks it. Only the operator's own override below is a demand
        // worth refusing a boot over. See `company::BootSocketRequest`.
        .env("ORG_LAUNCHER_RUNTIME_SOCKET", &runtime_socket.preferred)
        .env(PI_BINARY_ENV, &pi_binary)
        // A bind belongs to THIS invocation only. Carrying a parent's value
        // forward would silently defeat chiefd's own port walk.
        .env_remove("CHIEFD_STORE_BIND");
    if let Some(demanded) = &runtime_socket.demanded {
        command.arg("--runtime-socket").arg(demanded);
    }
    detach(&mut command);
    let child = command.spawn().map_err(|error| {
        LifecycleError::host(format!(
            "could not start the chiefd paired with this chief at {}: {error}\nInstall both binaries with: bun run release",
            binary.display()
        ))
    })?;
    // The pid, the binary and the log the child's own output goes to. Those
    // three are what an investigator needs to cross the process boundary, and
    // reconstructing them from `ps` on a box hours later is how the incident
    // was diagnosed the first time.
    tracing::info!(
        event = "daemon.spawned",
        company = %dir.display(),
        pid = i64::from(child.id()),
        binary = %binary.display(),
        pi_binary = %pi_binary.display(),
        log_path = %log_path.display(),
        "spawned the company daemon"
    );
    Ok(child)
}

/// Put the spawned daemon in its own process group so it outlives this command.
///
/// `process_group(0)`, not a `setsid` EXECUTABLE: macOS does not ship the GNU
/// `setsid` utility, so asking PATH for it makes a normal installed ChiefD
/// unusable before its first company can start. It is also not `pre_exec`,
/// which would need `unsafe` in a crate that forbids it — and the property that
/// actually matters here is the one `process_group` gives: a terminal hangup
/// signals its FOREGROUND process group, and the daemon is no longer in it.
pub(crate) fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

/// Signal a pid, treating "already gone" as success.
fn signal(pid: i64, signal: nix::sys::signal::Signal) {
    if let Ok(raw) = i32::try_from(pid) {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), signal);
    }
}

/// The start budget for this invocation.
fn start_budget() -> Duration {
    std::env::var("CHIEFD_START_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(DEFAULT_START_BUDGET, Duration::from_millis)
}

/// The last non-empty lines of the daemon log, for a start-failure message.
fn log_tail(log_path: &Path, lines: usize) -> Option<String> {
    let text = std::fs::read_to_string(log_path).ok()?;
    let non_empty: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return None;
    }
    Some(non_empty[non_empty.len().saturating_sub(lines)..].join("\n"))
}

/// Diagnose a child that died before it published a healthy company runtime.
fn child_exited_message(
    dir: &Path,
    pid: i64,
    log_path: &Path,
    status: Option<std::process::ExitStatus>,
) -> String {
    // KNOWN FACTS FIRST, and no hypothesis at all. A child that has exited has
    // already said why in its own log; a diagnostic that guesses over the top
    // of that is worse than one that says "the log names it" — which, during
    // the outage, it did, and nobody was shown it.
    let mut message =
        format!("chiefd for {} (pid {pid}) exited before becoming healthy", dir.display());
    if let Some(status) = status {
        match status.code() {
            Some(code) => message.push_str(&format!(" (exit status {code})")),
            None => message.push_str(&format!(" ({status})")),
        }
    }
    match log_tail(log_path, 10) {
        Some(tail) => {
            message.push_str(&format!(
                "; it wrote this before exiting:\n{tail}\nsee full log at {}",
                log_path.display()
            ));
        }
        None => message.push_str(&format!("; see full log at {}", log_path.display())),
    }
    message
}

/// Diagnose a start that ran out its readiness deadline.
///
/// If the daemon is still answering, the message says it may just be slow to
/// initialize and names the knob — not that it is broken. That distinction is
/// the difference between waiting a bit longer and deleting a database.
fn timed_out_message(
    dir: &Path,
    pid: i64,
    budget: Duration,
    last: &HealthProbe,
    log_path: &Path,
) -> String {
    // REACHING HERE NOW PROVES THE CHILD IS ALIVE. Every pass of the wait loop
    // reaps the child first, so an exited daemon leaves through
    // `child_exited_message` in one pass and can no longer arrive here at all.
    // That is what makes the hypothesis at the bottom of this message legitimate
    // — before the reap it was offered to five different causes, four of which
    // it was wrong for, because "exited immediately" and "running but silent"
    // were indistinguishable from a `kill(pid, 0)` that answers for zombies.
    let mut message = format!(
        "chiefd for {} (pid {pid}) was still running but had not become healthy after {budget:?}",
        dir.display()
    );
    // WHAT IS KNOWN, BEFORE ANYTHING THAT IS GUESSED. The daemon's own words
    // outrank this client's reading of them, and during the outage the log
    // named the cause exactly while the operator was shown a hypothesis instead.
    if let Some(tail) = log_tail(log_path, 10) {
        message.push_str(&format!("\nthe daemon's own log says:\n{tail}"));
    }
    if let Some(status) = last.http_status {
        let reason = last.reason.as_ref().map_or_else(String::new, |r| format!(" \"{r}\""));
        message.push_str(&format!(
            "\nits health endpoint is still answering {status}{reason}, so it may just be slow to initialize (raise CHIEFD_START_TIMEOUT_MS)"
        ));
    } else if let Some(reason) = last.reason.as_ref() {
        message.push_str(&format!("\nlast readiness observation: {reason}"));
        // A daemon that runs happily and publishes NOTHING is the one symptom a
        // stale install produces, and it produces it perfectly. The ancestor of
        // this sentence was written for the registry era — an installed chiefd
        // built before beacond moved to its static port started fine,
        // registered at the old address and was never seen (observed live: a
        // binary carrying `127.0.0.1:8790` twice and the current port zero
        // times). The rendezvous inherits the shape exactly: a chiefd built
        // before the rendezvous existed writes no `daemon.json` at all, so this
        // wait can only ever end in its budget.
        //
        // Offered as ONE possibility among named others, never as the answer.
        // It is the most common cause of this exact shape and it is still a
        // guess, and a diagnostic that asserts a guess sends the reader to
        // reinstall a binary that was never the problem.
        if reason.contains("daemon rendezvous") {
            message.push_str(
                "\nthe log above is the first thing to read. If it is empty or says nothing \
                 about why, a chiefd that runs but never publishes its rendezvous is most often \
                 an INSTALLED binary older than the contract this client reads — it starts fine \
                 and is never found. Other causes with this same shape: storage it cannot open, \
                 a port walk that found nothing free, or an admission call that is still \
                 blocked. Reinstall chiefd from this build only if the log does not name one \
                 of those",
            );
        }
    }
    message.push_str(&format!("\nsee full log at {}", log_path.display()));
    message
}

/// Grace before force: give the daemon a chance to unwind an in-flight init
/// cleanly, and only `SIGKILL` if it ignores the `SIGTERM`.
async fn terminate_after_timeout(
    dir: &Path,
    pid: i64,
    budget: Duration,
    last: &HealthProbe,
    log_path: &Path,
) -> LifecycleError {
    signal(pid, nix::sys::signal::Signal::SIGTERM);
    let deadline = Instant::now() + STOP_BUDGET;
    while beacond::liveness::pid_is_live(pid) && Instant::now() < deadline {
        // os-liveness: waiting for a SIGTERM to land. Bounded by STOP_BUDGET.
        #[allow(clippy::disallowed_methods)]
        tokio::time::sleep(STOP_INTERVAL).await;
    }
    if beacond::liveness::pid_is_live(pid) {
        signal(pid, nix::sys::signal::Signal::SIGKILL);
    }
    LifecycleError::host(timed_out_message(dir, pid, budget, last, log_path))
}

#[cfg(test)]
mod tests {
    use super::{
        child_exited_message, incompatible_daemon, is_expected_company_runtime, observed,
        read_rendezvous, stale_daemon_restart, start_after_admission, timed_out_message,
        HealthProbe,
    };
    use host_primitives::rendezvous::DaemonRendezvous;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn healthy(company: &str) -> HealthProbe {
        HealthProbe {
            ok: true,
            http_status: Some(200),
            reason: Some("ok".to_string()),
            runtime_mode: Some("company".to_string()),
            company: Some(company.to_string()),
            // A daemon that says nothing about the actuator, which is what
            // every one of these fixtures is about: adoption is decided by
            // health, mode and company, and #1207's fact changes none of that.
            actuator_silent_ms: None,
            actuator_attended: None,
            // No version reported: these fixtures predate the skew check by
            // design, so adoption is decided by health, mode and company alone
            // — a version-aware fixture would have to match this client's, and
            // that coupling is exactly what the dedicated skew tests below own.
            daemon_version: None,
        }
    }

    /// THE OPERATOR'S EXACT FAILURE: after a release stopped beacond, bare `chief`
    /// spawned `chiefd run` first. Chiefd reserved port 8792, could not register
    /// with beacond on 6969, exited, and therefore never wrote `daemon.json`.
    /// A failed admission precondition must stop before the daemon log or child
    /// can exist.
    #[tokio::test]
    async fn a_company_daemon_is_never_spawned_before_beacond_is_ready() {
        let company = tempfile::tempdir().expect("company");
        let client = super::super::http::Client::new();
        let unavailable = std::future::ready(Err(super::super::LifecycleError::unreachable(
            "beacond is unavailable",
        )));

        let error = start_after_admission(
            &client,
            company.path(),
            &super::super::company::BootSocketRequest {
                demanded: None,
                preferred: "default".to_owned(),
            },
            unavailable,
        )
        .await
        .expect_err("beacond failure must refuse before a company daemon is spawned");

        assert!(error.to_string().contains("beacond is unavailable"), "{error}");
        assert!(
            !super::super::paths::daemon_log_path(company.path()).exists(),
            "no child log means the chiefd spawn boundary was not crossed"
        );
        assert!(
            !super::super::paths::daemon_rendezvous_path(company.path()).exists(),
            "no failed child can leave a rendezvous"
        );
    }

    /// Write a rendezvous into `dir`, describing `describes`.
    ///
    /// The seam rule that bans `std::fs::write` is about production filesystem
    /// effects belonging to a host transaction; there is no host transaction in
    /// a unit test, and what is under test is what the READER does with real
    /// bytes on disk.
    #[allow(clippy::disallowed_methods)]
    fn publish(dir: &Path, describes: &Path, pid: u32) {
        let path = super::super::paths::daemon_rendezvous_path(dir);
        std::fs::create_dir_all(path.parent().expect("run dir")).expect("run dir");
        let body = serde_json::to_string(&DaemonRendezvous {
            dir: describes.to_path_buf(),
            key: "0123456789ab".to_owned(),
            url: "http://127.0.0.1:8793".to_owned(),
            pid,
            // A rendezvous from the bootstrap generation: no build reported,
            // which the reader must treat as unknowable and adopt.
            build: None,
        })
        .expect("serialize");
        std::fs::write(&path, body).expect("publish");
    }

    /// AN UNANSWERABLE QUESTION NEVER STOPS A LIVE DAEMON.
    ///
    /// The bootstrap generation: a daemon started by a build that predates
    /// build-identity reporting publishes no `build`, so nothing can be
    /// compared. That must ADOPT, exactly as before this rule existed.
    /// Treating "I do not know" as "stale" would take a working company down
    /// on every start until its daemon happened to be restarted for some other
    /// reason — a check doing more damage than the staleness it hunts.
    #[test]
    fn a_daemon_that_reports_no_build_is_adopted_rather_than_stopped() {
        let published = DaemonRendezvous {
            dir: PathBuf::from("/work/anvils"),
            key: "aaaaaaaaaaaa".to_owned(),
            url: "http://127.0.0.1:8793".to_owned(),
            pid: 4242,
            build: None,
        };
        assert!(
            stale_daemon_restart(&published).is_none(),
            "nothing was reported, so nothing is known, so nothing is stopped"
        );
    }

    /// And the same for a report this test binary's own environment cannot
    /// place: a development tree is not a versioned install, so the question is
    /// not about it. Both arms of this test are the same rule — only a PROVEN
    /// mismatch is a restart — and it is the rule that keeps a `cargo run`
    /// daemon and a pre-identity daemon equally safe.
    #[test]
    fn a_report_from_outside_a_versioned_install_is_never_a_restart() {
        let published = DaemonRendezvous {
            dir: PathBuf::from("/work/anvils"),
            key: "aaaaaaaaaaaa".to_owned(),
            url: "http://127.0.0.1:8793".to_owned(),
            pid: 4242,
            build: Some(host_primitives::rendezvous::ReportedBuild {
                exe: PathBuf::from("/somewhere/target/debug/chiefd"),
                identity: host_primitives::rendezvous::BuildIdentity {
                    dev: 24,
                    ino: 193_693,
                    size: 41_235_968,
                    mtime_s: 1_756_000_000,
                    mtime_ns: 123_456_789,
                },
            }),
        };
        assert!(
            stale_daemon_restart(&published).is_none(),
            "a development build is out of scope, never stale"
        );
    }

    #[test]
    fn a_patch_difference_is_compatible_so_an_upgrade_keeps_driving_a_running_company() {
        // The common case right after `chief upgrade`: the client is one patch
        // ahead of a daemon that has been up since before the swap. The wire
        // did not change across a patch, so it must keep driving it, not refuse.
        assert_eq!(incompatible_daemon(Some("2.0.7"), "2.0.9"), None);
        assert_eq!(incompatible_daemon(Some("2.0.9"), "2.0.7"), None);
        assert_eq!(incompatible_daemon(Some("2.0.7"), "2.0.7"), None);
    }

    #[test]
    fn a_minor_or_major_difference_refuses_and_names_both_versions() {
        assert_eq!(
            incompatible_daemon(Some("2.0.9"), "2.1.0"),
            Some(("2.0.9".to_owned(), "2.1.0".to_owned())),
            "a minor bump is a wire the client cannot assume"
        );
        assert_eq!(
            incompatible_daemon(Some("1.9.9"), "2.0.0"),
            Some(("1.9.9".to_owned(), "2.0.0".to_owned())),
            "a major bump refuses"
        );
    }

    #[test]
    fn an_absent_or_unparseable_daemon_version_is_no_opinion_never_a_refusal() {
        // A daemon from before #H.6 reports no version. It must not be stranded
        // on attach — driving it is exactly as safe as it was before the field
        // existed. A version string neither side can read is not skew evidence.
        assert_eq!(incompatible_daemon(None, "2.0.0"), None);
        assert_eq!(incompatible_daemon(Some("nightly"), "2.0.0"), None);
        assert_eq!(incompatible_daemon(Some("2.0.0"), "nightly"), None);
    }

    #[test]
    fn adoption_needs_all_three_facts_never_two() {
        assert!(is_expected_company_runtime(&healthy("0123456789ab"), "0123456789ab"));
        // Healthy, a company host, but the WRONG company — the exact adoption
        // that writes one company's state into another's database.
        assert!(!is_expected_company_runtime(&healthy("ba9876543210"), "0123456789ab"));
        // Healthy and the right key, but a docstore-only listener.
        let docstore = HealthProbe {
            runtime_mode: Some("docstore-only".to_string()),
            ..healthy("0123456789ab")
        };
        assert!(!is_expected_company_runtime(&docstore, "0123456789ab"));
        // A company host with the right key that is not ready.
        let not_ready = HealthProbe { ok: false, ..healthy("0123456789ab") };
        assert!(!is_expected_company_runtime(&not_ready, "0123456789ab"));
        // Healthy with an unproven role.
        let unproven = HealthProbe { runtime_mode: None, ..healthy("0123456789ab") };
        assert!(!is_expected_company_runtime(&unproven, "0123456789ab"));
    }

    /// TWO COMPANIES MAY BE CALLED THE SAME THING, so the identity probe
    /// compares the KEY and never the display word.
    ///
    /// Under the retired slug comparison these two proved each other: a client
    /// standing in `/work/acme` would have adopted the daemon serving
    /// `/elsewhere/acme`, and written one company's state into the other's
    /// database — which is the exact failure the whole check exists to prevent.
    #[test]
    fn two_same_named_companies_never_prove_each_others_identity() {
        let here = super::super::paths::company_key(Path::new("/work/acme"));
        let elsewhere = super::super::paths::company_key(Path::new("/elsewhere/acme"));
        assert!(!is_expected_company_runtime(&healthy(&elsewhere), &here));
        assert!(is_expected_company_runtime(&healthy(&here), &here));
    }

    /// THE COPIED-PROJECT CASE. `.chief/` lives inside the company directory,
    /// so `cp -r` copies the rendezvous with it — and a reader that trusted the
    /// file's location rather than its contents would point the copy's client
    /// at the ORIGINAL's daemon.
    #[test]
    fn a_rendezvous_copied_from_another_directory_is_not_this_directorys_daemon() {
        let copy = tempfile::tempdir().expect("tempdir");
        publish(copy.path(), Path::new("/work/the-original"), std::process::id());
        assert!(
            read_rendezvous(copy.path()).is_none(),
            "a rendezvous naming another directory must not be adopted here"
        );
    }

    /// A rendezvous that is absent, unreadable, or truncated mid-write is
    /// simply "no daemon here" — never an error surfaced to an operator. All
    /// three have the same repair, which is the spawn that follows.
    #[test]
    fn an_absent_or_unreadable_rendezvous_is_no_daemon_rather_than_a_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_rendezvous(dir.path()).is_none(), "nothing published");

        let path = super::super::paths::daemon_rendezvous_path(dir.path());
        #[allow(clippy::disallowed_methods)]
        {
            std::fs::create_dir_all(path.parent().expect("run dir")).expect("run dir");
            std::fs::write(&path, b"{\"dir\":\"/work/anvils\",\"key\":").expect("half a file");
        }
        assert!(read_rendezvous(dir.path()).is_none(), "half a rendezvous is not a rendezvous");

        publish(dir.path(), dir.path(), 4242);
        assert_eq!(
            read_rendezvous(dir.path()).map(|published| published.pid),
            Some(4242),
            "and a whole one describing this directory reads back"
        );
    }

    #[test]
    fn a_refusal_names_what_was_actually_observed() {
        assert_eq!(observed(&healthy("ba9876543210")), "company 'ba9876543210'");
        assert_eq!(
            observed(&HealthProbe {
                runtime_mode: Some("docstore-only".into()),
                ..healthy("0123456789ab")
            }),
            "docstore-only"
        );
        assert_eq!(
            observed(&HealthProbe { runtime_mode: None, ..healthy("0123456789ab") }),
            "an unproven listener"
        );
        assert_eq!(
            observed(&HealthProbe {
                ok: false,
                http_status: Some(503),
                reason: Some("schema-missing: org_documents absent".into()),
                ..HealthProbe::default()
            }),
            "an unhealthy listener answering 503 (schema-missing: org_documents absent)"
        );
        assert_eq!(
            observed(&HealthProbe {
                ok: false,
                reason: Some("no answer".into()),
                ..HealthProbe::default()
            }),
            "an unreachable listener"
        );
    }

    #[test]
    fn a_still_answering_daemon_is_reported_as_slow_not_broken() {
        // The whole reason `HealthProbe` is not a bool: an operator sent to
        // "your daemon is broken" when it is merely slow deletes a database.
        let slow = HealthProbe {
            ok: false,
            http_status: Some(503),
            reason: Some("schema-missing: org_documents absent".to_string()),
            ..HealthProbe::default()
        };
        let message = timed_out_message(
            Path::new("/work/acme"),
            42,
            Duration::from_secs(15),
            &slow,
            Path::new("/l/a.log"),
        );
        assert!(message.contains("still answering 503"));
        assert!(message.contains("may just be slow to initialize"));
        assert!(message.contains("CHIEFD_START_TIMEOUT_MS"));

        let absent = HealthProbe {
            reason: Some("waiting for the daemon rendezvous of pid 42".to_string()),
            ..HealthProbe::default()
        };
        let message = timed_out_message(
            Path::new("/work/acme"),
            42,
            Duration::from_secs(15),
            &absent,
            Path::new("/l/a.log"),
        );
        assert!(!message.contains("slow to initialize"));
        assert!(message.contains("last readiness observation"));
    }

    /// A daemon that never PUBLISHES is named as a probable stale install.
    ///
    /// Observed live in the registry era, and the shape carries over exactly:
    /// an installed chiefd carrying `127.0.0.1:8790` twice and the current
    /// discovery port zero times ran perfectly and was never seen, while the
    /// message said only "waiting", which sends an operator into a log that
    /// says the same thing. A chiefd built before the rendezvous existed fails
    /// identically — it writes no `daemon.json` at all — so the sentence moved
    /// with the wait rather than going with beacond.
    #[test]
    fn a_daemon_that_never_publishes_its_rendezvous_names_the_stale_install() {
        let never = HealthProbe {
            reason: Some("waiting for the daemon rendezvous of pid 42".to_string()),
            ..HealthProbe::default()
        };

        let message = timed_out_message(
            Path::new("/work/acme"),
            42,
            Duration::from_secs(15),
            &never,
            Path::new("/l/a.log"),
        );

        assert!(message.contains("INSTALLED binary older"), "{message}");
        assert!(message.contains("Reinstall chiefd"), "{message}");
    }

    /// A daemon that answers but is not ready keeps the SLOW advice.
    ///
    /// The stale-install sentence must not attach to a health probe that is
    /// plainly talking to us — telling somebody to reinstall a binary that is
    /// answering 503 sends them to replace a working process.
    #[test]
    fn a_daemon_that_answers_is_never_called_a_stale_install() {
        let answering = HealthProbe {
            ok: false,
            http_status: Some(503),
            reason: Some("schema-missing".to_string()),
            ..HealthProbe::default()
        };

        let message = timed_out_message(
            Path::new("/work/acme"),
            42,
            Duration::from_secs(15),
            &answering,
            Path::new("/l/a.log"),
        );

        assert!(!message.contains("INSTALLED binary older"), "{message}");
    }
    // -----------------------------------------------------------------------
    // The died-child report.
    //
    // The outage: chiefd exited at +1.3 ms with a precise refusal already in
    // `daemon.log`, and the client polled the rendezvous 61 times over 15 s
    // before printing a guessed cause whose remedy was wrong. The branch that
    // reports a dead child had existed and promised the log tail all along.
    // -----------------------------------------------------------------------

    /// THE MECHANISM, pinned against the OS rather than against our own code.
    ///
    /// This is why the died-child branch never fired for a spawned child, and
    /// it is the one fact that makes the fix necessary rather than tidier: a
    /// child that has exited and NOT been reaped is a zombie, and a zombie
    /// answers `kill(pid, 0)` exactly as a running process does. The daemon is
    /// put in its own process GROUP by `detach`, which does not change its
    /// parent — so every spawned chiefd this client has ever started was
    /// reapable only by this client, and the liveness probe could never return
    /// "gone" for one.
    ///
    /// If a future change reverts to a `kill(pid, 0)` liveness check in the
    /// wait loop, this test still passes and the wait-loop test below fails —
    /// which is the right split, because this one is a statement about Unix.
    #[test]
    fn an_exited_but_unreaped_child_still_answers_a_liveness_probe() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 3"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a child that exits immediately");
        let pid = i64::from(child.id());

        // Long enough that the child has certainly exited, and short enough
        // that nothing has reaped it: only this process can, and it has not.
        //
        // os-liveness: the ban routes waiting through the injected Clock so
        // tests never sleep, and this is the one wait a Clock cannot stand in
        // for — the subject is the KERNEL's own transition of a real process to
        // zombie, which no fake clock advances.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(Duration::from_millis(250));

        assert!(
            beacond::liveness::pid_is_live(pid),
            "a zombie answers kill(pid, 0) as live — if this ever fails, the \
             premise of the reap fix has changed and the wait loop should be revisited"
        );

        let status = child.try_wait().expect("try_wait reads the child status").expect("exited");
        assert_eq!(status.code(), Some(3), "try_wait reports the real exit status");
        assert!(
            !beacond::liveness::pid_is_live(pid),
            "reaping is what actually makes the pid go away"
        );
    }

    #[test]
    fn a_dead_childs_report_leads_with_its_exit_status_and_its_own_log() {
        let company = tempfile::tempdir().expect("company");
        let log_path = company.path().join("daemon.log");
        // host-effect: the ban keeps filesystem writes inside a host
        // transaction. This is a FIXTURE — a daemon log the daemon never ran to
        // write — in a tempdir this test owns, which is the file under test
        // rather than a product effect.
        #[allow(clippy::disallowed_methods)]
        std::fs::write(
            &log_path,
            "starting\nERROR admission refused: unknown company for this directory\n",
        )
        .expect("write log");

        let status = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .status()
            .expect("run a child that exits 7");
        let message = child_exited_message(company.path(), 801, &log_path, Some(status));

        assert!(message.contains("exit status 7"), "{message}");
        // The daemon's OWN words. During the outage the log named the cause
        // exactly and the operator was shown a hypothesis instead.
        assert!(message.contains("admission refused: unknown company"), "{message}");
        assert!(message.contains("it wrote this before exiting"), "{message}");
        // No guessing on a path where the cause is already known.
        assert!(!message.contains("Reinstall"), "{message}");
        assert!(!message.contains("most often"), "{message}");
    }

    #[test]
    fn a_dead_childs_report_survives_an_unreadable_status_and_an_absent_log() {
        let company = tempfile::tempdir().expect("company");
        let log_path = company.path().join("daemon.log");
        let message = child_exited_message(company.path(), 801, &log_path, None);
        assert!(message.contains("exited before becoming healthy"), "{message}");
        assert!(message.contains("see full log at"), "{message}");
    }

    #[test]
    fn the_timeout_message_shows_the_log_before_any_hypothesis() {
        let company = tempfile::tempdir().expect("company");
        let log_path = company.path().join("daemon.log");
        // host-effect: a fixture log in a tempdir this test owns. See above.
        #[allow(clippy::disallowed_methods)]
        std::fs::write(&log_path, "bound 127.0.0.1:8792\nwaiting on storage lock\n")
            .expect("write log");
        let last = HealthProbe {
            reason: Some("waiting for the daemon rendezvous of pid 801 (observed pid none)".into()),
            ..HealthProbe::default()
        };

        let message =
            timed_out_message(company.path(), 801, Duration::from_secs(15), &last, &log_path);

        // Known before guessed, and the ordering is the assertion.
        let log_at = message.find("waiting on storage lock").expect("log tail present");
        let guess_at = message.find("most often").expect("hypothesis present");
        assert!(log_at < guess_at, "the log must come before the hypothesis:\n{message}");
        // The message may only claim the child was ALIVE, because reaching
        // this function now proves it: an exited child leaves in one pass.
        assert!(message.contains("still running"), "{message}");
        // The hypothesis is one of several named, not the answer.
        assert!(message.contains("Other causes with this same shape"), "{message}");
    }
}
