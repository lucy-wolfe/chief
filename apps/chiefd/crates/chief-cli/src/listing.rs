//! `chief ls` — the box-wide company listing.
//!
//! Ported from the deleted TypeScript `ls.ts` and
//! `apps/cli/src/legacy/chiefd/{index,status}.ts`, all deleted.
//!
//! # TOMBSTONE: the bare-`chief` running-companies view
//!
//! A second renderer (`render_running`) and a follow-up hint (`follow_up`,
//! `stopped_follow_up`) lived here to answer bare `chief` with a table plus a
//! line telling the operator what to type next. Bare `chief` now FOUNDS or
//! GOES IN, decided by whether `<dir>/.chief/db/chief.db` exists — so it does
//! the thing instead of suggesting it, and the hint has nobody to address.
//! Two renderers over one gather is also two ways for the same rows to be
//! described; `chief ls` is the one that asks about the box.
//!
//! # The decision this module owns
//!
//! What "running" means. [`derive_status`] is the whole rule and it has exactly
//! three outputs, with everything mixed or indeterminate falling through to
//! `Unknown` — never invented as a fourth bucket, and never rounded to the
//! nearest confident answer.
//!
//! # And what "there is nothing there" means
//!
//! [`CompanyStatus::Missing`] is NOT a fourth bucket of that rule and is
//! deliberately decided outside it, by [`derive_status_with_store`]. Every one
//! of `derive_status`'s inputs is a PROBE — a request that timed out, a
//! manifest that did not parse, a tmux server that errored — so an ambiguous
//! one must round to `Unknown`. Whether a company's store database is on disk
//! is not a probe: it is a checked fact with two answers and no third.
//!
//! # And what "nobody is looking" means
//!
//! [`CompanyStatus::Unobserved`] is likewise NOT a fourth output of the probe
//! rule, and is decided outside it for the same reason `Missing` is: it is a
//! checked fact.
//!
//! `running` used to be derived from daemon health plus session existence, and
//! both of those are true of a company whose runtime nobody is actuating —
//! including every API-hosted company, permanently. A word that implies health
//! about a different question is the failure this repo keeps finding; `running`
//! promises people are up and being watched, and only one of those was ever
//! checked.
//!
//! # Who is asked, and why it changed
//!
//! The authority used to be chiefd: `POST /v1/org/runtime/actions` reported
//! `withheld ∈ {no-actuator, observation-untrusted}`, both derived from a
//! host report the actuator had committed upward. That route is deleted with
//! the direction it represented, and chiefd holds no fact about what is
//! running anywhere.
//!
//! It is not replaced by a second upward channel and it is not lost either.
//! **`chief ls` runs on the host.** It is a tmux client, it already reads the
//! company's own session on this socket, and "is there a live actuator window
//! for this company here" is a question it can answer for itself, with a
//! stronger fact than the one it lost: a RUNNING WINDOW rather than a lease
//! whose holder may have exited up to a lease-window ago
//! (`attach::actuator_needed` measured exactly that — 188 consecutive samples
//! reporting `present` with no actuator process anywhere on the host).
//!
//! What genuinely goes is REMOTE liveness: this listing can no longer say
//! anything about a company actuated from another machine, and does not
//! pretend to — an unreadable tmux is [`CompanyStatus::Unknown`], never an
//! accusation. An API-hosted company has no actuator window here and reads
//! `unobserved`, which is the same true word it had before.
//!
//! It exists because `chief ls` used to call a company `stopped` when its
//! whole data root had been deleted, and then invite the operator to
//! `chief attach` it. Three such rows were sitting in a shared beacond when
//! this was written, alongside thirty-seven others from throwaway test
//! companies, and the listing asserted all forty were merely stopped. A
//! registry that only ever grows and never tells the truth about what is behind
//! a row is worse than no registry.

use std::path::Path;
use std::time::Duration;

use chief_cli::actuate::crash_loop;

use super::company::conventional_session_name;
use super::daemon::probe_health;
use super::discovery::{CompanyRow, Discovery};
use super::http::Client;
use super::tmux::ActuatorSession;
use super::Result;

/// A listing row's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompanyStatus {
    /// Daemon healthy, manifest readable, session up.
    Running,
    /// Daemon unreachable and the session is not up. A REAL state: the company
    /// still exists, all of its durable state is on disk, and
    /// `chief attach` brings it back.
    Stopped,
    /// beacond holds a row and there is no company behind it: the store
    /// database in the row's own DIRECTORY is not on disk. Nothing can start
    /// it, and `chief rm` in that directory is the only thing to do with it.
    Missing,
    /// The daemon is healthy, the manifest resolved and the session is up —
    /// and nothing is observing the runtime, so nobody can say whether the
    /// people in it are alive. An API-hosted company is permanently here by
    /// construction: no `chief-cli` actuator ever attaches to one.
    Unobserved,
    /// The daemon is healthy and answering for this company — and its tmux
    /// session is PROVABLY GONE.
    ///
    /// Distinct from [`Self::Unknown`], and the distinction is the whole point.
    /// On 2026-08-18 a company lost its entire tmux server — both sessions,
    /// eleven panes, five people — and `chief ls` printed `unknown`, which is
    /// the word an operator scrolls past. It was not unknown. `has-session`
    /// answered, and it answered NO. A probe that failed to run is `unknown`;
    /// a probe that ran and found nothing is this.
    ///
    /// Distinct from [`Self::Stopped`] too: `stopped` promises `chief attach`
    /// picks the company up, and says the daemon is down. Here the daemon is
    /// up and serving a roster for people who have no panes.
    RuntimeGone,
    /// The daemon is healthy, the session is up AND a live actuator window is
    /// on this socket — and chiefd has still heard nothing from an actuator
    /// for longer than its own lapse.
    ///
    /// #1207, and the state the supervisor makes possible: a supervisor that is
    /// alive over a child that cannot get up looks exactly like health to tmux,
    /// which is all `unobserved` can see. Only chiefd knows nobody is reading
    /// the desired set, and only tmux knows the window is there; neither says
    /// this on its own.
    ///
    /// Distinct from [`Self::Unobserved`], which is the word when the window is
    /// ABSENT or dead. tmux's word wins when the session is not `Running` —
    /// this can only ever reclassify a row that was about to claim `running`.
    Unattended,
    /// Anything else — including every indeterminate probe.
    Unknown,
}

