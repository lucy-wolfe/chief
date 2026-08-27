//! The company daemon's own lifecycle routes, and the two placement decisions
//! that used to live in TypeScript: which tmux socket and which tmux session a
//! company owns.
//!
//! Ported from `cli.ts::companyBootSocket` (reached from `ls.ts`,
//! `attach-wiring.ts`, `stop.ts`, `reset.ts` and `launcher-wiring.ts`, each of
//! which carried its OWN partial re-implementation of the precedence for the
//! case where the manifest could not be read) and from the manifest/ownership
//! reads those five files each did through the process-global durable-store
//! client.
//!
//! # The bug this shape deletes
//!
//! Five call sites resolved a socket, and three of them carried a
//! "best-effort, minus the tier that needs chiefd" copy of the precedence for
//! use while the daemon was down. That is why `attach` needed a
//! stop-and-restart step: it spawned the daemon against a guessed socket, then
//! discovered the real one and had to correct it. Here the precedence is one
//! function with one input — a manifest that is either present or absent — and
//! the daemon is never spawned against a guess that the very next read
//! contradicts, because the ownership tier is read from the SAME daemon before
//! any actuation happens.

use std::time::Duration;

use chief_cli::placement::session_name_for;
use chief_cli::roster::Roster;

use super::http::{base, Client};
use super::{LifecycleError, Result};

/// Every company-route request's budget.
const ROUTE_BUDGET: Duration = Duration::from_secs(5);

/// The company facts the operator surface needs.
/// A company's operator stand-down, as the daemon reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StandDownFacts {
    /// When the operator stood the company down.
    pub(crate) since: String,
    /// What they said about it, or empty.
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompanyFacts {
    /// What this company is CALLED.
    ///
    /// Read back rather than derived, and it is the reason several verbs now
    /// bring the daemon up before they can name a tmux session: the slug lives
    /// in the store, the store is served by this company's own daemon, and a
    /// client that invented one would put the operator in a session no
    /// actuator will ever mint.
    pub(crate) slug: String,
    /// How many people the manifest carries.
    pub(crate) people_count: usize,
    /// The root department's head — the CEO.
    pub(crate) chief_person_id: String,
}

// TOMBSTONE (chief-home-is-cwd §4c): `CeoOnlyPrepared`, its `warning_text`,
// and `parse_ceo_only_answer` — the body of `POST
// /v1/org/runtime/prepare-ceo-only`, read off the wire. The route is deleted
// with the daemon-side CEO boot, so there is nothing left to parse.
//
// WHY THE WARNINGS ARE NOT MOURNED. The only field the parser still read was
// `warnings`: contained per-person materialization failures, collected while
// the DAEMON brought the CEO pane up. The daemon brings no pane up — the
// operator client owns every pane — so no caller of this client can produce
// that list any more. Printing an empty one would be furniture.
//
// WHAT REACHES CEO-ONLY NOW, since no client states the intent: nothing has
// to. An omitted launch intent is an EMPTY ALLOW-LIST, not an off switch
// (`conformance/fixtures/activity/fence-omitted-is-chief-only-not-unfenced.json`),
// so the fence admits the root head and denies everyone else, and the root
// separately holds an unconditional organization-root lease that keeps it
// desired. `stop::stop_runtime` clears the launch intent, which is why `reset`
// still lands on CEO-only after its teardown, and genesis records the same
// start decision through `org_ops::prepare_ceo_only` in-store. The route was
// re-stating a fail-safe the store already had.

/// The conventional session name, `org-<slug>-<key6>_`
/// (`placement::session_name_for`).
///
/// Used whenever the manifest is unreadable, which is exactly the case where a
/// company's daemon is down. It is a NAMING CONVENTION, not a guess about
/// state: a company whose manifest cannot be read still has a predictable
/// session name, and printing that is strictly better than printing nothing.
///
/// The two halves are facts of different kinds. The KEY is where the company
/// is — this client is standing in the directory and hashes it — and the SLUG
/// is what the company is CALLED, which only the store knows. A company whose
/// store cannot be read has no slug to print, which is why the caller supplies
/// it rather than this function reaching for one.
#[must_use]
pub(crate) fn conventional_session_name(slug: &str, key: &str) -> String {
    session_name_for(slug, key)
}

/// Minimal ISO-8601 rendering of epoch milliseconds, UTC, always with exactly
/// three fractional digits — the shape `new Date(ms).toISOString()` produces,
/// which is what every chiefd route accepts as its caller-supplied `at`.
///
/// Dependency-free, and re-derived here because it is a WIRE rendering, not
/// business logic: the alternative was `chiefd_core::isotime::iso_millis` —
/// one edge that would have put the whole store crate back in this binary's
/// link graph. The
/// branch-free civil-calendar conversion is the standard one, byte-identical
/// in output to the daemon's own.
fn iso_millis(epoch_millis: i64) -> String {
    let (days, millis_of_day) =
        (epoch_millis.div_euclid(86_400_000), epoch_millis.rem_euclid(86_400_000));
    let (year, month, day) = civil_from_days(days);
    let (hours, minutes, seconds, millis) = (
        millis_of_day / 3_600_000,
        (millis_of_day / 60_000) % 60,
        (millis_of_day / 1_000) % 60,
        millis_of_day % 1_000,
    );
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

/// Days since the Unix epoch to a civil `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, the same algorithm
/// `chiefd_core::isotime` carries, so the two renderings cannot drift.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 { month_prime + 3 } else { month_prime - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Which tmux socket a company's actuator drives.
///
/// The precedence, highest first, ported from `companyBootSocket`:
///
/// 1. `TEAM_LAUNCHER_TMUX_SOCKET` — an explicit operator override.
/// 2. The company's own recorded runtime-ownership socket, when it has one. A
///    company that previously ran on a non-default socket comes back onto it.
/// 3. The operator's current `$TMUX` server, so a company created from inside
///    tmux lands on the SAME server the operator is already looking at.
/// 4. `own` — the identity of the thing being booted, supplied by the caller.
///
/// Tier 2 is the only one that needs a live daemon, which is why it is passed
/// in rather than read here: the caller decides whether it has that fact.
///
/// # Tier 4 was the literal string `"default"`, and that is a shared server
///
/// `tmux` with no `-L` uses the socket named `default`. So the old fallback did
/// not merely pick an arbitrary name — it picked THE well-known one, the server
/// that every bare `tmux` command on the box lands on. And tier 4 is not an
/// exotic branch: it is reached whenever there is no override, no recorded
/// ownership and no ambient `$TMUX`, which is a first boot from an ordinary
/// shell — the common case. chief even announced it ("no ambient tmux session —
/// starting one for this ChiefD operator context").
///
/// Two companies booted that way shared ONE tmux server, and so did every
/// unrelated `tmux` invocation on the machine. That is a correctness fault on
/// its own, with no bad actor required: **a tmux server exits when its last
/// session is destroyed**, so tearing down one company took every other company
/// on that box with it, and an operator's own stray `tmux kill-server` did the
/// same. It cost a live company all eleven of its panes and five people on
/// 2026-08-18; the disappearance was never explained at OS level, and it did
/// not need to be — a shared server is enough.
///
/// The fallback is now the identity of the thing being booted: a company's key
/// (`sha256(canonical <dir>)[..12]`), or [`FOUNDER_SOCKET`] for the pre-company
/// Founder. Two different companies cannot collide, and neither can collide
/// with a bare `tmux`. It is a REQUIRED parameter rather than an internal
/// default so that "forgot to say who I am" cannot compile — the previous shape
/// let every caller reach the shared name by saying nothing at all.
#[must_use]
pub(crate) fn boot_socket(
    explicit: Option<&str>,
    recorded_ownership: Option<&str>,
    ambient_tmux: Option<&str>,
    own: &str,
) -> String {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return value.to_string();
    }
    if let Some(value) = recorded_ownership.map(str::trim).filter(|value| !value.is_empty()) {
        return value.to_string();
    }
    if let Some(socket_path) = ambient_tmux.and_then(|value| value.split(',').next()) {
        let current = socket_path.trim().rsplit('/').next().unwrap_or_default();
        if !current.is_empty() && current != "." && current != ".." {
            return current.to_string();
        }
    }
    own.to_string()
}

