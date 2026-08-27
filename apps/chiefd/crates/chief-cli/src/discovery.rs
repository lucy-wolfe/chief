//! The consumer half of discovery: reading beacond's box-wide presence rows,
//! and bringing beacond up when it is not there.
//!
//! Ported from `packages/chiefing`'s `DiscoveryClient` (the read half only —
//! chiefing keeps serving TypeScript callers) and from
//! `apps/cli/src/legacy/foundation/beacond-ensure.ts`, which is deleted.
//!
//! # beacond is no longer on the path between a command and its own company
//!
//! It used to be: a verb was given a slug, asked the registry where that
//! company's daemon was, and bound the URL it answered. Every rung of that
//! ladder now hangs off the company's own directory instead
//! ([`super::daemon`] reads `<dir>/.chief/run/daemon.json`), so what is left
//! here is the ONE question a directory cannot answer — *what else exists on
//! this box?* — which `chief ls` and the web app ask and nothing else does.
//!
//! So there is no `lookup`, no `require` and no "did you mean" here any more.
//! Each of them existed to turn a typed slug into a location, and no verb types
//! a slug. `deregister` went with them: it cleared the location columns of a
//! daemon that could not clear its own, and a row whose pid is dead already
//! reads as stopped — a client that tidied it would be asserting a process is
//! gone at the one moment it cannot know that.
//!
//! `beacon.rs` is chiefd's PUBLISHING half and stays exactly what its own doc
//! says it is — a daemon does not discover itself. The two never share a client
//! and now never share a route at all.
//!
//! # Two removals, and they are not interchangeable
//!
//! A daemon exiting clears a row's LOCATION columns; the company survives and
//! is `stopped`. `company/delete` removes the ROW; the company stops existing.
//! Stopping a company must never reach the second one, and removing a company
//! must never stop at the first — a row with no location and no directory
//! behind it is the ghost that made `chief ls` untrustworthy on every box that
//! had ever run a test.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::http::{base, Client};
use super::{LifecycleError, Result};

/// Every discovery request's budget. beacond is a loopback service with no
/// business logic; anything slower than this is not answering.
const REQUEST_BUDGET: Duration = Duration::from_secs(2);

/// How long a freshly spawned beacond gets to bind before the operator is told
/// it never came up.
const ENSURE_BUDGET: Duration = Duration::from_secs(5);

/// The cadence of the bind wait. See [`ensure_running`] for why a wait exists.
const ENSURE_INTERVAL: Duration = Duration::from_millis(100);

/// One beacond company row.
///
/// The location columns are present together or not at all; a row with none of
/// them is a company that exists but is not running.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompanyRow {
    /// **The identity.** The canonical absolute directory this company
    /// occupies. One row per directory, forever.
    pub(crate) dir: String,
    /// The company's DISPLAY name. Not an identity: two directories may hold
    /// companies with the same slug, and both rows are legitimate.
    pub(crate) slug: String,
    /// The daemon's published base URL.
    #[serde(default)]
    pub(crate) url: Option<String>,
}

/// This machine's name, as `chief` prints it above the running-companies view.
///
/// Literally the function `chiefd`'s `beacon.rs` REPORTS from, called rather
/// than re-implemented. The two had drifted once already: the reporter read
/// `/proc` only, while this side read `/proc` and then shelled out to
/// `hostname(1)`. On macOS that meant the reporter wrote `"unknown"` and this
/// side read the real name — a guaranteed mismatch on the one platform where it
/// mattered. P6 put the two on opposite sides of a crate boundary, so the
/// shared definition lives in the registry crate they BOTH depend on.
#[must_use]
pub(crate) fn hostname() -> String {
    beacond::liveness::hostname()
}

/// The consumer-side beacond client.
pub(crate) struct Discovery {
    url: String,
    client: Client,
}