impl CompanyStatus {
    /// The lowercase word the table prints.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Missing => "missing",
            Self::Unobserved => "unobserved",
            Self::Unattended => "unattended",
            Self::RuntimeGone => "runtime-gone",
            Self::Unknown => "unknown",
        }
    }
}

/// What a company's own daemon endpoint said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointState {
    /// 200 with `{"status":"ok"}`.
    Healthy,
    /// Answered, but not with that.
    Reachable,
    /// Nothing answered at all.
    Unreachable,
}

/// One rendered listing row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompanyRowView {
    /// The company slug — its display name, which two rows may share.
    pub(crate) name: String,
    /// The directory it occupies, which is its identity and the place an
    /// operator has to `cd` to in order to do anything with it.
    pub(crate) directory: String,
    /// Its status.
    pub(crate) status: CompanyStatus,
    /// Its daemon URL, or `-` when it has none.
    pub(crate) chiefd: String,
    /// Its tmux session name.
    pub(crate) session: String,
    /// Its head count, or `-` when the manifest could not be read.
    pub(crate) people: String,
    /// How long chiefd has heard nothing from an actuator, when it said so.
    /// Printed only beside `unattended`, which is the only word it explains.
    pub(crate) actuator_silent: Option<Duration>,
}

/// The endpoint state a listing row is entitled to believe.
///
/// # A company with no database on disk cannot be served by any daemon
///
/// `probe_health` proves only that SOMETHING at that URL is a healthy chiefd.
/// The URL comes from beacond's registry and a port is a reusable number: on
/// 2026-08-18 a company whose directory had been deleted still carried
/// `http://127.0.0.1:8792` in its row, another company's daemon had since taken
/// that port, and `chief ls` printed the live neighbour's URL beside the dead
/// company's name — and called the row `unknown`.
///
/// The disqualifying fact is on disk and the caller has already read it. A
/// daemon serves a company by opening `<dir>/.chief/db/chief.db`; if that
/// database is absent then no daemon anywhere can be serving this row, and the
/// health the probe saw belongs to a stranger. That is a proof, not a
/// heuristic, which is why this is a total function of two facts and not a
/// guess about identity.
///
/// Deliberately NOT keyed on whether the manifest read succeeded, which is the
/// obvious alternative and is wrong: a company mid-genesis has a healthy daemon
/// of its very own that cannot yet answer for a manifest, and disbelieving it
/// would punish the honest case in order to catch the dishonest one.
///
/// The downgrade is also what lets the disk be consulted at all.
/// [`derive_status_with_store`] short-circuits on `Healthy` — correctly, since
/// a running company holds an unlinked database open — so a deleted-directory
/// row could never reach the `missing` branch while it still looked healthy.
#[must_use]
pub(crate) fn trusted_endpoint(transport: EndpointState, store_present: bool) -> EndpointState {
    match (transport, store_present) {
        (EndpointState::Healthy, false) => EndpointState::Reachable,
        (state, _) => state,
    }
}

/// The address a listing row prints for a company.
///
/// A URL is shown only for a row that still has a company on disk. For a row
/// that does not, the address is provably not its own (see
/// [`trusted_endpoint`]), and a dash cannot be pasted into a browser — which is
/// the entire argument for printing one.
#[must_use]
pub(crate) fn listed_url(url: &str, store_present: bool) -> String {
    if store_present {
        url.to_string()
    } else {
        "-".to_string()
    }
}

/// The status rule.
///
/// - **running**: the daemon is healthy AND the manifest resolved AND the
///   session exists.
/// - **stopped**: the daemon is unreachable AND the session is not provably up.
/// - **unknown**: everything else — reachable-but-not-ready, an errored tmux
///   probe, a session existing while the daemon is down, or a healthy daemon
///   whose manifest still did not resolve.
#[must_use]
pub(crate) fn derive_status(
    endpoint: EndpointState,
    manifest_available: bool,
    session: Option<bool>,
) -> CompanyStatus {
    if endpoint == EndpointState::Healthy && manifest_available && session == Some(true) {
        return CompanyStatus::Running;
    }
    // A PROBE THAT RAN AND FOUND NOTHING IS NOT AN INDETERMINATE PROBE.
    //
    // `session` is a tri-state and this arm is the one it exists for:
    // `Some(false)` means `has-session` answered and answered no, while `None`
    // means the tmux read itself did not complete. Both used to fall to
    // `unknown` at the bottom of this function, so a company whose entire tmux
    // server had vanished — daemon still healthy, manifest still resolving,
    // seven people still on the roster — printed the same word as a company
    // whose socket could not be reached. That is what an operator was shown on
    // 2026-08-18 while eleven panes and five people were gone.
    //
    // Gated on a HEALTHY daemon that resolved its manifest, so this can only
    // ever reclassify the row that was otherwise about to claim `running`: it
    // is the same single-branch scope `derive_status_with_actuator` takes, for
    // the same reason. A company whose daemon is down and whose session is
    // absent is `stopped` below, which is already the true word for it.
    if endpoint == EndpointState::Healthy && manifest_available && session == Some(false) {
        return CompanyStatus::RuntimeGone;
    }
    if endpoint == EndpointState::Unreachable && session != Some(true) {
        return CompanyStatus::Stopped;
    }
    CompanyStatus::Unknown
}