/// The two halves of the socket a daemon is started with.
///
/// A DEMAND is what an operator typed (`TEAM_LAUNCHER_TMUX_SOCKET`, which the
/// spawn passes on as `--runtime-socket`). A PREFERENCE is what this client
/// guessed in the absence of any better fact — the ambient `$TMUX` server, or
/// the company's own key.
///
/// # They used to be ONE value, and that is what bricked every upgrade
///
/// `chief` cannot read a company's runtime-ownership claim before a daemon
/// serves it, so the socket it names at spawn is a guess. It was passed as
/// `ORG_LAUNCHER_RUNTIME_SOCKET`, which chiefd read as an explicit demand, and a
/// demand contradicting a live claim is refused. That never fired while tier 4
/// was the shared string `"default"` — the same string every pre-`cb63690a0`
/// claim names. The moment tier 4 became the company key, every existing
/// company refused to start, and the operator saw only
/// `chiefd ... did not become healthy within 15s`.
///
/// So the guess now travels as a preference, which a live claim outranks. That
/// is not a second opinion about precedence: it is [`boot_socket`]'s OWN order
/// — the recorded claim above the ambient server and above the company key —
/// stated identically on both sides of the spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BootSocketRequest {
    /// The operator's explicit override, if they set one.
    pub(crate) demanded: Option<String>,
    /// Where to run when nobody claims this company.
    pub(crate) preferred: String,
}

/// Split this process's environment into [`BootSocketRequest`]'s two halves.
///
/// `own` is tier 4 — see [`boot_socket`].
#[must_use]
pub(crate) fn boot_socket_request(own: &str) -> BootSocketRequest {
    let demanded = std::env::var("TEAM_LAUNCHER_TMUX_SOCKET")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let ambient = std::env::var("TMUX").ok();
    BootSocketRequest { demanded, preferred: boot_socket(None, None, ambient.as_deref(), own) }
}

/// What a boot does about a runtime-ownership claim naming another socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimMove {
    /// Run where the claim says. The company either IS there, or nobody can
    /// prove it is not.
    Obey,
    /// The claim is dead by PROOF: no session for this company exists on the
    /// socket it names. Release it and come back on this company's own socket.
    Reclaim,
}

/// Decide whether a live claim is still true, from a proof and nothing else.
///
/// `probe` answers `has-session` for the CLAIMED socket, three-valued exactly
/// as [`crate::tmux::session_exists`] does: `Some(true)` present, `Some(false)`
/// PROVEN absent, `None` tmux would not answer. It is called only when there is
/// something to decide, so an ordinary boot asks tmux nothing.
///
/// # Why a proof, and why only this proof
///
/// A claim is runtime state. `chief stop` releases it, but a company killed any
/// other way — `pkill`, a crash, a box reboot, an OOM — leaves it standing, and
/// a company created before `cb63690a0` holds one naming the shared `default`
/// server that no upgrade can ever satisfy. Something has to move a stale claim
/// to a true one without a human editing anything.
///
/// The one thing that must never happen is actuating a company onto a server
/// its claim does not name — a shadow fleet is worse than a refusal. So only a
/// PROVEN absence reconciles. `Some(true)` obeys because the company is there;
/// `None` obeys because a question tmux would not answer proves nothing, and
/// absence is never proven by not looking. A timeout, a heartbeat age or "the
/// claim looks old" would each be a guess in the one position where a guess
/// converges a second fleet.
pub(crate) fn claim_move(
    recorded: Option<&str>,
    own: &str,
    probe: impl FnOnce(&str) -> Option<bool>,
) -> ClaimMove {
    let Some(claimed) = recorded.map(str::trim).filter(|claimed| !claimed.is_empty()) else {
        // Nobody claims this company; there is nothing to reconcile and the
        // caller's own socket already stands.
        return ClaimMove::Obey;
    };
    if claimed == own {
        return ClaimMove::Obey;
    }
    if probe(claimed) == Some(false) {
        ClaimMove::Reclaim
    } else {
        ClaimMove::Obey
    }
}

/// The socket the pre-company Founder boots onto when nothing else names one.
///
/// Founder has no directory and therefore no company key, so it carries its own
/// name. It is emphatically NOT `"default"`: the point of tier 4 is that chief
/// never lands on the server a bare `tmux` uses.
pub(crate) const FOUNDER_SOCKET: &str = "chief-founder";

/// Read the environment tiers of [`boot_socket`] from this process.
///
/// `own` is tier 4 — see [`boot_socket`]. Every caller states who it is; there
/// is no shared name left to fall through to.
#[must_use]
pub(crate) fn boot_socket_from_env(recorded_ownership: Option<&str>, own: &str) -> String {
    let explicit = std::env::var("TEAM_LAUNCHER_TMUX_SOCKET").ok();
    let ambient = std::env::var("TMUX").ok();
    boot_socket(explicit.as_deref(), recorded_ownership, ambient.as_deref(), own)
}

/// The socket of the tmux server THIS process is running inside.
///
/// For a verb that lives in a pane there is no question to ask anybody: tmux
/// puts the server's own socket path in `$TMUX`, and a pane cannot be in a
/// different server from the one that started it. So this is the same
/// precedence as [`boot_socket`] with **tier 2 deliberately absent**, and its
/// absence is a correctness claim before it is a speed one.
///
/// # Why the recorded socket is the WRONG answer here, not merely a slow one
///
/// Tier 2 is the company's recorded runtime-ownership socket — where the
/// company ran LAST time. It is the right answer for a verb that is about to
/// start or adopt a company, which is why `attach`, `stop` and `founder` all
/// use it. It is the wrong answer for a pane: if it ever differed from `$TMUX`,
/// obeying it would point the rail at a tmux server it is not in, so every
/// command it sent would be about somebody else's panes.
///
/// # And it is not merely slow, it is the whole of the boot
///
/// Tier 2 needs a live daemon, so asking for it is an authenticated HTTP round
/// trip. Measured on the operator's own box, on the click that produced their
/// screenshot: **1106ms** for `POST /v1/org/runtime-owner/read`, contending
/// with a converge pass that was itself taking 2310ms. That single read was 68%
/// of the 1630ms the rail spent between starting and having anything true to
/// draw — the whole of the interval in which the operator watched their
/// department list sit empty.
#[must_use]
pub(crate) fn pane_socket_from_env(own: &str) -> String {
    let explicit = std::env::var("TEAM_LAUNCHER_TMUX_SOCKET").ok();
    let ambient = std::env::var("TMUX").ok();
    boot_socket(explicit.as_deref(), None, ambient.as_deref(), own)
}