impl Discovery {
    /// `BEACOND_URL` or beacond's own compiled-in default.
    ///
    /// Read from [`beacond::config`], not restated: this module and
    /// `beacon.rs` each carried a private `DEFAULT_BEACOND_URL` literal, so
    /// one crate wrote the discovery port down twice and beacond wrote it a
    /// third time. See [`unreachable_beacond_detail`] for the incident that
    /// makes this a correctness rule and not a tidiness one.
    #[must_use]
    pub(crate) fn from_env() -> Self {
        let url = std::env::var("BEACOND_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(beacond::config::default_url);
        Self { url, client: Client::new() }
    }

    /// The configured registry address, for complete refusal diagnostics.
    #[must_use]
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// `GET /v1/list` — every company, running or not.
    ///
    /// # Errors
    /// [`LifecycleError::Unreachable`] when beacond does not answer or answers
    /// something this client cannot read.
    pub(crate) async fn list(&self) -> Result<Vec<CompanyRow>> {
        let url = format!("{}/v1/list", base(&self.url));
        let answer = self
            .client
            .get(&url, REQUEST_BUDGET)
            .await
            .map_err(|error| LifecycleError::unreachable(error.to_string()))?;
        if answer.status != 200 {
            return Err(LifecycleError::unreachable(format!(
                "beacond answered {} for /v1/list: {}",
                answer.status, answer.body
            )));
        }
        // ONE shape, because beacond serves one: `ListResponse { companies }`
        // (`beacond::router::list`). The bare-array arm this used to try first
        // decoded a body beacond has never sent — dead tolerance for a peer
        // that is in this same workspace and can simply be read (Mandate 0).
        serde_json::from_str::<CompanyList>(&answer.body).map(|list| list.companies).map_err(
            |error| {
                LifecycleError::unreachable(format!("beacond sent an unreadable /v1/list: {error}"))
            },
        )
    }

    /// `POST /v1/company/create` — record the company that occupies `dir`.
    ///
    /// **There is no conflict arm.** Its predecessor answered `409 slug-taken`
    /// when the slug existed under a different orgs root, because one slug
    /// under two roots was two companies fighting over one key. The directory
    /// is the key now, so the same pair is simply two rows and a slug is a
    /// display word with no uniqueness at all. A second create for the same
    /// directory updates that word and nothing else.
    ///
    /// # Errors
    /// [`LifecycleError::Unreachable`] on transport failure or on any status
    /// beacond does not document for this route.
    pub(crate) async fn create_company(&self, dir: &Path, key: &str, slug: &str) -> Result<()> {
        let url = format!("{}/v1/company/create", base(&self.url));
        let body = serde_json::json!({
            "dir": dir.display().to_string(),
            "key": key,
            "slug": slug,
        });
        let answer = self
            .client
            .post_json(&url, &body, REQUEST_BUDGET)
            .await
            .map_err(|error| LifecycleError::unreachable(error.to_string()))?;
        match answer.status {
            // beacond answers create with 200, never 201
            // (`beacond::router::create_company`). Accepting a status it
            // cannot send is a guess about a peer in this same workspace.
            200 => Ok(()),
            other => Err(LifecycleError::unreachable(format!(
                "beacond answered {other} for /v1/company/create: {}",
                answer.body
            ))),
        }
    }

    /// `POST /v1/company/delete` — remove the company row itself.
    ///
    /// The opposite of [`Self::create_company`] and the LAST step of
    /// `chief rm`. This is the only call in the product that makes a company
    /// stop existing, and it is reachable from exactly one verb: nothing in a
    /// company's own daemon links this module, so no company can remove
    /// another.
    ///
    /// beacond answers a delete of a directory it does not hold with
    /// `200 {"deleted": false}` rather than a 404, so a repeat is a no-op and
    /// not an error — a removal that was interrupted after the row went is
    /// still completable.
    ///
    /// # Errors
    /// [`LifecycleError::Unreachable`] on transport failure or on any status
    /// beacond does not document for this route.
    pub(crate) async fn delete_company(&self, dir: &Path) -> Result<()> {
        let url = format!("{}/v1/company/delete", base(&self.url));
        let body = serde_json::json!({ "dir": dir.display().to_string() });
        let answer = self
            .client
            .post_json(&url, &body, REQUEST_BUDGET)
            .await
            .map_err(|error| LifecycleError::unreachable(error.to_string()))?;
        if answer.status == 200 {
            Ok(())
        } else {
            Err(LifecycleError::unreachable(format!(
                "beacond answered {} for /v1/company/delete: {}",
                answer.status, answer.body
            )))
        }
    }

    /// One health request, and everything this client can learn from it.
    ///
    /// **REACHABILITY IS A 200 AND NOTHING ELSE**, exactly as the `reachable`
    /// predicate this replaced defined it. It deliberately does NOT require the
    /// body to parse: a beacond from a build whose health answer this client
    /// has never seen is still a beacond, and treating it as absent would have
    /// every command spawn a second one over a live one. An answer that does
    /// not parse is `Answering { health: None }` — unknowable, never absent.
    ///
    /// The identity rides this same answer precisely so the installed-build
    /// check costs no second round trip on a path every command walks.
    async fn probe(&self) -> BeacondProbe {
        let url = format!("{}/v1/health", base(&self.url));
        let Ok(answer) = self.client.get(&url, Duration::from_millis(500)).await else {
            return BeacondProbe::Unreachable;
        };
        if answer.status != 200 {
            return BeacondProbe::Unreachable;
        }
        BeacondProbe::Answering { health: serde_json::from_str(&answer.body).ok() }
    }
}

/// What one health request found.
#[derive(Debug)]
enum BeacondProbe {
    /// Nothing answered, or it answered something other than 200.
    Unreachable,
    /// A beacond answered. `health` is `None` when its answer did not carry
    /// the identity fields — the BOOTSTRAP GENERATION, which is unknowable and
    /// therefore never restarted.
    Answering { health: Option<BeacondHealth> },
}

/// beacond's health answer, as far as this client cares about it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeacondHealth {
    /// beacond's OWN pid — the only source there is. `chief` spawns beacond
    /// detached and drops the child handle, so nothing else on this box knows
    /// it, and a rule that signalled a pid it had inferred would be signalling
    /// a stranger.
    pid: u32,
    /// Which file that process is running, for the installed-build check.
    /// Absent from a beacond that predates the field: unknowable, never stale.
    #[serde(default)]
    build: Option<host_primitives::rendezvous::ReportedBuild>,
}