/// The status rule, plus the one thing no probe can tell you.
///
/// The order of the two questions is the whole correctness argument:
///
/// 1. **A healthy daemon is asked FIRST and wins outright.** A running company
///    holds its own database open, and an open file that has been unlinked is
///    still a database — the company is there, serving, with people in it.
///    Calling that `missing` because a `stat` came back empty would replace one
///    lie with a louder one. So a healthy endpoint never consults the disk at
///    all.
/// 2. **Otherwise the disk decides before the probe rule does.** Nothing is
///    answering, and the question "is this company stopped, or is it gone?" has
///    an answer on disk that no amount of probing will produce. `stopped`
///    promises `chief attach` will work; with no database it will not.
///
/// [`derive_status`] is called unchanged in both surviving branches, so its
/// three outputs and every assertion about them stay exactly what they were.
#[must_use]
pub(crate) fn derive_status_with_store(
    endpoint: EndpointState,
    manifest_available: bool,
    session: Option<bool>,
    store_present: bool,
) -> CompanyStatus {
    if endpoint == EndpointState::Healthy {
        return derive_status(endpoint, manifest_available, session);
    }
    if !store_present {
        return CompanyStatus::Missing;
    }
    derive_status(endpoint, manifest_available, session)
}

/// The status rule, plus the disk, plus who is actuating.
///
/// Layered rather than folded in: [`derive_status`] keeps its three outputs and
/// [`derive_status_with_store`] keeps its four, so every assertion either has
/// ever made still holds and this function can only reclassify a row that the
/// existing rules already called `Running`.
///
/// That single-branch scope is the whole safety argument. `stopped`, `missing`
/// and `unknown` are untouched — a company nobody is observing because it is
/// switched off is `stopped`, which is already the true word for it — and only
/// the row that was about to claim `running` on half the evidence is affected.
#[must_use]
pub(crate) fn derive_status_with_actuator(
    endpoint: EndpointState,
    manifest_available: bool,
    session: Option<bool>,
    store_present: bool,
    actuator: ActuatorSession,
) -> CompanyStatus {
    let base = derive_status_with_store(endpoint, manifest_available, session, store_present);
    if base != CompanyStatus::Running {
        return base;
    }
    match actuator {
        // A live actuator window on this socket. Somebody is converging this
        // company, here, now — which is what `running` was always promising.
        ActuatorSession::Running => CompanyStatus::Running,
        // No window, or one whose panes have all exited. Nobody here is
        // converging this company, so nobody can say whether the people in it
        // are alive.
        ActuatorSession::Absent | ActuatorSession::Exited => CompanyStatus::Unobserved,
        // A tmux read that failed proves nothing in either direction, and
        // `unknown` is this module's existing word for exactly that.
        ActuatorSession::Unknown => CompanyStatus::Unknown,
    }
}

/// The status rule, plus what chiefd heard.
///
/// Layered for the third time, for the reason the layer above states: every
/// assertion the four functions below it have ever made still holds, and this
/// can only reclassify the row that was already going to say `running`.
///
/// `attended` is a tri-state and the `None` arm is load-bearing: a daemon too
/// old to carry the fact, or a body that could not be read, must leave the row
/// exactly as it was. Silence from the WIRE is not silence from the ACTUATOR.
#[must_use]
pub(crate) fn derive_status_with_attendance(
    endpoint: EndpointState,
    manifest_available: bool,
    session: Option<bool>,
    store_present: bool,
    actuator: ActuatorSession,
    attended: Option<bool>,
) -> CompanyStatus {
    let base =
        derive_status_with_actuator(endpoint, manifest_available, session, store_present, actuator);
    if base != CompanyStatus::Running {
        return base;
    }
    match attended {
        // chiefd has heard nothing past its own lapse, while a live actuator
        // window sits on this socket. Both facts, and only together.
        Some(false) => CompanyStatus::Unattended,
        Some(true) | None => base,
    }
}

/// The cell the table prints for a status.
///
/// `unattended` is the one word that is useless without a number: "nobody is
/// converging this company" and "nobody has been converging this company for
/// two hours" ask for different reactions, and the second one is what the
/// operator was denied on 2026-08-23. Every other word prints exactly as it
/// always has.
#[must_use]
pub(crate) fn status_cell(status: CompanyStatus, silent: Option<Duration>) -> String {
    match (status, silent) {
        (CompanyStatus::Unattended, Some(silent)) => {
            format!("{} {}", status.label(), crash_loop::human_duration(silent))
        }
        _ => status.label().to_owned(),
    }
}

/// A listing probe is much shorter than an operational call on purpose.
///
/// The old listing ran a health call and then, on failure, a second
/// reachability call — two sequential round trips against the SAME url, each at
/// the full operational budget, for every company whose daemon is simply not
/// running. Rendered serially that is most of why a bare `chiefd` with nothing
/// running was slow to say so. A live local port answers well under a second,
/// or there is nothing there to answer at all.
///
/// The runtime-actions read is deliberately NOT wrapped in it. This budget is
/// sized for "is anything listening on this port", which is answered by a TCP
/// connect or not at all; the actions route computes a converge plan against
/// durable state, and timing it out at 400 ms would turn a slow but perfectly
/// healthy company into `unknown` on every listing. It is only ever reached
/// for a company whose daemon has ALREADY answered a health check, so it costs
/// nothing on the stopped rows that made this budget necessary.
const LISTING_PROBE_BUDGET: Duration = Duration::from_millis(400);