/// A typed client for one company daemon's proven URL.
pub(crate) struct CompanyClient<'a> {
    url: String,
    /// What to CALL this company in a sentence an operator reads.
    ///
    /// The directory, because that is the only name every caller holds: the
    /// slug lives in the store and a company whose daemon is refusing has no
    /// readable one. It is also the answer to the question a refusal provokes
    /// — *where do I go to fix this?* — which a slug never was.
    label: String,
    /// The directory-derived company key every `/v1/org/*` route matches its
    /// live company against. See [`CompanyClient::new`].
    key: String,
    client: &'a Client,
}

impl<'a> CompanyClient<'a> {
    /// Bind to the exact URL a liveness-and-identity proof returned.
    ///
    /// Never a URL looked up separately: a replacement registration between the
    /// proof and the call is the race this shape exists to remove.
    ///
    /// # Why the `key` is a parameter and not composed here
    ///
    /// Every `/v1/org/*` route resolves its company by matching the request's
    /// `slug` field against the company's identity. That identity used to be
    /// the composite `slug@sha256(orgs_root)[..12]`, and EVERY client composed
    /// it independently — nine sites, which drifted. It is now the directory
    /// hash, produced once by `paths::company_key` and carried.
    ///
    /// So this constructor takes it rather than deriving it: a second
    /// derivation is a second opinion about which company is being addressed,
    /// and the first time those disagreed every route answered
    /// `404 unknown-company` and no company could be created at all.
    pub(crate) fn new(client: &'a Client, url: &str, dir: &std::path::Path, key: &str) -> Self {
        Self {
            url: base(url).to_string(),
            label: dir.display().to_string(),
            key: key.to_owned(),
            client,
        }
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}{path}", self.url);
        let answer = self
            .client
            .post_json(&url, &body, ROUTE_BUDGET)
            .await
            .map_err(|error| LifecycleError::unreachable(error.to_string()))?;
        if answer.status == 200 {
            return Ok(answer.json().unwrap_or(serde_json::Value::Null));
        }
        Err(LifecycleError::refused(route_refusal(&self.label, path, answer.status, &answer.body)))
    }

    /// The company's client-agnostic roster: who exists, and who chiefd wants
    /// running.
    ///
    /// The ONE read placement needs. Everything the operator client shows —
    /// the session, the windows, the panes and their order — is derived from
    /// this body and nothing else; chiefd publishes no session, window, pane
    /// or layout of its own.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached, refuses, or answers
    /// with a body this client cannot decode. A malformed roster is a REFUSAL
    /// and never an empty one: an empty roster reads to an actuator as "stop
    /// everybody".
    pub(crate) async fn roster(&self) -> Result<Roster> {
        let body =
            self.post("/v1/org/roster/desired", serde_json::json!({ "slug": self.key })).await?;
        serde_json::from_value(body).map_err(|error| {
            LifecycleError::refused(format!(
                "chiefd for '{}' sent a roster this client cannot read: {error}",
                self.label
            ))
        })
    }

    /// The company's manifest facts, or `None` when it has none yet.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn facts(&self) -> Result<Option<CompanyFacts>> {
        let body =
            self.post("/v1/org/manifest/read", serde_json::json!({ "slug": self.key })).await?;
        if body.get("found").and_then(serde_json::Value::as_bool) != Some(true) {
            return Ok(None);
        }
        // The route returns the manifest as a serialized document, so the wire
        // round-trips byte-for-byte; decode only the three fields the operator
        // surface needs rather than the whole structural authority.
        let Some(serialized) = body.get("manifest").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let manifest: serde_json::Value = serde_json::from_str(serialized).map_err(|error| {
            LifecycleError::unreachable(format!(
                "chiefd for '{}' sent a manifest this client cannot read: {error}",
                self.label
            ))
        })?;
        // AC6: `runtimeSession` is NOT read off the manifest, because chiefd
        // no longer publishes it. It never needed to: this read already fell
        // back to `conventional_session_name`, the client's own derivation, on
        // any manifest it could not decode — so the wire value was a second
        // source of truth for a name this crate mints from the slug it was
        // handed. Callers use `conventional_session_name` directly now.
        let people_count = manifest
            .get("people")
            .and_then(serde_json::Value::as_object)
            .map_or(0, |people| people.len());
        let root_id = manifest.get("rootDepartmentId").and_then(serde_json::Value::as_str);
        let chief_person_id = root_id
            .and_then(|id| manifest.get("departments").and_then(|d| d.get(id)))
            .and_then(|department| department.get("headPersonId"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        // THE COMPANY'S NAME, and a manifest without one is a REFUSAL rather
        // than a company called "". Every tmux session this client opens is
        // named from it, so an empty answer would silently mint `org--<key>_`
        // and put the operator somewhere no actuator is drawing.
        let slug = manifest
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .filter(|slug| !slug.is_empty())
            .ok_or_else(|| {
                LifecycleError::refused(format!(
                    "chiefd for '{}' sent a manifest with no slug, so this client cannot name the \
                     company's tmux session",
                    self.label
                ))
            })?
            .to_owned();
        Ok(Some(CompanyFacts { slug, people_count, chief_person_id }))
    }

    /// The socket named by this company's LIVE runtime-ownership claim, if any.
    ///
    /// # A RELEASED CLAIM IS NOT A PLACEMENT, and reading it as one put a
    /// company back on the shared server
    ///
    /// The name and the predicate are
    /// `chiefd_host::gather::ChiefdFacts::active_runtime_owner_socket`'s,
    /// deliberately: this is the SAME row, read for the same purpose, and until
    /// `8ff573ff6` made a claim releasable while a company keeps running, the
    /// two readers disagreed about what it means. The daemon filtered on
    /// `status`; this one read `socketName` off the row and never looked. So
    /// after a handoff — which releases the old claim, restarts, and lands the
    /// daemon on the client's own preference — `chief actuate` read the
    /// released row, obeyed a socket nobody claimed any more, and projected the
    /// company's people onto `default`: the shared server every bare `tmux`
    /// lands on, the one `cb63690a0` exists to keep companies off, and the one
    /// whose last-session-exit took eleven panes off a live company.
    ///
    /// Measured 2026-08-18 on one company across two servers: the daemon on
    /// `qa`, the CEO pane and both rails on `default`.
    ///
    /// A released (or absent) claim answers `None` — nobody is running, so
    /// there is nothing to adopt and [`boot_socket`]'s tier 2 must fall
    /// through. Grep this name and both readers of the row appear together.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn active_runtime_owner_socket(&self) -> Result<Option<String>> {
        let body = self
            .post("/v1/org/runtime-owner/read", serde_json::json!({ "slug": self.key }))
            .await?;
        if body.get("found").and_then(serde_json::Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let Some(serialized) = body.get("doc").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let owner: serde_json::Value = serde_json::from_str(serialized).map_err(|error| {
            LifecycleError::unreachable(format!(
                "chiefd for '{}' sent an unreadable runtime-owner row: {error}",
                self.label
            ))
        })?;
        if owner.get("status").and_then(serde_json::Value::as_str) != Some("active") {
            return Ok(None);
        }
        Ok(owner
            .get("socketName")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string))
    }

    /// Claim this company's runtime ownership for the daemon this client is
    /// talking to.
    ///
    /// # Why a caller exists for this route at last
    ///
    /// The route documents itself as having no production caller, and asks any
    /// future one to say in writing why it wants a lease no launch is backing.
    /// Here is the why: `attach::reconcile_runtime_claim` RELEASES a
    /// claim for a company that stays UP. Nothing else in the product does
    /// that — `stop_supervised_runtime` releases a company it has just torn
    /// down — and nothing re-mints one, because a claim is minted only when the
    /// runtime projects or tears down a session and a post-handoff boot does
    /// neither: the people come back from durable start intent through the
    /// converge loop. So the handoff left a running company holding no claim,
    /// which is exactly the state the shadow-fleet guard exists to make
    /// impossible — a second `chief` in that directory would meet no claim to
    /// contradict and would converge a second fleet.
    ///
    /// The route names no socket. The daemon claims with its OWN
    /// `config.socket`, so this is not a second spelling of a claim: it is the
    /// fresh daemon recording the socket it already resolved.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn claim_runtime_ownership(&self) -> Result<()> {
        self.post("/v1/org/runtime/ownership/claim", serde_json::json!({ "slug": self.key }))
            .await
            .map(|_| ())
    }

    // TOMBSTONE (chief-home-is-cwd §4c): `prepare_ceo_only`, which POSTed
    // `/v1/org/runtime/prepare-ceo-only`. It was the ONE boot-shaped call on
    // this client, and `attach` and `reset` each documented that fact as their
    // structural guarantee against a roster boot ever being reachable.
    //
    // THAT GUARANTEE IS NOW ABSOLUTE RATHER THAN NARROW. There is no
    // boot-shaped call left here at all: the route is deleted with the
    // daemon-side CEO boot, and no method on `CompanyClient` asks any daemon to
    // start anybody. Nothing roster-shaped ever existed, so a future edit has
    // nowhere to reach for either. Do not re-add one — a client that can ask
    // for a boot is a second opinion about what should be running, beside the
    // desired set the converge loop already publishes.

    // TOMBSTONE: `actuator_present` and `runtime_observation`.
    //
    // Both POSTed `/v1/org/runtime/actions`, and neither was an incidental
    // caller of a deleted route: they WERE the deleted capabilities.
    // `actuator_present` read `actuator.presence`, a lease chiefd granted to
    // whoever last reported an observation; `runtime_observation` branched on
    // `withheld ∈ {no-actuator, observation-untrusted}`, which is chiefd
    // repeating what an actuator told it about tmux. Both asked chiefd a
    // question about the host, and chiefd holds no such fact any more.
    //
    // Neither is replaced by a second upward channel. Both callers ask the
    // HOST instead, which is where the answer was all along: `attach` reads
    // the actuator's own tmux session (`tmux::actuator_session`) and `ls`
    // reads the same thing for its coverage word. That is a stronger fact than
    // the lease it replaces -- `attach::actuator_needed` records the
    // measurement, 188 consecutive samples reporting `present` while no
    // actuator process existed anywhere on the host.
    //
    // NAMED, ACCEPTED LOSS: neither caller can see an actuator on a DIFFERENT
    // machine. A company actuated from elsewhere reads `unobserved` here.
    // Recovering that would mean an actuator reporting its presence upward,
    // which is exactly the direction this change closes.

    /// Clear every person's launch intent.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn clear_launch_intent(&self, at: &str) -> Result<()> {
        self.post("/v1/org/launch-intent/clear", serde_json::json!({ "slug": self.key, "at": at }))
            .await
            .map(|_| ())
    }

    /// Stand this company down: stop every person and keep them stopped.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn stand_down(&self, at: &str, reason: &str) -> Result<()> {
        self.post(
            "/v1/org/stand-down",
            serde_json::json!({ "slug": self.key, "at": at, "reason": reason }),
        )
        .await
        .map(|_| ())
    }

    /// Lift this company's stand-down.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn resume(&self, at: &str) -> Result<()> {
        self.post("/v1/org/stand-down/clear", serde_json::json!({ "slug": self.key, "at": at }))
            .await
            .map(|_| ())
    }

    /// This company's stand-down, or `None` when it is working normally.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn read_stand_down(&self) -> Result<Option<StandDownFacts>> {
        let body =
            self.post("/v1/org/stand-down/read", serde_json::json!({ "slug": self.key })).await?;
        Ok(serde_json::from_value(
            body.get("standDown").cloned().unwrap_or(serde_json::Value::Null),
        )
        .unwrap_or(None))
    }

    /// Drop the runtime projection rows.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn clear_runtime(&self, at: &str) -> Result<()> {
        self.post("/v1/org/runtime/clear", serde_json::json!({ "slug": self.key, "at": at }))
            .await
            .map(|_| ())
    }

    /// Release this company's runtime-ownership claim.
    ///
    /// The route names no socket: the daemon releases with its OWN
    /// `config.socket`, and `released_ownership` refuses a release from any
    /// other, so there is no socket for a caller to get wrong.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn release_runtime_ownership(&self) -> Result<()> {
        self.post("/v1/org/runtime/ownership/release", serde_json::json!({ "slug": self.key }))
            .await
            .map(|_| ())
    }

    /// Stamp the session epoch so every person's NEXT boot starts with an empty
    /// Pi session, and answer with the instant the fence now stands at.
    ///
    /// This is what a reset means for sessions, and it is a durable fence
    /// rather than a queued maintenance request: an operator resetting a
    /// company is overriding cooperative settling, not asking politely.
    ///
    /// # Why the stamp verb and not the singleton publish
    ///
    /// The epoch is an INSTANT — "a transcript modified before this is not
    /// resumed" — stored as `session_epoch(slug, at, reason)`. There is no
    /// counter in the schema, in the row model, or on the wire. This method
    /// used to read the row, look for an integer `epoch` key the store has
    /// never carried, and publish `{epoch, updatedAt}` back through
    /// `/v1/org/session-epoch/publish`, which is the generic singleton-row seam
    /// and deserializes the WHOLE typed document. Every `chief reset` answered
    /// `400 missing field 'version'`, and no reset has ever stamped an epoch.
    /// The read was equally dead: it returns `{version, organization, epochAt,
    /// reason}`, so the `epoch` lookup always missed and "the next epoch" was
    /// permanently 1.
    ///
    /// `/v1/org/session-epoch/stamp` is the operator verb for exactly this. It
    /// takes the two facts a caller can know, derives `version` (the constant
    /// one) and `organization` (its own company) rather than being told them,
    /// and takes the monotonic maximum against the stored instant inside one
    /// writer transaction — so the fence this method promises only moves
    /// forward.
    /// The raw publish overwrites, and could silently move the fence BACKWARDS
    /// and un-clear a boot that already happened.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached, refuses, or answers
    /// without the instant it stamped.
    pub(crate) async fn stamp_session_epoch(&self, at: &str) -> Result<String> {
        let stamped = self
            .post(
                "/v1/org/session-epoch/stamp",
                serde_json::json!({
                    "slug": self.key,
                    "epochAt": at,
                    "reason": format!("operator reset of '{}'", self.label),
                }),
            )
            .await?;
        stamped
            .get("epochAt")
            .and_then(serde_json::Value::as_str)
            .filter(|epoch_at| !epoch_at.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                LifecycleError::refused(format!(
                    "chiefd for '{}' stamped the session epoch but did not say when, so this \
                     reset cannot promise a clean session and stopped before tearing anything \
                     down",
                    self.label
                ))
            })
    }

    /// Pin the company's durable actuation mode to `shadow`.
    ///
    /// Merges over the STORED config, never the breaker-folded projection:
    /// naming a mode must not silently reset a sweep or budget override the
    /// operator set. That merge is the route's, not this caller's — see
    /// `/v1/org/converge-safety/set-actuation-config`.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn set_actuation_shadow(&self) -> Result<()> {
        self.post(
            "/v1/org/converge-safety/set-actuation-config",
            serde_json::json!({ "slug": self.key, "actuationMode": "shadow" }),
        )
        .await
        .map(|_| ())
    }

    /// Durably start one person.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn start_person(&self, person_id: &str) -> Result<()> {
        self.post(
            "/v1/org/person/start",
            serde_json::json!({ "slug": self.key, "personId": person_id }),
        )
        .await
        .map(|_| ())
    }

    /// Seed a company's genesis documents in one transaction.
    ///
    /// The wire carries the QUESTION — a company `spec` — not the answer. The
    /// route derives the manifest, the materialization document and the person
    /// operating contracts from it, so the three cannot disagree with each
    /// other. This client used to post a pre-normalized manifest plus two
    /// hand-built companion documents, which the route has not accepted since
    /// #751 moved normalization into `chiefd_core::store::organization_spec`;
    /// every create failed its 422 with `unknown field 'manifest'`.
    ///
    /// # Errors
    /// [`LifecycleError`] when the route cannot be reached or refuses.
    pub(crate) async fn genesis(&self, spec: &serde_json::Value, at: &str) -> Result<()> {
        self.post(
            "/v1/org/manifest/genesis",
            serde_json::json!({
                "slug": self.key,
                "spec": spec,
                "at": at,
            }),
        )
        .await
        .map(|_| ())
    }
}