/// beacond's `/v1/list` in its object form.
#[derive(Debug, Deserialize)]
struct CompanyList {
    companies: Vec<CompanyRow>,
}

/// A stale beacond's restart order: the operator's line, and both identities.
struct StaleBeacond {
    line: String,
    running: host_primitives::rendezvous::BuildIdentity,
    installed: host_primitives::rendezvous::BuildIdentity,
}

/// Should this running beacond be replaced because it is not the installed
/// build?
///
/// `None` for every answer that is not a PROVEN mismatch. The unknowables — a
/// development build, an install that cannot be read — are left alone for the
/// same reason the bootstrap generation is: this is the one box-wide component,
/// and stopping it on a question nobody answered would cost every company on
/// the box its discovery.
fn stale_beacond_restart(home: &Path, health: &BeacondHealth) -> Option<StaleBeacond> {
    let installed = super::paths::beacond_binary(home);
    match super::build_identity::check(BEACOND_PROGRAM, health.build.as_ref(), &installed) {
        super::build_identity::BuildCheck::Current => None,
        super::build_identity::BuildCheck::Unknowable { reason } => {
            tracing::info!(
                event = "beacond.build.unknowable",
                reason = %reason,
                "the running beacond's build could not be compared against the installed one"
            );
            None
        }
        super::build_identity::BuildCheck::Stale { running, installed: installed_build } => {
            let exe =
                health.build.as_ref().map_or_else(|| installed.clone(), |build| build.exe.clone());
            Some(StaleBeacond {
                line: format!(
                    "{}\nThis is box-wide: every company on this box loses discovery for the \
                     moment it takes to rebind. No company state and no running pane is touched.",
                    super::build_identity::stale_line(
                        BEACOND_PROGRAM,
                        running,
                        installed_build,
                        &exe,
                    )
                ),
                running,
                installed: installed_build,
            })
        }
    }
}

/// The name this component is called by, in a log line and in a refusal.
const BEACOND_PROGRAM: &str = "beacond";