/// Build one row per beacond company.
///
/// A row with no location is a company that exists but is stopped or has never
/// booted. It belongs in the table without probing a guessed URL — a listing
/// must never invent an address to ask.
/// # ONE CREDENTIAL PER ROW, and it cannot be otherwise
///
/// This verb reads EVERY company on the box, and each one's operator key lives
/// in its own directory (`<dir>/.chief/keys`) — there is no fleet-wide identity
/// left to present to all of them. So the client is built per row, from that
/// row's own directory, and is never hoisted out of the loop.
///
/// A single shared client is not merely untidy here, it is a WRONG ANSWER: the
/// manifest read that fills the PEOPLE column is authenticated, so a client
/// carrying the wrong company's key (or none) gets a 401, `facts()` answers
/// `None`, and [`derive_status`] downgrades a perfectly healthy company from
/// `running` to `unknown` — the listing reporting every running company on the
/// box as indeterminate.
pub(crate) async fn gather(companies: &[CompanyRow]) -> Result<Vec<CompanyRowView>> {
    let mut rows = Vec::with_capacity(companies.len());
    for company in companies {
        // The row's OWN directory, never a recomputed default: a company whose
        // directory has since been deleted is precisely the case this check
        // exists for, and looking anywhere else would report it present.
        let dir = Path::new(&company.dir);
        let client = Client::operator(dir);
        let conventional =
            conventional_session_name(&company.slug, &super::paths::company_key(dir));
        let store_present = super::paths::company_present(dir);
        let Some(url) = company.url.as_deref() else {
            // No location columns at all: created and never booted, or stopped.
            // Nothing is worth probing, so the disk is the only remaining
            // question and `derive_status_with_store` answers it from an
            // unreachable endpoint.
            rows.push(CompanyRowView {
                name: company.slug.clone(),
                directory: company.dir.clone(),
                status: derive_status_with_store(
                    EndpointState::Unreachable,
                    false,
                    None,
                    store_present,
                ),
                chiefd: "-".to_string(),
                session: conventional,
                people: "-".to_string(),
                // Nothing answered, so there is no silence to report.
                actuator_silent: None,
            });
            continue;
        };
        let probe = tokio::time::timeout(LISTING_PROBE_BUDGET, probe_health(&client, url))
            .await
            .unwrap_or_default();
        let transport = match (probe.ok, probe.http_status) {
            (true, _) => EndpointState::Healthy,
            (false, Some(_)) => EndpointState::Reachable,
            (false, None) => EndpointState::Unreachable,
        };
        // The manifest is read against THIS company's own resolved endpoint,
        // never a process-global one: a listing row must not be able to leak
        // one company's endpoint into another's read.
        let facts = if transport == EndpointState::Healthy {
            super::company::CompanyClient::new(&client, url, dir, &super::paths::company_key(dir))
                .facts()
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let endpoint = trusted_endpoint(transport, store_present);
        // The session name is DERIVED, never read from the manifest — it is
        // `org-<slug>-<key6>_` whether or not a daemon is up to answer.
        let session = conventional.clone();
        // THIS company's socket, not a process-global one. It was
        // `boot_socket_from_env(None)`, which reached tier 4 and asked the
        // shared `default` server about every row — so a company on its own
        // socket had its session probed on somebody else's server and read as
        // absent. The key is the same tier-4 identity the company booted with.
        let socket = super::company::boot_socket_from_env(None, &super::paths::company_key(dir));
        let session_state =
            facts.as_ref().and_then(|_| super::tmux::session_exists(&socket, &session));
        // Read only for a company whose daemon is healthy — a row that cannot
        // reach `running` cannot be reclassified by the answer, and the read
        // costs a tmux round trip. LOCAL: this is the same socket the session
        // read above used, asked by the process that is entitled to look.
        let actuator = if endpoint == EndpointState::Healthy {
            super::tmux::actuator_session(&socket, &super::attach::actuator_session_name(&session))
        } else {
            ActuatorSession::Unknown
        };
        rows.push(CompanyRowView {
            name: company.slug.clone(),
            directory: company.dir.clone(),
            status: derive_status_with_attendance(
                endpoint,
                facts.is_some(),
                session_state,
                store_present,
                actuator,
                probe.actuator_attended,
            ),
            actuator_silent: probe
                .actuator_silent_ms
                .and_then(|millis| u64::try_from(millis).ok())
                .map(Duration::from_millis),
            // A URL is printed only for a row that still has a company on
            // disk. For a row that does not, the address is provably not its
            // own, and a dash cannot be pasted into a browser.
            chiefd: listed_url(url, store_present),
            session,
            people: facts.map_or_else(|| "-".to_string(), |f| f.people_count.to_string()),
        });
    }
    Ok(rows)
}

/// The `ls` table's columns.
///
/// DIRECTORY is second, right after the name it disambiguates: two rows may
/// carry the same NAME — that is the whole point of keying a company by where
/// it is — so a table without it cannot tell an operator which company a row is
/// about, or where to `cd` to act on it.
const COLUMNS: [&str; 6] = ["NAME", "DIRECTORY", "STATUS", "CHIEFD", "SESSION", "PEOPLE"];
/// Which column shrinks when the row will not fit.
///
/// DIRECTORY, and it ellipsizes from the LEFT. It is the only unbounded cell in
/// the table — a URL is `http://127.0.0.1:8792` and a status is one word — and
/// a path's identifying end is its LAST segment, so `…/work/anvils` keeps what
/// distinguishes it while `/Users/pat/very/long/…` throws exactly that away.
const DIRECTORY_COLUMN: usize = 1;
/// Space between columns.
const COLUMN_GAP: usize = 2;
/// The width the table targets.
const TERMINAL_WIDTH: usize = 80;
/// Below this, shrinking DIRECTORY buys nothing.
const MIN_DIRECTORY_WIDTH: usize = 10;

/// Docker-style, space-aligned. The slug is NEVER shrunk or truncated; CHIEFD
/// is the one column that ellipsizes to make a long row fit, and only when
/// doing so actually helps. An empty registry renders just the header.
#[must_use]
pub(crate) fn render_table(rows: &[CompanyRowView]) -> Vec<String> {
    let cells: Vec<[String; 6]> = rows
        .iter()
        .map(|row| {
            [
                row.name.clone(),
                row.directory.clone(),
                status_cell(row.status, row.actuator_silent),
                row.chiefd.clone(),
                row.session.clone(),
                row.people.clone(),
            ]
        })
        .collect();
    let mut widths: Vec<usize> = COLUMNS
        .iter()
        .enumerate()
        .map(|(index, header)| {
            cells
                .iter()
                .map(|row| row[index].chars().count())
                .chain([header.len()])
                .max()
                .unwrap_or(0)
        })
        .collect();

    // Only shrink DIRECTORY down to whatever budget is left after every other
    // column takes its natural width. If a long NAME or SESSION already blows
    // the budget on its own, shrinking DIRECTORY buys nothing and only makes
    // the row worse, so it keeps its natural width instead.
    let gaps = COLUMN_GAP * (widths.len() - 1);
    let others: usize = widths
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != DIRECTORY_COLUMN)
        .map(|(_, w)| *w)
        .sum();
    let budget = TERMINAL_WIDTH.saturating_sub(gaps + others);
    if budget >= MIN_DIRECTORY_WIDTH && widths[DIRECTORY_COLUMN] > budget {
        widths[DIRECTORY_COLUMN] = budget;
    }

    let format_row = |values: &[String]| -> String {
        let last = values.len() - 1;
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let width = widths[index];
                let cell = if index == DIRECTORY_COLUMN && value.chars().count() > width {
                    // FROM THE LEFT: a path's last segment is what names it.
                    let keep = width.saturating_sub(1);
                    let skip = value.chars().count().saturating_sub(keep);
                    format!("…{}", value.chars().skip(skip).collect::<String>())
                } else {
                    value.clone()
                };
                if index == last {
                    cell
                } else {
                    format!("{cell:<width$}")
                }
            })
            .collect::<Vec<_>>()
            .join(&" ".repeat(COLUMN_GAP))
    };

    let header: Vec<String> = COLUMNS.iter().map(|value| (*value).to_string()).collect();
    let mut lines = vec![format_row(&header)];
    lines.extend(cells.iter().map(|row| format_row(row)));
    lines
}