/// chiefd refusal codes whose `detail` is a serde message about the REQUEST
/// DOCUMENT rather than prose written for a person.
///
/// `RouteError::malformed("malformed-doc", e.to_string())` is how every
/// `direct_org_row_route_pair!` publish reports a `doc` string its typed
/// singleton could not deserialize, and `e` is a `serde_json::Error`. The code
/// is a real refusal — chiefd is right to send it — but the detail is
/// `missing field \`version\` at line 1 column 50`, which is not a sentence to
/// show an operator, so this class is reported like a body rejection rather
/// than passed through.
const REQUEST_SHAPE_CODES: &[&str] = &["malformed-doc"];

/// Turn one non-200 company-route answer into a sentence an operator can act on.
///
/// Two very different failures used to print identically, as the raw status and
/// the raw body.
///
/// A chiefd REFUSAL carries `{code, detail}`. chiefd understood the request and
/// declined it on a product rule, and `detail` is already written for a person,
/// so `detail` is what gets printed.
///
/// Everything else is a rejection of the REQUEST — the body extractor's, whose
/// text is a bare serde sentence, or a [`REQUEST_SHAPE_CODES`] refusal, which is
/// chiefd wrapping the same serde sentence in an envelope. Either way it names a
/// field of a struct the operator has never heard of and cannot supply, and it
/// invites them to go looking for something they typed wrong. The real fact is
/// that this build of `chiefd` sends a request the daemon serving this company
/// does not accept: a build skew, with exactly one operator-side move. chiefd's
/// own words are kept at the end for whoever has to fix the client.
#[must_use]
fn route_refusal(company: &str, path: &str, status: u16, body: &str) -> String {
    let envelope = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|answer| {
        let code = answer.get("code").and_then(serde_json::Value::as_str)?.to_string();
        let detail = answer
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some((code, detail))
    });
    if let Some((code, detail)) = &envelope {
        if !REQUEST_SHAPE_CODES.contains(&code.as_str()) {
            let refusal =
                if detail.is_empty() { code.clone() } else { format!("{detail} ({code})") };
            return format!("chiefd for '{company}' refused: {refusal}");
        }
    }
    let said = match &envelope {
        Some((code, detail)) => format!("{detail} ({code})"),
        None => body.to_string(),
    };
    format!(
        "The chiefd serving '{company}' does not accept the request this `chiefd` sends to {path} \
         (HTTP {status}), so the operator client and that daemon are from different builds. \
         Nothing was changed. Run `chief stop` in {company} and try again — the next command \
         starts a daemon from this build. chiefd answered: {said}"
    )
}