/// Stop the beacond at the pid IT REPORTED, gracefully.
///
/// SIGTERM and never SIGKILL: beacond's own shutdown path deregisters nothing
/// and holds no company state, but it does own an open sqlite file, and a
/// registry killed mid-write is a worse outcome than a stale binary. The
/// respawn below is the ordinary `ensure_running` spawn — the same one that
/// starts a beacond nobody had started at all.
///
/// A pid that is already gone is not an error: the beacond may have exited
/// between the health answer and this signal, and the spawn beneath handles
/// "nothing is running" as its ordinary case.
async fn stop_reported_beacond(pid: u32) {
    let Ok(raw) = i32::try_from(pid) else { return };
    let _ =
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), nix::sys::signal::Signal::SIGTERM);
    // os-liveness: a stopped process releases its port when the kernel says so
    // and there is no channel that reports it. Bounded, small, and a courtesy
    // rather than a race the outcome depends on — the spawn below waits for
    // the bind on its own ladder.
    #[allow(clippy::disallowed_methods)]
    tokio::time::sleep(Duration::from_millis(BEACOND_STOP_GRACE_MS)).await;
}

/// How long to let a stopped beacond release its port before respawning.
///
/// Small on purpose: the spawn that follows already waits for the bind on its
/// own ladder, so this only spares the first attempt a certain failure.
const BEACOND_STOP_GRACE_MS: u64 = 250;

/// Ensure discovery is up, spawning the installed binary if it is not.
///
/// Ported from `foundation/beacond-ensure.ts`. Three properties survive
/// unchanged, and each is the reason a line of it exists:
///
/// - **Idempotent.** A reachable beacond costs one request and never a respawn.
/// - **Installed binary only.** `~/.chief/bin/beacond`, never a PATH lookup:
///   beacond is not expected on `$PATH` and a stray same-named binary must
///   never be started by mistake.
/// - **Refuses instead of timing out** when the binary is absent, because "no
///   installed beacond" and "beacond will not bind" are different operator
///   problems with different fixes.
///
/// # Errors
/// [`LifecycleError::Refused`] when nothing is installed to spawn,
/// [`LifecycleError::Host`] when the spawn fails or the bind never happens.
/// `phases` narrates the COLD path to whoever is waiting (#1051). The warm
/// path — beacond already answering — is one probe and is over before a human
/// could read a line about it, so it says nothing; starting beacond spawns a
/// process and waits on [`ENSURE_BUDGET`], and that is the wait somebody is
/// actually sitting through. The tracing above and the frames here are taken
/// at the SAME two points, so the log and the pane never disagree about
/// whether beacond had to be started.
/// [`ensure_running_with_phases`] for the callers with nobody to narrate to.
///
/// Emitting into a sink whose receiver is dropped is a documented no-op, which
/// is the same wrapper shape `genesis::launch` takes over
/// `genesis::launch_with_phases`. Ten of this function's twelve callers are
/// verbs like `chief ls` and `chief stop` that have no stream and no waiting
/// human; making them all thread a sink would be churn in exchange for nothing.
///
/// # Errors
/// As [`ensure_running_with_phases`].
pub(crate) async fn ensure_running(discovery: &Discovery, home: &Path) -> Result<()> {
    let (sink, _receiver) = crate::host::phases::PhaseSink::channel(String::new());
    ensure_running_with_phases(discovery, home, &sink).await
}