/// `chief ls`.
///
/// # Errors
/// [`super::LifecycleError`] when discovery cannot be read.
pub(crate) async fn run_list() -> Result<()> {
    let home = super::paths::home()?;
    let discovery = Discovery::from_env();
    super::discovery::ensure_running(&discovery, &home).await?;
    // AUTHENTICATED PER ROW, inside `gather`: this verb reads EVERY company on
    // the box and each one's operator key lives in its own directory, so there
    // is no one credential to hoist out here.
    let rows = gather(&discovery.list().await?).await?;
    for line in render_table(&rows) {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        conventional_session_name, derive_status, derive_status_with_actuator,
        derive_status_with_attendance, derive_status_with_store, listed_url, render_table,
        status_cell, trusted_endpoint, ActuatorSession, CompanyRowView, CompanyStatus,
        EndpointState,
    };

    /// One listing row for the company called `name` in `/work/<name>`.
    ///
    /// The directory is derived from the name only so the fixture reads
    /// plainly; nothing under test depends on the two being related, and the
    /// case that matters most is the one where two rows share a NAME and
    /// differ only by directory.
    fn row(name: &str, status: CompanyStatus, chiefd: &str) -> CompanyRowView {
        row_in(name, &format!("/work/{name}"), status, chiefd)
    }

    /// One listing row, stating both halves.
    fn row_in(name: &str, dir: &str, status: CompanyStatus, chiefd: &str) -> CompanyRowView {
        CompanyRowView {
            name: name.to_string(),
            directory: dir.to_string(),
            status,
            chiefd: chiefd.to_string(),
            session: conventional_session_name(
                name,
                &super::super::paths::company_key(std::path::Path::new(dir)),
            ),
            people: "3".to_string(),
            actuator_silent: None,
        }
    }

    #[test]
    fn running_needs_all_three_facts() {
        assert_eq!(derive_status(EndpointState::Healthy, true, Some(true)), CompanyStatus::Running);
        assert_eq!(
            derive_status(EndpointState::Healthy, false, Some(true)),
            CompanyStatus::Unknown
        );
        // A session PROVED absent is `runtime-gone` rather than `unknown` — the
        // one answer this row changed, and the whole of the change. What this
        // test defends is unaltered: none of the three near-misses is
        // `running`, which is asserted immediately below for all of them.
        assert_eq!(
            derive_status(EndpointState::Healthy, true, Some(false)),
            CompanyStatus::RuntimeGone
        );
        assert_eq!(derive_status(EndpointState::Healthy, true, None), CompanyStatus::Unknown);
        for near_miss in [
            derive_status(EndpointState::Healthy, false, Some(true)),
            derive_status(EndpointState::Healthy, true, Some(false)),
            derive_status(EndpointState::Healthy, true, None),
        ] {
            assert_ne!(near_miss, CompanyStatus::Running, "running needs all three");
        }
    }

    #[test]
    fn a_healthy_watched_company_is_still_plainly_running() {
        // The control. Adding a fifth word must not cost the fourth its
        // meaning: everything that was `running` and IS observed stays exactly
        // that, with the same three probe inputs deciding it.
        assert_eq!(
            derive_status_with_actuator(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Running,
            ),
            CompanyStatus::Running
        );
    }

    /// An actuator window whose panes have ALL EXITED is not an actuator. It
    /// is the corpse of one, kept on screen by `remain-on-exit` so its last
    /// words can be quoted, and reading it as coverage would call a company
    /// `running` on the strength of a window that failed.
    #[test]
    fn a_dead_actuator_window_is_not_coverage() {
        assert_eq!(
            derive_status_with_actuator(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Exited,
            ),
            CompanyStatus::Unobserved
        );
    }

    #[test]
    fn a_healthy_company_nobody_is_observing_is_unobserved_rather_than_running() {
        // The defect this word exists for. Daemon healthy, manifest readable,
        // session up — every input the old rule had, all true — and nothing is
        // watching the runtime, so nobody can say whether the people in it are
        // alive. `running` claimed they were.
        assert_eq!(
            derive_status_with_actuator(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Absent,
            ),
            CompanyStatus::Unobserved
        );
    }

    #[test]
    fn a_coverage_read_that_failed_is_unknown_and_never_an_accusation() {
        // A failed read proves nothing in either direction. Rounding it to
        // `running` restores the lie; rounding it to `unobserved` accuses a
        // healthy company on the strength of a request that did not complete.
        assert_eq!(
            derive_status_with_actuator(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Unknown,
            ),
            CompanyStatus::Unknown
        );
    }

    #[test]
    fn coverage_never_reclassifies_a_row_that_was_not_going_to_say_running() {
        // The scope guarantee, asserted rather than described. A stopped
        // company is not observed either — nothing is running to observe — and
        // `stopped` is already the true word for it. Same for `missing`, whose
        // whole point is that there is nothing behind the row at all.
        for actuator in [
            ActuatorSession::Running,
            ActuatorSession::Absent,
            ActuatorSession::Exited,
            ActuatorSession::Unknown,
        ] {
            assert_eq!(
                derive_status_with_actuator(
                    EndpointState::Unreachable,
                    false,
                    None,
                    true,
                    actuator,
                ),
                CompanyStatus::Stopped,
                "{actuator:?} must not disturb a stopped row"
            );
            assert_eq!(
                derive_status_with_actuator(
                    EndpointState::Unreachable,
                    false,
                    None,
                    false,
                    actuator,
                ),
                CompanyStatus::Missing,
                "{actuator:?} must not disturb a missing row"
            );
        }
    }

    #[test]
    fn an_unobserved_company_prints_its_own_word() {
        assert_eq!(CompanyStatus::Unobserved.label(), "unobserved");
        assert_eq!(CompanyStatus::Unattended.label(), "unattended");
    }

    #[test]
    fn stopped_needs_an_unreachable_endpoint_and_a_session_that_is_not_up() {
        assert_eq!(
            derive_status(EndpointState::Unreachable, false, Some(false)),
            CompanyStatus::Stopped
        );
        // Indeterminate tmux still reads as stopped when nothing answers: the
        // rule is "not provably up", not "provably down".
        assert_eq!(derive_status(EndpointState::Unreachable, false, None), CompanyStatus::Stopped);
        // A session that IS up while the daemon is unreachable is exactly the
        // orphan case, and it must never be called stopped.
        assert_eq!(
            derive_status(EndpointState::Unreachable, false, Some(true)),
            CompanyStatus::Unknown
        );
    }

    #[test]
    fn reachable_but_not_ready_is_never_rounded_to_a_confident_answer() {
        for session in [Some(true), Some(false), None] {
            assert_eq!(
                derive_status(EndpointState::Reachable, true, session),
                CompanyStatus::Unknown
            );
        }
    }

    #[test]
    fn an_empty_registry_renders_just_the_header() {
        assert_eq!(render_table(&[]), vec!["NAME  DIRECTORY  STATUS  CHIEFD  SESSION  PEOPLE"]);
    }

    /// TWO COMPANIES MAY BE CALLED THE SAME THING, and the table has to tell
    /// them apart.
    ///
    /// The whole reason DIRECTORY is a column. Under the retired slug registry
    /// this pair could not exist — one slug was one row — and a listing that
    /// showed only the name would now give an operator two identical rows and
    /// no way to act on either.
    #[test]
    fn two_same_named_companies_are_told_apart_by_their_directories() {
        let lines = render_table(&[
            row_in("acme", "/work/acme", CompanyStatus::Running, "http://127.0.0.1:8791"),
            row_in("acme", "/elsewhere/acme", CompanyStatus::Stopped, "-"),
        ]);
        assert!(lines[1].contains("/work/acme"), "{}", lines[1]);
        assert!(lines[2].contains("/elsewhere/acme"), "{}", lines[2]);
        // And their sessions differ, because the key does.
        assert_ne!(lines[1], lines[2]);
    }

    /// THE DIRECTORY ELLIPSIZES FROM THE LEFT, because a path's last segment
    /// is what names it.
    ///
    /// The column this replaces was CHIEFD, which is a short loopback URL and
    /// never needed shrinking; the directory is the one unbounded cell. Cutting
    /// the TAIL would throw away exactly the part that identifies the company
    /// and leave every deep row reading `/Users/pat/very/long/…`.
    #[test]
    fn a_long_directory_keeps_its_last_segment_and_the_name_is_never_shrunk() {
        let deep = format!("/{}anvils", "very-long-segment/".repeat(9));
        let lines =
            render_table(&[row_in("acme", &deep, CompanyStatus::Running, "http://127.0.0.1:8791")]);
        assert!(lines[1].starts_with("acme"), "{}", lines[1]);
        assert!(lines[1].contains('…'), "the long directory must ellipsize: {}", lines[1]);
        assert!(lines[1].contains("anvils"), "the leaf is what names it: {}", lines[1]);
        assert!(
            !lines[1].contains("/very-long-segment/very-long-segment/very-long-segment/"),
            "the head is what is thrown away: {}",
            lines[1]
        );
    }

    #[test]
    fn a_name_that_already_blows_the_budget_does_not_also_lose_its_directory() {
        // Shrinking DIRECTORY buys nothing once NAME/SESSION alone exceed the
        // width, so it keeps its natural width instead of being mangled too.
        let name = "a".repeat(90);
        let lines = render_table(&[row(&name, CompanyStatus::Running, "http://127.0.0.1:8791")]);
        assert!(lines[1].contains("/work/aaa"), "{}", lines[1]);
    }

    /// The defect: a company whose data root has been deleted was reported
    /// `stopped`, which is the word for a company that is all there and simply
    /// not running.
    #[test]
    fn a_row_with_no_store_database_is_missing_and_never_stopped() {
        assert_eq!(
            derive_status_with_store(EndpointState::Unreachable, false, Some(false), false),
            CompanyStatus::Missing
        );
        // The ghost row exactly as beacond holds it: no location columns at
        // all, so nothing is probed and nothing is answering.
        assert_eq!(
            derive_status_with_store(EndpointState::Unreachable, false, None, false),
            CompanyStatus::Missing
        );
        // An indeterminate probe does not rescue it either: the disk answered.
        assert_eq!(
            derive_status_with_store(EndpointState::Reachable, false, None, false),
            CompanyStatus::Missing
        );
    }

    /// THE BORROWED URL. A row whose directory is gone printed a LIVE
    /// NEIGHBOUR'S daemon address and called itself `unknown`.
    ///
    /// beacond had `http://127.0.0.1:8792` for `acceptance-labs`, whose
    /// directory had been deleted; another company's daemon had taken that
    /// port; the health probe said `ok`; and because `derive_status_with_store`
    /// short-circuits on a healthy endpoint, the disk was never consulted. The
    /// operator was shown a working URL for a company that did not exist —
    /// worse than showing nothing, because a URL invites being opened.
    #[test]
    fn a_row_with_no_company_on_disk_cannot_borrow_a_strangers_healthy_daemon() {
        // The probe really did see a healthy chiefd. It just was not this one.
        assert_eq!(
            trusted_endpoint(EndpointState::Healthy, false),
            EndpointState::Reachable,
            "no daemon can serve a company whose database is not on disk"
        );
        // And the downgrade is what lets the disk decide, so the row finally
        // reaches the word TEST_SUITE.md Case 1 requires.
        assert_eq!(
            derive_status_with_store(
                trusted_endpoint(EndpointState::Healthy, false),
                false,
                None,
                false,
            ),
            CompanyStatus::Missing
        );
        assert_eq!(listed_url("http://127.0.0.1:8792", false), "-");
    }

    /// THE GUARD-RAIL FOR THE SAME CHANGE. A company that IS on disk keeps its
    /// endpoint and its address, including the one case the obvious
    /// alternative would have broken: a healthy daemon of its own that cannot
    /// yet answer for a manifest, mid-genesis.
    #[test]
    fn a_company_still_on_disk_keeps_its_own_endpoint_and_url() {
        for transport in
            [EndpointState::Healthy, EndpointState::Reachable, EndpointState::Unreachable]
        {
            assert_eq!(trusted_endpoint(transport, true), transport);
        }
        assert_eq!(listed_url("http://127.0.0.1:8792", true), "http://127.0.0.1:8792");
        // Mid-genesis: healthy, no manifest yet, database present. It must not
        // be downgraded and must not lose its address.
        assert_eq!(
            derive_status_with_store(
                trusted_endpoint(EndpointState::Healthy, true),
                false,
                None,
                true,
            ),
            CompanyStatus::Unknown
        );
    }

    /// THE GUARD-RAIL. A stopped company is a real state: its database is on
    /// disk, `chief attach` brings it back, and no part of this change may
    /// turn it into a removal.
    #[test]
    fn a_stopped_company_with_its_database_intact_stays_stopped() {
        assert_eq!(
            derive_status_with_store(EndpointState::Unreachable, false, Some(false), true),
            CompanyStatus::Stopped
        );
        assert_eq!(
            derive_status_with_store(EndpointState::Unreachable, false, None, true),
            CompanyStatus::Stopped
        );
    }

    /// A live daemon IS a company, whatever a `stat` says. It holds its own
    /// database open, and an unlinked open file is still a database.
    #[test]
    fn a_healthy_daemon_is_never_called_missing() {
        assert_eq!(
            derive_status_with_store(EndpointState::Healthy, true, Some(true), false),
            CompanyStatus::Running
        );
        // The second arm's ANSWER moved from `unknown` to `runtime-gone` when
        // the probe learned to distinguish "asked and told no" from "could not
        // ask" — see `derive_status`. The property this test defends is
        // untouched and is the one in its name: whatever a healthy daemon's
        // session says, the row is not `missing`.
        assert_eq!(
            derive_status_with_store(EndpointState::Healthy, true, Some(false), false),
            CompanyStatus::RuntimeGone
        );
        assert_ne!(
            derive_status_with_store(EndpointState::Healthy, true, Some(false), false),
            CompanyStatus::Missing
        );
    }

    /// THE 22:17:40 OUTAGE, AS THE OPERATOR'S TABLE SAW IT.
    ///
    /// The company's whole tmux server was gone while its daemon stayed healthy
    /// and still answered with seven people. `chief ls` printed `unknown` —
    /// the word an operator scrolls past — because a session that was PROVABLY
    /// absent and a session that could not be PROBED fell into the same bucket.
    /// They are opposite facts and only one of them is news.
    #[test]
    fn a_healthy_company_whose_session_is_provably_gone_says_so_and_is_not_unknown() {
        assert_eq!(
            derive_status(EndpointState::Healthy, true, Some(false)),
            CompanyStatus::RuntimeGone
        );
        assert_eq!(CompanyStatus::RuntimeGone.label(), "runtime-gone");
    }

    /// And the other half of the same distinction: a tmux read that did not
    /// complete proves nothing, and must keep saying so.
    #[test]
    fn a_session_that_could_not_be_probed_is_still_unknown() {
        assert_eq!(derive_status(EndpointState::Healthy, true, None), CompanyStatus::Unknown);
    }

    /// `missing` is decided outside [`derive_status`], so that rule keeps
    /// exactly the outputs its own doc claims for it.
    ///
    /// The count moved from three to four with `runtime-gone`, which is a
    /// deliberate widening of what the probe rule can say and not a leak: the
    /// assertion that matters — that this rule can never invent `missing` — is
    /// unchanged and is checked on every combination below.
    #[test]
    fn the_probe_rule_itself_never_invents_missing_and_has_exactly_four_outputs() {
        let mut seen = Vec::new();
        for endpoint in
            [EndpointState::Healthy, EndpointState::Reachable, EndpointState::Unreachable]
        {
            for manifest in [true, false] {
                for session in [Some(true), Some(false), None] {
                    let status = derive_status(endpoint, manifest, session);
                    assert_ne!(
                        status,
                        CompanyStatus::Missing,
                        "the probe rule must never invent `missing`"
                    );
                    if !seen.contains(&status) {
                        seen.push(status);
                    }
                }
            }
        }
        assert_eq!(seen.len(), 4, "running, stopped, runtime-gone, unknown: {seen:?}");
    }

    #[test]
    fn every_status_has_its_own_word() {
        let mut labels = [
            CompanyStatus::Running.label(),
            CompanyStatus::Stopped.label(),
            CompanyStatus::Missing.label(),
            CompanyStatus::Unknown.label(),
        ];
        assert_eq!(labels, ["running", "stopped", "missing", "unknown"]);
        labels.sort_unstable();
        let mut unique = labels.to_vec();
        unique.dedup();
        assert_eq!(unique.len(), 4);
    }
    /// #1207. The state neither half can see alone: tmux says the actuator
    /// window is RUNNING, chiefd says nobody has read the desired set. That is
    /// a live supervisor over a child that cannot get up, and it is exactly
    /// what the operator stared at for two hours while `chief ls` said
    /// `running`.
    #[test]
    fn a_running_actuator_that_chiefd_cannot_hear_is_unattended() {
        assert_eq!(
            derive_status_with_attendance(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Running,
                Some(false),
            ),
            CompanyStatus::Unattended
        );
    }

    /// The three arms that must NOT change, because each of them was already
    /// the true word.
    #[test]
    fn attendance_only_ever_reclassifies_the_row_that_claimed_running() {
        // Heard from: unchanged.
        assert_eq!(
            derive_status_with_attendance(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Running,
                Some(true),
            ),
            CompanyStatus::Running
        );
        // A daemon too old to carry the fact leaves the row exactly as it was:
        // silence from the WIRE is not silence from the ACTUATOR.
        assert_eq!(
            derive_status_with_attendance(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Running,
                None,
            ),
            CompanyStatus::Running
        );
        // tmux's word wins when the window is not there: `unobserved` stays
        // `unobserved` however loud chiefd's silence is.
        assert_eq!(
            derive_status_with_attendance(
                EndpointState::Healthy,
                true,
                Some(true),
                true,
                ActuatorSession::Absent,
                Some(false),
            ),
            CompanyStatus::Unobserved
        );
        // And a company that is switched off is `stopped`, not accused.
        assert_eq!(
            derive_status_with_attendance(
                EndpointState::Unreachable,
                false,
                Some(false),
                true,
                ActuatorSession::Unknown,
                Some(false),
            ),
            CompanyStatus::Stopped
        );
    }

    #[test]
    fn an_unattended_company_prints_its_own_word_with_the_duration() {
        assert_eq!(
            status_cell(CompanyStatus::Unattended, Some(Duration::from_secs(7383))),
            "unattended 2h 3m"
        );
        // Without a number it is still the honest word, just less useful.
        assert_eq!(status_cell(CompanyStatus::Unattended, None), "unattended");
        // Every other word prints exactly as it always has, duration or not.
        assert_eq!(status_cell(CompanyStatus::Running, Some(Duration::from_secs(99))), "running");
        assert_eq!(status_cell(CompanyStatus::Unobserved, None), "unobserved");
    }
}