/// An ISO-8601 millisecond stamp for the caller's clock.
///
/// Every company route takes the caller's `at`; the daemon never invents one,
/// so a replay carries the same stamp and stays idempotent.
#[must_use]
pub(crate) fn now_iso_millis() -> String {
    let now = std::time::SystemTime::now();
    let millis = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
    iso_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::{
        boot_socket, claim_move, conventional_session_name, iso_millis, route_refusal, ClaimMove,
        Client, CompanyClient, FOUNDER_SOCKET,
    };

    // TOMBSTONE (chief-home-is-cwd §4c): the three `prepare-ceo-only` verdict
    // tests, and with them the LAST reader of
    // `conformance/fixtures/wire/prepare-ceo-only-response.json`, which is
    // deleted in this same change. Its other reader,
    // `chiefd-api`'s `prepare_ceo_only_response_tests`, went with the route.
    //
    // The pair was the point: one test held the fixture equal to what the
    // server SERIALIZES and this one held it equal to what the client PARSES,
    // so neither side could move alone. Both sides are gone, so the fixture
    // pinned nothing and a kept copy would be a wire contract for a wire
    // nobody speaks.

    // --- the session-epoch wire contract -------------------------------------
    //
    // This stands up the REAL chiefd request type for the session-epoch stamp
    // verb, in front of the REAL `CompanyClient`, over a real loopback socket.
    // Nothing about the body is asserted by inspection: the route either
    // deserializes what this client sends or answers 400, exactly as the
    // daemon does, because this mirrors `chiefd-api`'s own extractor
    // (`deny_unknown_fields`, camelCase). `chief reset` shipped broken for as
    // long as this seam had no test at all.
    //
    // TOMBSTONE: the stub's `/v1/org/session-epoch/publish` arm, its
    // `PublishedEpoch`/`PublishRequest` mirrors, and the two tests that drove
    // them. That route is deleted — the publisher-route sweep found no caller
    // of any kind — and this client stopped calling it when `reset` moved to
    // the stamp verb. A stub for a route nobody serves pins nothing.

    /// `chiefd-api`'s `SessionEpochStampRequest`, field for field.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct StampRequest {
        slug: String,
        epoch_at: String,
        reason: String,
    }

    /// Serve the stamp route on loopback and answer the base URL.
    async fn chiefd_stub() -> String {
        let app = axum::Router::new().route(
            "/v1/org/session-epoch/stamp",
            axum::routing::post(|body: axum::extract::Json<StampRequest>| async move {
                // The stamp's monotonic maximum is chiefd's; the shape is
                // what is under test, so echo the accepted document back in
                // the daemon's own serialization.
                axum::Json(serde_json::json!({
                    "version": 1,
                    "organization": body.slug.clone(),
                    "epochAt": body.epoch_at,
                    "reason": body.reason,
                }))
            }),
        );
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the chiefd stub");
        let url = format!("http://{}", listener.local_addr().expect("stub address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        url
    }

    #[tokio::test]
    async fn a_reset_stamps_the_session_epoch_and_is_told_the_instant_it_landed_on() {
        let url = chiefd_stub().await;
        let client = Client::new();
        let company =
            CompanyClient::new(&client, &url, std::path::Path::new("/work/acme"), "0123456789ab");
        let stamped = company
            .stamp_session_epoch("2026-08-10T09:41:02.113Z")
            .await
            .expect("the stamp verb accepts what this client sends");
        assert_eq!(stamped, "2026-08-10T09:41:02.113Z");
    }

    // --- the runtime-ownership claim this client reads and mints -------------
    //
    // THE ROW IS READ BY TWO PROCESSES AND THEY MUST ASK THE SAME QUESTION.
    // `chiefd-host`'s `active_runtime_owner_socket` filters on `status`; this
    // client's namesake did not, and a company handed off between sockets came
    // back onto the released socket's server while its daemon ran on the new
    // one. These drive the REAL client over a real loopback socket against the
    // real wire shape (`OrgRowReadResponse`: `found` plus `doc`, a STRING
    // holding the serialized row), because the whole defect lived in one field
    // of that document being ignored.

    /// `chiefd-api`'s `OrgRowReadRequest`, field for field.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct RowReadRequest {
        slug: String,
        #[serde(default)]
        #[allow(dead_code)]
        if_seq_not: Option<i64>,
    }

    /// `chiefd-api`'s `SlugRequest`, field for field.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct SlugRequest {
        slug: String,
    }

    /// Serve a runtime-owner row and the ownership claim verb on loopback.
    ///
    /// `row` is the serialized `RuntimeOwner` the read route would answer with;
    /// `claims` records every slug the claim route was asked for, so a test can
    /// state that the claim was MADE and not merely available.
    async fn runtime_owner_stub(
        row: serde_json::Value,
        claims: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> String {
        let doc = serde_json::to_string(&row).expect("serialize the runtime-owner row");
        let app = axum::Router::new()
            .route(
                "/v1/org/runtime-owner/read",
                axum::routing::post(move |body: axum::extract::Json<RowReadRequest>| {
                    let doc = doc.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "found": true,
                            "doc": doc,
                            "seq": 41,
                            "slug": body.slug.clone(),
                        }))
                    }
                }),
            )
            .route(
                "/v1/org/runtime/ownership/claim",
                axum::routing::post(move |body: axum::extract::Json<SlugRequest>| {
                    let claims = std::sync::Arc::clone(&claims);
                    async move {
                        claims.lock().expect("record the claim").push(body.slug.clone());
                        // The route names no socket; the daemon claims its own.
                        axum::Json(serde_json::json!({
                            "organization": "verifynow-labs",
                            "status": "active",
                            "socketName": "qa",
                            "takeover": false,
                        }))
                    }
                }),
            );
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the chiefd stub");
        let url = format!("http://{}", listener.local_addr().expect("stub address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        url
    }

    fn no_claims() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    /// THE DEFECT, at the seam it lived at.
    ///
    /// Measured 2026-08-18: the handoff released the `default` claim at
    /// 03:00:57.798Z and the daemon came back on `qa` six seconds later, and
    /// `chief actuate` read this exact row and put the company's people back on
    /// `default` — the shared server every bare `tmux` lands on.
    #[tokio::test]
    async fn a_released_runtime_ownership_claim_names_no_socket() {
        let url = runtime_owner_stub(
            serde_json::json!({
                "version": 1,
                "organization": "verifynow-labs",
                "status": "released",
                "socketName": "default",
                "claimedAt": "2026-08-18T01:47:27.000Z",
                "releasedAt": "2026-08-18T03:00:57.798Z",
            }),
            no_claims(),
        )
        .await;
        let client = Client::new();
        let company =
            CompanyClient::new(&client, &url, std::path::Path::new("/work/acme"), "5eaf9a1ddb11");
        assert_eq!(
            company.active_runtime_owner_socket().await.expect("the read route answers"),
            None,
            "a released claim is nobody running anywhere, so there is no socket to adopt"
        );
    }

    /// The consequence, stated where the operator felt it: tier 2 falls
    /// through, so the actuator stays on the server it is actually in.
    #[tokio::test]
    async fn an_actuator_after_a_handoff_boots_where_the_daemon_is_and_not_on_default() {
        let url = runtime_owner_stub(
            serde_json::json!({
                "version": 1,
                "organization": "verifynow-labs",
                "status": "released",
                "socketName": "default",
                "releasedAt": "2026-08-18T03:00:57.798Z",
            }),
            no_claims(),
        )
        .await;
        let client = Client::new();
        let company =
            CompanyClient::new(&client, &url, std::path::Path::new("/work/acme"), "5eaf9a1ddb11");
        let recorded = company.active_runtime_owner_socket().await.expect("the read route answers");
        // `boot_socket_from_env`'s two tiers below the claim, spelled out
        // rather than read from this process: the ambient tmux server the
        // handoff restarted onto, then the company key.
        let socket = boot_socket(
            None,
            recorded.as_deref(),
            Some("/tmp/tmux-0/qa,4212,0"),
            "org-verifynow-labs-5eaf9a_",
        );
        assert_eq!(socket, "qa", "the released row must not outrank the server this process is in");
    }

    /// THE OTHER HALF OF THE SAME ROW: an ACTIVE claim is still adopted, and
    /// still outranks the ambient server. A test written over an active row
    /// passes either way, which is why the released case above exists.
    #[tokio::test]
    async fn an_active_runtime_ownership_claim_is_adopted_over_the_ambient_server() {
        let url = runtime_owner_stub(
            serde_json::json!({
                "version": 1,
                "organization": "verifynow-labs",
                "status": "active",
                "socketName": "org-verifynow-labs-5eaf9a_",
                "claimedAt": "2026-08-18T01:47:27.000Z",
                "validatedAt": "2026-08-18T03:00:57.000Z",
            }),
            no_claims(),
        )
        .await;
        let client = Client::new();
        let company =
            CompanyClient::new(&client, &url, std::path::Path::new("/work/acme"), "5eaf9a1ddb11");
        let recorded = company.active_runtime_owner_socket().await.expect("the read route answers");
        assert_eq!(recorded.as_deref(), Some("org-verifynow-labs-5eaf9a_"));
        assert_eq!(
            boot_socket(None, recorded.as_deref(), Some("/tmp/tmux-0/qa,4212,0"), "fallback"),
            "org-verifynow-labs-5eaf9a_",
            "a live claim is where the company runs, and it outranks this process's own server"
        );
    }

    /// THE SECOND HALF OF THE FINDING: a company that is UP must hold a claim.
    ///
    /// The handoff releases a claim for a company that keeps running, and no
    /// launch follows it — the people come back from durable start intent
    /// through the converge loop, and only a launch or a teardown mints a
    /// claim. So the client mints it, over the daemon that has just resolved
    /// the new socket. The route takes the slug and nothing else: the socket
    /// written is the daemon's own.
    #[tokio::test]
    async fn the_handoff_claims_the_runtime_for_the_daemon_it_restarted() {
        let claims = no_claims();
        let url = runtime_owner_stub(
            serde_json::json!({
                "version": 1,
                "organization": "verifynow-labs",
                "status": "released",
                "socketName": "default",
                "releasedAt": "2026-08-18T03:00:57.798Z",
            }),
            std::sync::Arc::clone(&claims),
        )
        .await;
        let client = Client::new();
        let company =
            CompanyClient::new(&client, &url, std::path::Path::new("/work/acme"), "5eaf9a1ddb11");
        company.claim_runtime_ownership().await.expect("the claim route accepts what this sends");
        assert_eq!(
            claims.lock().expect("read the recorded claims").as_slice(),
            ["5eaf9a1ddb11"],
            "the claim is made for this company, addressed by the key every /v1/org/ route matches"
        );
    }

    // --- what an operator is told when a route says no -----------------------

    #[test]
    fn a_chiefd_refusal_is_reported_in_chiefds_own_words() {
        let message = route_refusal(
            "acme",
            "/v1/org/session-epoch/stamp",
            422,
            r#"{"code":"invalid-session-epoch","detail":"Session epoch for 'acme' has an invalid time"}"#,
        );
        assert!(message.contains("Session epoch for 'acme' has an invalid time"), "{message}");
        assert!(message.contains("invalid-session-epoch"), "{message}");
        // chiefd decided; there is no build skew to report.
        assert!(!message.contains("different builds"), "{message}");
    }

    /// A BUILD-SKEW REFUSAL NAMES THE DIRECTORY, because that is where the
    /// recovery has to be typed.
    ///
    /// `chief stop` takes no company argument any more, so a message carrying
    /// a slug would tell an operator standing anywhere on the box to run a
    /// verb that acts on wherever they happen to be. The directory is both the
    /// company's identity and the answer to "where do I go".
    #[test]
    fn a_rejected_request_body_is_reported_as_build_skew_and_never_as_serde() {
        let message = route_refusal(
            "/work/acme",
            "/v1/org/session-epoch/stamp",
            400,
            "Failed to deserialize the JSON body into the target type: missing field `version`",
        );
        // The operator's terms: what happened, that nothing changed, and the
        // one move that fixes it.
        assert!(message.contains("different builds"), "{message}");
        assert!(message.contains("Nothing was changed"), "{message}");
        assert!(message.contains("Run `chief stop` in /work/acme"), "{message}");
        // Never the whole story, and never the headline.
        assert!(!message.starts_with("missing field"), "{message}");
    }

    #[test]
    fn a_malformed_doc_refusal_is_build_skew_too_because_its_detail_is_serdes() {
        // Observed against a live daemon: the pre-fix `chief reset` printed
        // "chiefd for 'reset-proof' refused: missing field `version` at line 1
        // column 50 (malformed-doc)". The envelope makes it a refusal, but the
        // detail inside it is still a serde sentence, so it is classified by
        // what it SAYS and not by whether chiefd wrapped it.
        let message = route_refusal(
            "/work/acme",
            "/v1/org/session-epoch/stamp",
            400,
            r#"{"code":"malformed-doc","detail":"missing field `version` at line 1 column 50"}"#,
        );
        assert!(message.contains("different builds"), "{message}");
        assert!(message.contains("Run `chief stop` in /work/acme"), "{message}");
        assert!(!message.starts_with("chiefd for '/work/acme' refused"), "{message}");
        // chiefd's own words survive, at the end, for whoever fixes the client.
        assert!(message.contains("missing field `version`"), "{message}");
    }

    #[test]
    fn a_refusal_with_a_code_but_no_detail_still_names_the_code() {
        let message =
            route_refusal("acme", "/v1/org/runtime/clear", 404, r#"{"code":"unknown-company"}"#);
        assert!(message.contains("unknown-company"), "{message}");
        assert!(!message.contains("different builds"), "{message}");
    }

    #[test]
    fn the_session_naming_convention_is_stable_for_an_unreadable_manifest() {
        assert_eq!(conventional_session_name("acme", "0123456789ab"), "org-acme-012345_");
        // The trailing `_` is the whole of the fix and is asserted here, at the
        // surface every stopped-company path prints and probes: a slug can
        // never contain it, so `acme`'s session name can never be a prefix of
        // `acme-corp`'s and a probe for a STOPPED company can never be answered
        // by a running neighbour. `tmux.rs` carries the rule, its negative
        // control, and the live proof.
        let key = "0123456789ab";
        assert_eq!(conventional_session_name("acme-corp", key), "org-acme-corp-012345_");
        assert!(!conventional_session_name("acme-corp", key)
            .starts_with(&conventional_session_name("acme", key)));
    }

    /// TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME, and a tmux
    /// server is box-wide — so the KEY is what keeps their sessions apart.
    ///
    /// Under the retired `org-<slug>_` these two were one session name, and
    /// the second attach would have landed the operator inside the first
    /// company's panes.
    #[test]
    fn two_same_named_companies_in_different_directories_get_different_sessions() {
        assert_ne!(
            conventional_session_name("acme", "0123456789ab"),
            conventional_session_name("acme", "ba9876543210")
        );
    }

    #[test]
    fn the_caller_stamp_renders_the_same_iso_8601_the_daemon_writes() {
        // FIXED VECTORS against `new Date(ms).toISOString()`, for the same
        // reason as the key above: after P6 this rendering is this crate's
        // own, and every company route takes it as the caller's `at`.
        assert_eq!(iso_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_millis(1_754_524_800_000), "2025-08-07T00:00:00.000Z");
        assert_eq!(iso_millis(1_786_060_800_000), "2026-08-07T00:00:00.000Z");
        // A leap day, and a stamp whose fractional part must keep its zeroes.
        assert_eq!(iso_millis(1_709_164_800_007), "2024-02-29T00:00:00.007Z");
        // Before the epoch: `div_euclid`/`rem_euclid`, not truncating division,
        // is what keeps this from rendering a negative millisecond field.
        assert_eq!(iso_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    /// The company key every tier-4 assertion below falls through to. A real
    /// key shape (twelve hex characters), because the point of the change is
    /// that the fallback IDENTIFIES the caller.
    const OWN: &str = "f7c6f2358be9";

    // --- the claim a boot may reconcile -------------------------------------
    //
    // `cb63690a0` moved tier 4 off the shared string `"default"`, so every
    // company created before it boots with an own-socket its live claim
    // contradicts, and there was no path from one to the other: chiefd refused,
    // `chief` printed a 15-second health timeout, and the only stated recovery
    // was a flag an operator had to find in a log file.

    /// A claim proved dead is reconciled: the socket it names holds no session
    /// for this company, so nothing can be projecting there.
    #[test]
    fn a_claim_whose_socket_holds_no_session_for_this_company_is_reclaimed() {
        let mut asked = None;
        let verdict = claim_move(Some("default"), OWN, |claimed| {
            asked = Some(claimed.to_owned());
            Some(false)
        });
        assert_eq!(verdict, ClaimMove::Reclaim);
        assert_eq!(
            asked.as_deref(),
            Some("default"),
            "the proof must be about the CLAIMED socket, never the one this boot wants"
        );
    }

    /// The invariant the refusal exists for. A shadow fleet is worse than a
    /// refusal, so a claim naming a socket that IS running this company stands
    /// — however much this boot would prefer its own.
    #[test]
    fn a_claim_whose_socket_still_runs_this_company_is_obeyed() {
        assert_eq!(claim_move(Some("default"), OWN, |_| Some(true)), ClaimMove::Obey);
    }

    /// PROOF, NOT ABSENCE OF EVIDENCE. `has-session` is three-valued and the
    /// third value is "tmux would not answer" — a permission error, a socket
    /// this user cannot reach, a tmux that is not there. Treating that as
    /// absence converges a second fleet onto a server that may be full of the
    /// operator's panes, which is the one outcome this whole path must never
    /// produce.
    #[test]
    fn a_claim_tmux_will_not_answer_for_is_obeyed_exactly_like_a_live_one() {
        assert_eq!(claim_move(Some("default"), OWN, |_| None), ClaimMove::Obey);
    }

    /// The ordinary boot asks tmux nothing: there is no claim, or it already
    /// names this socket.
    #[test]
    fn an_agreeing_or_absent_claim_is_never_probed() {
        let refuse_to_probe = |_: &str| -> Option<bool> { panic!("nothing to decide") };
        assert_eq!(claim_move(None, OWN, refuse_to_probe), ClaimMove::Obey);
        assert_eq!(claim_move(Some(OWN), OWN, refuse_to_probe), ClaimMove::Obey);
        assert_eq!(claim_move(Some("  "), OWN, refuse_to_probe), ClaimMove::Obey);
    }

    /// The spawn carries two different facts, and collapsing them is what made
    /// the upgrade un-startable: chiefd refuses a DEMAND that contradicts a live
    /// claim, and this client's own socket is a guess it makes before any claim
    /// is readable.
    #[test]
    fn the_operators_override_is_demanded_and_this_clients_guess_is_only_preferred() {
        // The CRATE's lock, not this module's own. A per-module mutex excludes
        // nobody: `tmux.rs`'s tests mutate the same process-global `TMUX`, and
        // two modules holding two different locks are not excluding each other.
        let _guard = crate::tmux::test_support::env_lock();
        let restore = (std::env::var("TEAM_LAUNCHER_TMUX_SOCKET").ok(), std::env::var("TMUX").ok());
        // Single-threaded under `env_lock`, and both variables are put back
        // below.
        std::env::remove_var("TEAM_LAUNCHER_TMUX_SOCKET");
        std::env::remove_var("TMUX");
        let guessed = super::boot_socket_request(OWN);
        assert_eq!(guessed.demanded, None, "an unset override demands nothing");
        assert_eq!(guessed.preferred, OWN, "and the guess is this company's own identity");

        std::env::set_var("TEAM_LAUNCHER_TMUX_SOCKET", "ci-socket");
        let demanded = super::boot_socket_request(OWN);
        assert_eq!(demanded.demanded.as_deref(), Some("ci-socket"));
        assert_eq!(demanded.preferred, OWN, "the fallback stands under the override");

        match &restore.0 {
            Some(value) => std::env::set_var("TEAM_LAUNCHER_TMUX_SOCKET", value),
            None => std::env::remove_var("TEAM_LAUNCHER_TMUX_SOCKET"),
        }
        if let Some(value) = &restore.1 {
            std::env::set_var("TMUX", value);
        }
    }

    #[test]
    fn an_explicit_override_beats_every_other_tier() {
        assert_eq!(
            boot_socket(Some("ci-socket"), Some("recorded"), Some("/tmp/tmux-0/ambient,1,0"), OWN),
            "ci-socket"
        );
    }

    #[test]
    fn a_recorded_ownership_socket_beats_the_ambient_server() {
        // The tier that needed a live daemon, and the reason the TypeScript
        // attach path had to stop and restart a daemon it had just spawned.
        assert_eq!(
            boot_socket(None, Some("recorded"), Some("/tmp/tmux-0/ambient,1,0"), OWN),
            "recorded"
        );
    }

    #[test]
    fn the_operators_own_tmux_server_is_used_when_nothing_is_recorded() {
        assert_eq!(boot_socket(None, None, Some("/private/tmp/tmux-501/work,3,0"), OWN), "work");
    }

    /// THE SHARED SERVER, AND WHY THIS TEST IS INVERTED RATHER THAN DELETED.
    ///
    /// This was `everything_absent_or_unusable_falls_through_to_default`, and
    /// every one of these five assertions demanded the string `"default"` —
    /// which is the socket a bare `tmux` uses. So the repo asserted, as a
    /// requirement, that an ordinary first boot lands on the machine's shared
    /// tmux server, alongside every other company and every stray `tmux`
    /// command on the box. A tmux server exits when its last session is
    /// destroyed, so that arrangement makes one company's teardown fatal to
    /// every other company on the machine, with nobody at fault.
    ///
    /// The cases are unchanged — the same five ways of having nothing usable —
    /// because the tier-4 CONDITION was always right. Only the answer was
    /// wrong.
    #[test]
    fn everything_absent_or_unusable_falls_through_to_this_callers_own_identity() {
        assert_eq!(boot_socket(None, None, None, OWN), OWN);
        assert_eq!(boot_socket(Some("  "), Some(""), None, OWN), OWN);
        // A `$TMUX` whose socket path basename is a path traversal segment is
        // not a socket name; falling through is the only safe reading.
        assert_eq!(boot_socket(None, None, Some("/tmp/..,1,0"), OWN), OWN);
        assert_eq!(boot_socket(None, None, Some("/tmp/.,1,0"), OWN), OWN);
        assert_eq!(boot_socket(None, None, Some(",1,0"), OWN), OWN);
    }

    /// Two companies that both fall through to tier 4 land on DIFFERENT
    /// servers. This is the whole property, and no assertion in this file
    /// could previously have caught its absence: the old fallback was a
    /// constant, so two companies were guaranteed to collide.
    #[test]
    fn two_companies_falling_through_never_share_a_server() {
        let first = super::super::paths::company_key(std::path::Path::new("/work/alpha"));
        let second = super::super::paths::company_key(std::path::Path::new("/work/beta"));
        assert_ne!(first, second, "premise: two directories have two keys");
        assert_ne!(
            boot_socket(None, None, None, &first),
            boot_socket(None, None, None, &second),
            "one company's last session closing must not take another company's server with it"
        );
    }

    /// And neither of them is the server a bare `tmux` talks to.
    #[test]
    fn no_caller_can_fall_through_onto_the_shared_tmux_default() {
        let company = super::super::paths::company_key(std::path::Path::new("/work/alpha"));
        assert_ne!(boot_socket(None, None, None, &company), "default");
        assert_ne!(boot_socket(None, None, None, FOUNDER_SOCKET), "default");
        assert_ne!(
            FOUNDER_SOCKET, "default",
            "Founder has no company key, so its own name is what keeps it off the shared server"
        );
    }

    #[test]
    fn the_stamp_is_iso_8601_with_millisecond_precision() {
        let stamp = super::now_iso_millis();
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.len(), "2026-08-07T00:00:00.000Z".len(), "{stamp}");
    }
}