#[tracing::instrument(name = "beacond.ensure_running", skip_all)]
pub(crate) async fn ensure_running_with_phases(
    discovery: &Discovery,
    home: &Path,
    phases: &crate::host::phases::PhaseSink,
) -> Result<()> {
    use crate::host::phases::Phase;
    if let BeacondProbe::Answering { health } = discovery.probe().await {
        // A BEACOND THAT DID NOT SAY WHAT IT IS, IS LEFT ALONE. Unknowable is
        // never stale: stopping a live registry on a question nobody answered
        // would cost every company on this box its discovery to satisfy a
        // check. The next beacond restart publishes the field and it becomes
        // knowable for ever after.
        let Some(health) = health else {
            tracing::info!(
                event = "beacond.build.unknowable",
                "the running beacond predates build-identity reporting, so it cannot say which \
                 binary it is; leaving it alone"
            );
            return Ok(());
        };
        {
            // #1281 THE BOX-WIDE ARM (kept as its own block so the borrow of
            // `health` above reads in one place). A beacond that is answering may still be
            // the wrong BUILD — the operator's ruling covers it by name, and it
            // is the one component here whose blast radius reaches other
            // people's companies.
            match stale_beacond_restart(home, &health) {
                None => return Ok(()),
                Some(stale) => {
                    eprintln!("{}", stale.line);
                    tracing::warn!(
                        event = "beacond.build.stale",
                        pid = health.pid,
                        running = %stale.running,
                        installed = %stale.installed,
                        "the running beacond is not the installed build; stopping it so the \
                         installed one is started. THIS IS BOX-WIDE: every company on this box \
                         loses DISCOVERY for the moment it takes to rebind. No company state and \
                         no running pane is touched — beacond is the presence registry, and \
                         daemons and panes do not depend on it after admission."
                    );
                    // SIGTERM TO THE PID IT REPORTED, never one inferred from a
                    // process table: a pid that is not beacond's is somebody
                    // else's process, and this is the one verb here that could
                    // kill a stranger.
                    stop_reported_beacond(health.pid).await;
                }
            }
        }
    }
    tracing::info!(
        event = "beacond.spawn.needed",
        url = discovery.url(),
        "beacond did not answer; starting the installed one"
    );
    phases.emit(Phase::BeacondStarting, discovery.url().to_string());
    let binary = super::paths::beacond_binary(home);
    if !binary.is_file() {
        return Err(LifecycleError::refused(format!(
            "beacond is not running and no installed binary was found at {}. Run 'bun run release' from the checkout, then retry.",
            binary.display()
        )));
    }
    let mut command = std::process::Command::new(&binary);
    command
        // BEACOND HOLDS NO COMPANY'S DIRECTORY. Without this it inherits the
        // cwd of whichever `chief` first noticed it was missing — which is a
        // company directory, essentially always. That is wrong twice: a daemon
        // that outlives every company pins a directory it does not own against
        // deletion, and `chief stop`'s stray sweep, which identifies an
        // unreachable process BY its working directory, would name the one
        // company-agnostic process the operator explicitly asked to keep.
        // Rooting it here makes that structural rather than an exception list.
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Discovery outlives the operator command that noticed it was missing.
    super::daemon::detach(&mut command);
    command.spawn().map_err(|error| {
        LifecycleError::host(format!("could not start beacond at {}: {error}", binary.display()))
    })?;

    let waiting_since = std::time::Instant::now();
    let deadline = std::time::Instant::now() + ENSURE_BUDGET;
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        if let BeacondProbe::Answering { health } = discovery.probe().await {
            tracing::info!(
                event = "beacond.reachable",
                attempt,
                waited_ms = chiefd_log::elapsed_ms(waiting_since),
                "beacond bound its port and answered"
            );
            // #1281 THE LOOP FLOOR. The beacond just started IS the installed
            // binary — unless the install itself is inconsistent, a `bin/`
            // symlink into a stale version directory or two install roots. Ask
            // once. A rule that restarts on mismatch and never checks the
            // result can restart for ever, and this one is box-wide.
            if let Some(stale) =
                health.as_ref().and_then(|health| stale_beacond_restart(home, health))
            {
                tracing::error!(
                    event = "beacond.build.stale-after-restart",
                    running = %stale.running,
                    installed = %stale.installed,
                    "the beacond this call just started still reports a different build; refusing \
                     rather than restarting a box-wide service again"
                );
                return Err(LifecycleError::refused(
                    super::build_identity::refusal_after_one_attempt(
                        BEACOND_PROGRAM,
                        health
                            .as_ref()
                            .map_or_else(
                                || super::paths::beacond_binary(home),
                                |health| {
                                    health.build.as_ref().map_or_else(
                                        || super::paths::beacond_binary(home),
                                        |build| build.exe.clone(),
                                    )
                                },
                            )
                            .as_path(),
                        &super::paths::beacond_binary(home),
                    ),
                ));
            }
            phases.emit(Phase::BeacondReady, discovery.url().to_string());
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            tracing::error!(
                event = "beacond.unreachable",
                attempt,
                waited_ms = chiefd_log::elapsed_ms(waiting_since),
                budget_ms = chiefd_log::duration_ms(ENSURE_BUDGET),
                url = discovery.url(),
                "beacond started and never became reachable"
            );
            return Err(LifecycleError::host(unreachable_beacond_detail(
                &binary.display().to_string(),
                discovery.url(),
            )));
        }
        tracing::info!(
            event = "beacond.ensure.wait",
            attempt,
            waited_ms = chiefd_log::elapsed_ms(waiting_since),
            budget_ms = chiefd_log::duration_ms(ENSURE_BUDGET),
            backoff_ms = chiefd_log::duration_ms(ENSURE_INTERVAL),
            "still waiting for beacond to bind"
        );
        // os-liveness: there is no push channel for "the process I just forked
        // bound its discovery port yet". Bounded by ENSURE_BUDGET above and
        // never unbounded — the same exemption the deleted TypeScript carried
        // in `scripts/reactive-allowlist.ts`.
        #[allow(clippy::disallowed_methods)]
        tokio::time::sleep(ENSURE_INTERVAL).await;
    }
}

/// What to tell an operator when beacond starts and never answers.
///
/// The naive message — "started but never became reachable" — sends them
/// hunting a network problem, and the overwhelmingly likely cause is not a
/// network at all: the INSTALLED binary predates the port this chiefd waits
/// on. beacond moved to a static 6969, and a copy installed before that change
/// binds something else, starts perfectly, and is never found. It is the exact
/// shape of failure this program keeps producing — two halves each behaving
/// correctly and disagreeing about one fact — so the message names it and says
/// what to do, rather than describing the symptom.
fn unreachable_beacond_detail(binary: &str, url: &str) -> String {
    format!(
        "beacond was started from {binary} but never became reachable at {url} within \
         {ENSURE_BUDGET:?}. The likeliest cause is that the INSTALLED beacond predates the \
         static discovery port and is listening somewhere else: it starts fine and is never \
         found. Reinstall it from this build before looking for a network fault."
    )
}

// TOMBSTONE: `nearest`, and the "did you mean 'acme-corp'?" refusal it fed.
//
// It turned a MISTYPED slug into a suggestion, and no verb takes a slug: a
// caller does not type a directory, it sends the one it is standing in. The
// nearest OTHER directory on the box is not an answer to anything, and
// offering one would invite an operator to act on a company they are not in.
// beacond deleted the Levenshtein machinery on its own side for the same
// reason (`beacond::router::lookup`'s doc).

#[cfg(test)]
mod tests {
    /// The message an operator reads when discovery never answers.
    ///
    /// Found by RUNNING `chief ls` on a box whose installed beacond predated
    /// the static port: the binary contained no occurrence of `6969` at all,
    /// started perfectly, and was never found. The old message described the
    /// symptom and sent the reader after a network fault that did not exist.
    #[test]
    fn an_unreachable_beacond_names_the_stale_install_first() {
        let detail =
            super::unreachable_beacond_detail("/root/.chief/bin/beacond", "http://127.0.0.1:6969");

        // The two facts a reader needs to act: which file, and which address.
        assert!(detail.contains("/root/.chief/bin/beacond"), "{detail}");
        assert!(detail.contains("http://127.0.0.1:6969"), "{detail}");
        // And the cause worth checking BEFORE a network, named as such.
        assert!(detail.contains("INSTALLED"), "{detail}");
        assert!(detail.contains("Reinstall"), "{detail}");
    }

    use super::{hostname, stale_beacond_restart, BeacondHealth, CompanyList, CompanyRow};
    use host_primitives::rendezvous::{BuildIdentity, ReportedBuild};

    /// A versioned install with `bin/beacond` pointing into it, as the
    /// installer lays one out.
    ///
    /// REAL FILES, and they must be: what is under test is what `stat` answers
    /// about real inodes on a real filesystem, and the defect this rule exists
    /// for — a replacement file at an unchanged path — is invisible to any
    /// mock, because it is the allocator's behaviour and not ours. Same
    /// allowance and same reason as `build_identity.rs`'s fixtures.
    #[allow(clippy::disallowed_methods)]
    fn install_with_beacond(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let home = tempfile::tempdir().expect("tempdir");
        let versioned = home.path().join(".chief/versions/0.5.0/bin");
        std::fs::create_dir_all(&versioned).expect("versions");
        std::fs::create_dir_all(home.path().join(".chief/bin")).expect("bin");
        let real = versioned.join("beacond");
        std::fs::write(&real, bytes).expect("write");
        let link = home.path().join(".chief/bin/beacond");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        (home, real)
    }

    fn reported(exe: &std::path::Path, pid: u32) -> BeacondHealth {
        BeacondHealth {
            pid,
            build: Some(ReportedBuild {
                exe: exe.to_path_buf(),
                identity: BuildIdentity::of_path(exe).expect("an identity"),
            }),
        }
    }

    /// THE OPERATOR'S CASE, box-wide half: a release rewrites the same version
    /// directory, so the version string does not move and the binary does.
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn a_rebuilt_beacond_is_stale_and_the_line_states_the_box_wide_blast_radius() {
        let (home, real) = install_with_beacond(b"the beacond that is running");
        let health = reported(&real, 4242);
        std::fs::remove_file(&real).expect("remove");
        std::fs::write(&real, b"the beacond that is installed now").expect("rewrite");

        let stale = stale_beacond_restart(home.path(), &health).expect("a proven mismatch");
        assert_ne!(stale.running, stale.installed);
        // THE BLAST RADIUS IS IN THE OPERATOR'S OWN LINE, not only in a code
        // comment. beacond is the one component here whose restart reaches
        // other people's companies, and a line that did not say so would be
        // asking them to accept a cost nobody named.
        assert!(stale.line.contains("box-wide"), "{}", stale.line);
        assert!(stale.line.contains("discovery"), "{}", stale.line);
        assert!(
            stale.line.contains("No company state and no running pane is touched"),
            "and the bound on that cost: {}",
            stale.line
        );
    }

    /// A beacond running the installed build is left alone, silently.
    #[test]
    fn a_current_beacond_is_not_restarted() {
        let (home, real) = install_with_beacond(b"one build");
        assert!(stale_beacond_restart(home.path(), &reported(&real, 4242)).is_none());
    }

    /// THE BOOTSTRAP GENERATION, and it matters more here than anywhere else:
    /// this is the box-wide component, so acting on an unanswered question
    /// would cost every company on the box its discovery.
    #[test]
    fn a_beacond_that_reports_no_build_is_never_restarted() {
        let (home, _real) = install_with_beacond(b"one build");
        let health = BeacondHealth { pid: 4242, build: None };
        assert!(stale_beacond_restart(home.path(), &health).is_none());
    }

    /// And a beacond somebody is running out of a development tree is out of
    /// scope, not stale — restarting a developer's own process onto the
    /// installed binary is the rule doing harm where it would least be
    /// expected.
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn a_development_beacond_is_out_of_scope() {
        let (home, _real) = install_with_beacond(b"one build");
        let dev = home.path().join("target/debug/beacond");
        std::fs::create_dir_all(dev.parent().expect("parent")).expect("target");
        std::fs::write(&dev, b"a cargo build").expect("write");
        assert!(stale_beacond_restart(home.path(), &reported(&dev, 4242)).is_none());
    }

    /// THE HEALTH ANSWER IS READ FROM BEACOND'S OWN BYTES. A field renamed on
    /// the server and not here would silently make every beacond unknowable —
    /// which reads as "nothing to do" and would quietly delete this rule.
    #[test]
    fn the_health_answer_beacond_actually_writes_is_the_one_this_client_parses() {
        let body = serde_json::json!({
            "status": "ok",
            "pid": 4242,
            "version": "0.5.0",
            "dbPath": "/root/.chief/beacond.sqlite",
            "build": {
                "exe": "/root/.chief/versions/0.5.0/bin/beacond",
                "identity": { "dev": 24, "ino": 193693, "size": 41235968, "mtimeS": 1756000000, "mtimeNs": 123456789 }
            }
        })
        .to_string();
        let health: BeacondHealth = serde_json::from_str(&body).expect("the client parses it");
        assert_eq!(health.pid, 4242);
        let build = health.build.expect("the identity survives the wire");
        assert_eq!(build.identity.ino, 193_693);
        assert_eq!(build.exe, std::path::PathBuf::from("/root/.chief/versions/0.5.0/bin/beacond"));
    }

    /// An OLDER beacond's answer must still parse, and must read as unknowable
    /// rather than as unreachable. Treating it as unreachable would spawn a
    /// second beacond over a live one.
    #[test]
    fn an_older_beacond_answer_parses_and_reads_unknowable() {
        let health: BeacondHealth =
            serde_json::from_str(r#"{"status":"ok","pid":4242}"#).expect("still parses");
        assert_eq!(health.build, None);

        // And the OLDEST answer — no pid either — fails to parse, which
        // `probe` turns into `Answering { health: None }`: UNKNOWABLE, not
        // unreachable. The difference is the whole safety property here. Read
        // as unreachable, a live box-wide beacond would have a second one
        // spawned over it by every `chief` command.
        assert!(serde_json::from_str::<BeacondHealth>(r#"{"status":"ok"}"#).is_err());
    }

    /// The exact bytes `beacond`'s `/v1/list` writes, envelope and all.
    ///
    /// `/v1/list` has exactly ONE shape, and this pins that the client reads
    /// that one rather than guessing among several. The bare-array arm the
    /// decoder used to try first was tolerance for a body `beacond::router`
    /// has never written — a peer in this same workspace, whose serializer can
    /// simply be read. Mandate 0: a fallback for a case that cannot arise is
    /// still a compatibility path, and it is the arm nobody would ever notice
    /// had rotted.
    #[test]
    fn a_list_is_read_only_out_of_beaconds_envelope_and_a_bare_array_is_not_accepted() {
        let enveloped = r#"{"companies":[{"dir":"/work/anvils","key":"0123456789ab","slug":"anvils","registeredAt":"2026-08-07T20:38:33.198Z"}]}"#;
        let list: CompanyList =
            serde_json::from_str(enveloped).expect("beacond's list must decode");
        assert_eq!(list.companies.len(), 1);
        assert_eq!(list.companies[0].dir, "/work/anvils");
        assert_eq!(list.companies[0].slug, "anvils");

        let bare = r#"[{"dir":"/work/anvils","key":"0123456789ab","slug":"anvils"}]"#;
        serde_json::from_str::<CompanyList>(bare)
            .expect_err("the shape beacond never sends is no longer a shape this client accepts");
    }

    /// TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME, and `chief ls`
    /// has to be able to show both.
    ///
    /// The case the retired slug-keyed registry could not represent at all:
    /// one slug was one row, so the second company either overwrote the first
    /// or was refused `409 slug-taken`. The DIRECTORY is what separates the
    /// rows, which is why it is what a row is read for.
    #[test]
    fn two_directories_holding_same_named_companies_are_two_rows() {
        let body = r#"{"companies":[
            {"dir":"/work/acme","key":"0123456789ab","slug":"acme","url":"http://127.0.0.1:8792"},
            {"dir":"/elsewhere/acme","key":"ba9876543210","slug":"acme"}
        ]}"#;
        let list: CompanyList = serde_json::from_str(body).expect("decode");
        let dirs: Vec<&str> = list.companies.iter().map(|row| row.dir.as_str()).collect();
        assert_eq!(dirs, ["/work/acme", "/elsewhere/acme"]);
        assert_eq!(list.companies[0].url.as_deref(), Some("http://127.0.0.1:8792"));
        // Created and never booted: absent, not empty.
        assert_eq!(list.companies[1].url, None);
    }

    /// A row registered before its daemon published a location has no location
    /// columns at all — beacond omits them rather than sending `null`.
    #[test]
    fn a_created_but_never_booted_row_decodes_with_no_location_columns() {
        let decoded: CompanyRow = serde_json::from_str(
            r#"{"dir":"/work/anvils","key":"0123456789ab","slug":"anvils","registeredAt":"2026-08-07T00:00:00.000Z"}"#,
        )
        .expect("decode");
        assert!(decoded.url.is_none());
    }

    #[test]
    fn this_machine_can_name_itself_on_every_platform_we_ship() {
        // The root cause was a hostname read that only worked on Linux, and it
        // failed SILENTLY into the sentinel rather than at compile time. This
        // asserts the real function on whatever platform the suite runs on, so
        // the macOS half cannot regress unnoticed again.
        let name = hostname();
        assert!(!name.is_empty());
        assert_ne!(name, beacond::config::UNNAMEABLE_HOST, "this host could not name itself");
    }
}
