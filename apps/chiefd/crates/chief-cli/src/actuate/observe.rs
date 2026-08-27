//! Observe live tmux into an M1 [`ObservedTopology`](plan::ObservedTopology).
//!
//! The observe half of the cycle, and the enforcement point for a Q3.2
//! invariant M1 (being pure) cannot itself hold: **"observation failed" must be
//! distinguishable from "observed empty", and must fail closed.** A failed or
//! untrusted read returns `Err` here, so the caller never fabricates an empty
//! topology and plans a teardown from it; a genuinely absent session returns
//! `Ok` with `session_exists = false`, which M1 reads as "create it".
//!
//! Reads go through [`HostExecutor::tmux`], so the trait's transient-retry
//! ladder applies and an exhausted ladder surfaces as [`HostErr::Untrusted`] —
//! never as absence. Presence and the raw ownership tags are classified with the
//! same pure trust rules the real executor's audit uses
//! ([`crate::actuate::classify_presence`]); this is a raw-tag observe (M1 needs the
//! per-object window identity that the filtered `audit_session`
//! discards), not a second copy of the ownership verdict.

use crate::actuate::plan;

use crate::actuate::ever_observed::EverObserved;
use std::collections::BTreeMap;

use crate::actuate::host::{HostErr, HostExecutor, Socket, TmuxCmd};
use crate::actuate::trust::tags;
use crate::actuate::{classify_presence, SessionPresence};

struct ViewportReapFence<'a> {
    executor: &'a dyn HostExecutor,
    socket: &'a Socket,
    session: &'a str,
    generation: Option<String>,
}

impl ViewportReapFence<'_> {
    fn invalidate(&mut self) -> Result<(), ObserveError> {
        if self.generation.is_some() {
            return Ok(());
        }
        self.generation = Some(
            super::interpret::invalidate_viewport_manifest(
                self.executor,
                self.socket,
                self.session,
            )
            .map_err(|detail| ObserveError::Failed {
                session: self.session.to_owned(),
                verb: "invalidate viewport manifest before torn-object reap".to_owned(),
                detail,
            })?,
        );
        Ok(())
    }
}

impl Drop for ViewportReapFence<'_> {
    fn drop(&mut self) {
        if let Some(generation) = self.generation.as_deref() {
            super::interpret::request_viewport_manifest_refresh(
                self.executor,
                self.socket,
                self.session,
                generation,
            );
        }
    }
}

/// Why an observation could not be trusted. Every variant fails the cycle
/// closed: the caller must not plan from a fabricated empty topology.
#[derive(Debug, thiserror::Error)]
pub enum ObserveError {
    /// tmux did not answer authoritatively (transient ladder exhausted, or an
    /// unrecognized diagnostic). Never evidence of absence.
    #[error("observation untrusted: {0}")]
    Untrusted(#[source] HostErr),
    /// A read tmux *did* answer, but with a failure the observe cannot proceed
    /// past (e.g. listing a session's windows failed).
    #[error("observation of session '{session}' failed at {verb}: {detail}")]
    Failed {
        /// The session being observed.
        session: String,
        /// The tmux verb that failed.
        verb: String,
        /// Redacted stderr.
        detail: String,
    },
}

/// Observe the live tmux topology of one company's session.
///
/// Whole-session observation is never destructive. Session creation is one
/// tmux server command queue, so an interrupted client leaves either no
/// session or the fully tagged session. An empty ownership read is ambiguous
/// and fails closed without mutation.
///
/// #18 P2 / task #23: also reaps any window/pane still carrying the
/// chiefd-internal `tags::MINTING` marker — a mint from a previous
/// `apply_plan` call whose actuator died before finishing that object's
/// identity-tag sequence. The marker is read as an EXTRA trailing field on
/// the SAME `list-windows`/`list-panes` calls this function already makes
/// (no additional tmux round-trip), and a marked object is destroyed
/// (`kill-window`/`kill-pane` — the only new calls, and only issued when a
/// torn object genuinely exists) and excluded from the returned topology,
/// rather than surfacing as a permanently-fatal or permanently-duplicated
/// object to the caller's planning pass. See `reap_torn_mints`'s prior
/// design note in `interpret.rs` for why REAPING (not resuming) is the safe
/// recovery: the marker alone does not durably record which logical window
/// or person the interrupted mint was for, so there is nothing to safely
/// resume, only something safe to remove.
///
/// This makes `observe` no longer purely read-only in the rare case a torn
/// mint exists — an intentional, narrowly-scoped exception to the module's
/// usual "observe never mutates" rule, chosen over a separate pre-pass sweep
/// specifically to avoid adding tmux round-trips to every ordinary pass.
///
/// `ever_observed`: #739 P3's positive-evidence registry. Must be a
/// PER-COMPANY instance held across repeated calls to this function over
/// the company's converge loop lifetime, never constructed fresh per call
/// — a fresh registry can never accumulate "ever," which is the entire
/// property P3 needs. The caller owns that lifetime; this function only
/// writes into it.
///
/// # Errors
/// [`ObserveError`] on any untrusted or failed read. A *provably absent* session
/// is not an error: it returns `Ok` with `session_exists = false`.
pub fn observe(
    executor: &dyn HostExecutor,
    socket: &Socket,
    session: &str,
    ever_observed: &EverObserved,
) -> Result<plan::ObservedTopology, ObserveError> {
    let presence = session_presence(executor, socket, session)?;
    if presence == SessionPresence::ProvablyAbsent {
        return Ok(plan::ObservedTopology {
            session_exists: false,
            session_organization: String::new(),
            windows: Vec::new(),
            panes: Vec::new(),
        });
    }

    let session_organization = read_session_organization(executor, socket, session)?;
    if session_organization.is_empty() {
        return Err(ObserveError::Failed {
            session: session.to_owned(),
            verb: "read session ownership".to_owned(),
            detail: "the organization tag is empty; whole-session observation fails closed"
                .to_owned(),
        });
    }

    // #18 P2 / task #23: tmux destroys a session whose last window's last
    // pane dies, so reaping a torn object below can cascade into destroying
    // the session itself out from under this same read. Tracked so both loops
    // below can re-check presence rather than either hard-failing the next
    // tmux call against a session that just vanished, or returning a stale
    // topology describing objects tmux no longer has.
    let mut reaped_anything = false;
    let mut viewport_reap = ViewportReapFence { executor, socket, session, generation: None };

    let mut window_ids: Vec<String> = Vec::new();
    for line in
        list(executor, socket, session, "list-windows", "#{window_id}\t#{@organization_minting}")?
    {
        // A reply scripted before this field existed carries no tab and no
        // second field, which reads as "not minting" — tolerant on purpose,
        // see the fn doc.
        let mut fields = line.splitn(2, '\t');
        let window_id = fields.next().unwrap_or_default().trim().to_owned();
        if window_id.is_empty() {
            continue;
        }
        if fields.next().is_some_and(|marker| !marker.trim().is_empty()) {
            tracing::warn!(
                window = %window_id,
                "converge: reaping a window whose tag sequence was interrupted before completion (#18 P2, task #23)"
            );
            viewport_reap.invalidate()?;
            let _ = executor
                .tmux(socket, TmuxCmd { argv: vec!["kill-window".into(), "-t".into(), window_id] });
            reaped_anything = true;
            continue;
        }
        // COLLECTED, not read. Every surviving object's tags are fetched in ONE
        // invocation below — see `read_all_tags`.
        window_ids.push(window_id);
    }

    // A window reap above may have just cascaded the session away. Re-check
    // before the pane list: without this, `list-panes` against a session the
    // reap itself just removed fails closed with an untrusted/failed read,
    // when the true, correct observation is the ordinary "provably absent"
    // case every other caller already handles.
    if reaped_anything
        && session_presence(executor, socket, session)? == SessionPresence::ProvablyAbsent
    {
        return Ok(plan::ObservedTopology {
            session_exists: false,
            session_organization: String::new(),
            windows: Vec::new(),
            panes: Vec::new(),
        });
    }

    let mut panes = Vec::new();
    let mut pane_rows: Vec<(String, String, String)> = Vec::new();
    // `#{pane_start_command}` is captured so M1 can attribute an untagged orphan
    // to its person via the crash-surviving `ORG_LAUNCHER_PERSON=` env in the
    // argv (#64). It can contain spaces but not tabs, so `splitn(4, '\t')` keeps
    // the whole command as the third field and the minting marker (#18 P2) as
    // an optional fourth.
    for line in list(
        executor,
        socket,
        session,
        "list-panes",
        "#{pane_id}\t#{window_id}\t#{pane_start_command}\t#{@organization_minting}",
    )? {
        let mut fields = line.splitn(4, '\t');
        let (Some(pane_id), Some(window_id)) =
            (fields.next().map(str::trim), fields.next().map(str::trim))
        else {
            return Err(ObserveError::Failed {
                session: session.to_owned(),
                verb: "list-panes".to_owned(),
                detail: format!("unusable pane row {line:?}"),
            });
        };
        // A pane with no start command (or from a reply scripted before the
        // minting field existed) reports an empty/absent third and fourth
        // field — both tolerated as "no command" / "not minting".
        let start_command = fields.next().unwrap_or("").trim().to_owned();
        if fields.next().is_some_and(|marker| !marker.trim().is_empty()) {
            tracing::warn!(
                pane = %pane_id,
                "converge: reaping a pane whose tag sequence was interrupted before completion (#18 P2, task #23)"
            );
            viewport_reap.invalidate()?;
            let _ = executor.tmux(
                socket,
                TmuxCmd { argv: vec!["kill-pane".into(), "-t".into(), pane_id.to_owned()] },
            );
            reaped_anything = true;
            continue;
        }
        // Read every tag as a named binding, in the ORDER the four tag reads
        // are issued: organization, window, person, launch hash. Order is not
        // cosmetic here — each `read_tag` is one `show-options` round trip, so
        // the sequence IS this function's wire contract with the executor, and
        // the scripted executor the unit tests drive replies positionally. A
        // `person_id` hoisted above the struct literal (to mark it into the
        // registry below) silently moved the person read ahead of the
        // organization read, and every pane came back with its neighbour's
        // value — org read as "eng", person read as "cobalt". Binding all four
        // up front keeps the emitted order fixed no matter what later code
        // needs to inspect before the struct is built.
        pane_rows.push((pane_id.to_owned(), window_id.to_owned(), start_command));
    }

    // ONE INVOCATION FOR EVERY OBJECT'S TAGS. See `read_all_tags` for why the
    // spawn count is the only thing that matters here.
    let mut objects: Vec<(&'static str, String)> =
        window_ids.iter().map(|id| ("-w", id.clone())).collect();
    objects.extend(pane_rows.iter().map(|(pane, _, _)| ("-p", pane.clone())));
    let tagged = read_all_tags(executor, socket, &objects)?;
    let empty = BTreeMap::new();
    let mut windows: Vec<plan::ObservedWindow> = window_ids
        .into_iter()
        .map(|window_id| {
            let own = tagged.get(&window_id).unwrap_or(&empty);
            plan::ObservedWindow {
                organization_id: own.get(tags::ORGANIZATION).cloned().unwrap_or_default(),
                logical_id: own.get(tags::WINDOW).cloned().unwrap_or_default(),
                tmux_id: window_id,
                protected_ui: false,
                sleeping_notice: false,
            }
        })
        .collect();
    for (pane_id, window_id, start_command) in pane_rows {
        let own = tagged.get(&pane_id).unwrap_or(&empty);
        // The rail owns these panes. They are not people and must not enter the
        // planner's stray-pane quarantine. `read_all_tags` already returned
        // every PANE-LOCAL option, so this classification adds no tmux call and
        // cannot inherit a marker from the pane's window.
        //
        // Fail closed on ownership: even one ownership option means this is no
        // longer clean furniture. Keep that pane in the observation so the
        // existing partial-tag quarantine can protect it. Unknown panes also
        // stay observed; only Chief's exact marker values are furniture.
        if is_unowned_chief_furniture(own) {
            if let Some(window) = windows.iter_mut().find(|window| window.tmux_id == window_id) {
                window.protected_ui = true;
                let exact_sleeping_notice =
                    own.get(tags::ASLEEP).is_some_and(|department| !department.is_empty())
                        && !own.contains_key(tags::SIDEBAR)
                        && !own.contains_key(tags::WAKING_PERSON)
                        && !own.contains_key(tags::SLEEPING_PERSON)
                        && !own.contains_key(tags::MINTING);
                if exact_sleeping_notice {
                    window.sleeping_notice = true;
                }
            }
            continue;
        }
        let organization_id = own.get(tags::ORGANIZATION).cloned().unwrap_or_default();
        let logical_window_id = own.get(tags::WINDOW).cloned().unwrap_or_default();
        let person_id = own.get(tags::PERSON).cloned().unwrap_or_default();
        let launch_hash = own.get(tags::LAUNCH_HASH).cloned().unwrap_or_default();
        // #739 P3: this pane was just observed alive, tagged, on THIS
        // socket, right now -- exactly the positive evidence the design
        // doc requires. An empty person_id (tag absent) is not a person to
        // mark; nothing here corresponds to "everyone."
        if !person_id.is_empty() {
            ever_observed.mark_observed(&person_id);
        }
        panes.push(plan::ObservedPane {
            organization_id,
            logical_window_id,
            person_id,
            launch_hash,
            tmux_id: pane_id.to_owned(),
            tmux_window_id: window_id.to_owned(),
            start_command,
        });
    }

    // A pane reap above may ALSO have just cascaded the window, then the
    // session, away — the pane's window and the pane itself are the only
    // things this function has already committed to reading by that point, so
    // (unlike the window-loop case) nothing errors, but returning the
    // `windows`/`panes` collected before the cascade would describe objects
    // tmux no longer has. One more presence check keeps the returned
    // topology honest rather than stale.
    if reaped_anything
        && session_presence(executor, socket, session)? == SessionPresence::ProvablyAbsent
    {
        return Ok(plan::ObservedTopology {
            session_exists: false,
            session_organization: String::new(),
            windows: Vec::new(),
            panes: Vec::new(),
        });
    }

    Ok(plan::ObservedTopology { session_exists: true, session_organization, windows, panes })
}

/// Whether a pane is clean furniture owned by the Chief rail.
///
/// Ownership means the option exists, not that its value is nonempty. An empty
/// ownership option is still partial tagging and must remain visible to the
/// planner's fail-closed quarantine.
fn is_unowned_chief_furniture(options: &BTreeMap<String, String>) -> bool {
    let has_ownership = [tags::ORGANIZATION, tags::WINDOW, tags::PERSON, tags::LAUNCH_HASH]
        .iter()
        .any(|tag| options.contains_key(*tag));
    if has_ownership {
        return false;
    }

    options.get(tags::SIDEBAR).is_some_and(|value| value == "1")
        || options.get(tags::ASLEEP).is_some_and(|value| !value.is_empty())
        || options.get(tags::WAKING_PERSON).is_some_and(|value| !value.is_empty())
        || options.get(tags::SLEEPING_PERSON).is_some_and(|value| !value.is_empty())
}

fn session_presence(
    executor: &dyn HostExecutor,
    socket: &Socket,
    session: &str,
) -> Result<SessionPresence, ObserveError> {
    let out = executor
        .tmux(socket, TmuxCmd { argv: vec!["has-session".into(), "-t".into(), session.into()] })
        .map_err(ObserveError::Untrusted)?;
    match classify_presence(out.status, &out.stdout, &out.stderr) {
        proven @ (SessionPresence::Present | SessionPresence::ProvablyAbsent) => Ok(proven),
        SessionPresence::Unproven(_) => Err(ObserveError::Untrusted(HostErr::Untrusted {
            reason: "has-session did not answer authoritatively; presence is unproven".into(),
        })),
    }
}

/// List ids/rows under a session, failing closed on a non-zero exit.
fn list(
    executor: &dyn HostExecutor,
    socket: &Socket,
    session: &str,
    verb: &str,
    format: &str,
) -> Result<Vec<String>, ObserveError> {
    let mut argv = vec![verb.to_owned()];
    if verb == "list-panes" {
        argv.push("-s".to_owned());
    }
    argv.extend(["-t".to_owned(), session.to_owned(), "-F".to_owned(), format.to_owned()]);
    let out = executor.tmux(socket, TmuxCmd { argv }).map_err(ObserveError::Untrusted)?;
    if out.status != 0 {
        return Err(ObserveError::Failed {
            session: session.to_owned(),
            verb: verb.to_owned(),
            detail: out.stderr.trim().to_owned(),
        });
    }
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

// TOMBSTONE: `session_selection`, deleted with `sidebar_options::SELECTION`.
// The actuator read the operator's marker out of a tmux option because it was
// a SEPARATE PROCESS from every rail and that option was the only thing they
// shared. It is the same process as the rail's brain now, so converge asks it
// directly (`sidebar::brain::Handle::focus`) — which is also why the gesture
// correlator no longer needs a bus: a click reaches the placement pass by a
// field read rather than by a round trip through tmux.

/// Resolve the exact named session, then read only its local organization.
///
/// The name is compared in Rust instead of passed as a tmux target. Tmux 3.3a
/// accepts prefix targets, so a target read can report a different session.
/// `list-sessions` gives the stable `$N` id used for the local option read.
/// The second call uses `show-options` because tmux formats inherit global
/// user options; a global value is never authority over this session.
fn read_session_organization(
    executor: &dyn HostExecutor,
    socket: &Socket,
    session: &str,
) -> Result<String, ObserveError> {
    let out = executor
        .tmux(
            socket,
            TmuxCmd {
                argv: vec![
                    "list-sessions".to_owned(),
                    "-F".to_owned(),
                    "#{session_name}\t#{session_id}".to_owned(),
                ],
            },
        )
        .map_err(ObserveError::Untrusted)?;
    if out.status != 0 {
        return Err(ObserveError::Untrusted(HostErr::Untrusted {
            reason:
                "tmux could not report the session identity; the observation is not trustworthy"
                    .into(),
        }));
    }
    let mut exact_id = None;
    for line in out.stdout.lines() {
        let mut fields = line.splitn(2, '\t');
        if fields.next().unwrap_or_default() == session {
            let tmux_id = fields.next().unwrap_or_default().trim().to_owned();
            if !tmux_id.starts_with('$') || tmux_id[1..].chars().any(|ch| !ch.is_ascii_digit()) {
                break;
            }
            exact_id = Some(tmux_id);
            break;
        }
    }
    let tmux_id = exact_id.ok_or_else(|| {
        ObserveError::Untrusted(HostErr::Untrusted {
            reason:
                "tmux did not report the exact named session; the observation is not trustworthy"
                    .into(),
        })
    })?;
    let local = executor
        .tmux(
            socket,
            TmuxCmd {
                argv: vec![
                    "show-options".to_owned(),
                    "-q".to_owned(),
                    "-t".to_owned(),
                    tmux_id.clone(),
                ],
            },
        )
        .map_err(ObserveError::Untrusted)?;
    if local.status != 0 {
        return Err(ObserveError::Untrusted(HostErr::Untrusted {
            reason:
                "tmux could not report the session-local identity; the observation is not trustworthy"
                    .into(),
        }));
    }
    let mut organization = String::new();
    for line in local.stdout.lines() {
        let Some((name, value)) = line.trim().split_once(' ') else { continue };
        if name == tags::ORGANIZATION {
            organization = value.trim().trim_matches('"').to_owned();
        }
    }
    Ok(organization)
}

/// The line that opens one object's block in a batched tag read.
///
/// Option lines always begin `@`, so ANY line that does not is a marker. That is
/// the whole parse: no escaping and no counting, and no ambiguity from an option
/// VALUE that looks like a marker, because a value never starts a line.
///
/// Emitted through `-F` and never as a literal message: tmux treats `%`
/// specially in a literal `display-message` string and a pane id starts with one
/// — `display-message -p -t %5 "OBJ %5"` prints `OBJ` followed by a hundred
/// spaces and then `%5`. Measured, not guessed.
const OBJECT_MARKER: &str = "OBJ ";

/// Every ownership tag of EVERY object, in ONE tmux invocation.
///
/// # Why one invocation is the whole point
///
/// A tmux command is a PROCESS: fork, exec the tmux binary, connect to the unix
/// socket, take a reply, exit. That round trip is ~25ms even with the server on
/// the same machine, and it dominates — tmux's own work is microseconds. The
/// cost of an observation is therefore `spawns x 25ms`, and the spawn count is
/// the only lever there is.
///
/// A tag at a time was twenty-six spawns on the operator's own company
/// (~700ms). An object at a time was eleven (~290ms). This is ONE, whatever the
/// company's size — so a whole observation is six invocations (presence, exact
/// session id, local session options, the two listings, and this), and that
/// number no longer grows with the number of people.
///
/// # The shapes that look simpler and are wrong
///
/// Measured against a real tmux rather than reasoned about, and the first is a
/// safety hole:
///
/// * `list-panes -F "#{@tag}"` INHERITS. With a pane's own option unset it
///   returns the WINDOW's value, so a stray pane inside one of our windows reads
///   as fully ownership-tagged and the #410/#438 quarantine is defeated
///   silently.
/// * `list-windows -F "#{@tag}"` returns the ACTIVE PANE's value, not the
///   window's — caught by a crash test as a window minted `ops` reading back
///   `eng`, its surviving pane's old home.
/// * Chaining `show-options -qv` per tag drops the LINE entirely for an unset
///   option, so a positional read shifts every value up by one the moment any
///   tag is absent.
///
/// `show-options -p` lists only that pane's own options and `-w` only that
/// window's — no inheritance, verified — and an object with no tags contributes
/// only its marker, which is exactly how a stray stays a stray.
fn read_all_tags(
    executor: &dyn HostExecutor,
    socket: &Socket,
    objects: &[(&'static str, String)],
) -> Result<BTreeMap<String, BTreeMap<String, String>>, ObserveError> {
    let mut tagged: BTreeMap<String, BTreeMap<String, String>> =
        objects.iter().map(|(_, id)| (id.clone(), BTreeMap::new())).collect();
    if objects.is_empty() {
        return Ok(tagged);
    }
    let mut argv: Vec<String> = Vec::with_capacity(objects.len() * 12);
    for (scope, id) in objects {
        if !argv.is_empty() {
            argv.push(";".to_owned());
        }
        let field = if *scope == "-w" { "window_id" } else { "pane_id" };
        argv.extend([
            "display-message".to_owned(),
            "-p".to_owned(),
            "-t".to_owned(),
            id.clone(),
            "-F".to_owned(),
            format!("{OBJECT_MARKER}#{{{field}}}"),
            ";".to_owned(),
            "show-options".to_owned(),
            (*scope).to_owned(),
            "-t".to_owned(),
            id.clone(),
        ]);
    }
    let out = executor.tmux(socket, TmuxCmd { argv }).map_err(ObserveError::Untrusted)?;
    if out.status != 0 {
        return Err(ObserveError::Untrusted(HostErr::Untrusted {
            reason: "tmux could not report the ownership tags; the observation is not trustworthy"
                .into(),
        }));
    }
    let mut current: Option<String> = None;
    for line in out.stdout.lines() {
        let line = line.trim();
        if let Some(id) = line.strip_prefix(OBJECT_MARKER) {
            current = Some(id.trim().to_owned());
            continue;
        }
        // Not a marker and not an option line: tmux talking about something
        // else. Ignored rather than mis-attributed to the open block.
        if !line.starts_with('@') {
            continue;
        }
        let (Some(id), Some((name, value))) = (current.as_ref(), line.split_once(' ')) else {
            continue;
        };
        if let Some(entry) = tagged.get_mut(id) {
            // tmux quotes a value containing spaces. Ownership tags are ids and
            // never do, but stripping keeps a quoted reply from carrying its
            // quotes into an equality test.
            entry.insert(name.trim().to_owned(), value.trim().trim_matches('"').to_owned());
        }
    }
    Ok(tagged)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::is_unowned_chief_furniture;
    use crate::actuate::fake::{ScriptedReply, ScriptedTmux};
    use crate::actuate::host::{HostErr, TmuxCmd, TmuxOut};
    use crate::actuate::runner::{RecordingWaiter, SystemTmuxRunner, TmuxRunner};
    use crate::actuate::trust::tags;
    use crate::actuate::TmuxHost;
    use crate::actuate::*;
    use crate::proc::ProcReader;
    use crate::real::RealHostExecutor;

    fn exec(scripted: ScriptedTmux) -> RealHostExecutor<ScriptedTmux, RecordingWaiter> {
        RealHostExecutor::new(
            TmuxHost::new(scripted, RecordingWaiter::default()),
            ProcReader::default(),
        )
    }

    fn socket() -> Socket {
        Socket("chiefd-test".into())
    }

    #[test]
    fn a_sleeping_card_is_protected_chief_furniture_not_an_unowned_stray() {
        let options = BTreeMap::from([(tags::SLEEPING_PERSON.to_owned(), "vera".to_owned())]);
        assert!(is_unowned_chief_furniture(&options));
    }

    struct EmptySessionIdentityOnce {
        inner: SystemTmuxRunner,
        pending: std::sync::atomic::AtomicBool,
    }

    impl TmuxRunner for EmptySessionIdentityOnce {
        fn run(&self, socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr> {
            let identity_read = cmd.argv.first().is_some_and(|verb| verb == "show-options")
                && cmd.argv.iter().any(|arg| arg == "-q")
                && !cmd.argv.iter().any(|arg| arg == "-v");
            if identity_read && self.pending.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let mut out = self.inner.run(socket, cmd)?;
                out.stdout = out
                    .stdout
                    .lines()
                    .filter(|line| !line.starts_with("@organization_id "))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(out);
            }
            self.inner.run(socket, cmd)
        }
    }

    fn real_tmux(socket: &Socket, argv: &[&str]) -> TmuxOut {
        SystemTmuxRunner::default()
            .run(socket, &TmuxCmd { argv: argv.iter().map(|word| (*word).to_owned()).collect() })
            .expect("tmux is available")
    }

    fn observe_one_pane_with(options: &str) -> plan::ObservedTopology {
        let object_options = if options.is_empty() {
            "OBJ @1\n@organization_id cobalt\n@organization_window_id eng\nOBJ %5".to_owned()
        } else {
            format!(
                "OBJ @1\n@organization_id cobalt\n@organization_window_id eng\nOBJ %5\n{options}"
            )
        };
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("cobalt-session\t$1"),
            ScriptedReply::ok("@organization_id cobalt"),
            ScriptedReply::ok("@1"),
            ScriptedReply::ok("%5\t@1\tchief furniture"),
            ScriptedReply::ok(&object_options),
        ]);
        observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect("present")
    }

    #[test]
    fn an_unowned_sidebar_is_omitted_from_person_observation() {
        let observed = observe_one_pane_with("@organization_sidebar 1");
        assert!(observed.panes.is_empty(), "the rail is Chief furniture, not a stray person pane");
        assert!(observed.windows[0].protected_ui, "the rail protects its live UI window");
    }

    #[test]
    fn unowned_asleep_and_focus_panels_are_omitted_from_person_observation() {
        for value in ["research", crate::placement::FOCUS_WINDOW_ID] {
            let observed = observe_one_pane_with(&format!("@chief_asleep_for {value}"));
            assert!(
                observed.panes.is_empty(),
                "an asleep/focus panel marked {value:?} is Chief furniture"
            );
            assert!(
                observed.windows[0].protected_ui,
                "an asleep/focus panel marked {value:?} protects its window"
            );
            assert!(
                observed.windows[0].sleeping_notice,
                "the planner receives the exact notice fact"
            );
        }
    }

    /// A WAKING BODY IS FURNITURE AND NOTHING MORE.
    ///
    /// It used to be carried to the planner as `ObservedWindow::waking_focus`,
    /// the exact existing pane converge was allowed to CLAIM with
    /// `respawn-pane` so a cold click's "… is starting" cell became the
    /// person's own pane. One window per person deleted the claim — a woken
    /// person is placed in a window of their own, not in the rail's card
    /// window — so all this observation owes the planner is that the pane is
    /// not a person and its window is protected UI.
    #[test]
    fn a_waking_body_is_protected_furniture_and_never_person_ownership() {
        for markers in
            ["@chief_waking_person eli", "@chief_asleep_for __focus__\n@chief_waking_person eli"]
        {
            let scripted = ScriptedTmux::new([
                ScriptedReply::ok(""),
                ScriptedReply::ok("cobalt-session\t$1"),
                ScriptedReply::ok("@organization_id cobalt"),
                ScriptedReply::ok("@1"),
                ScriptedReply::ok("%755\t@1\t/bin/sh"),
                ScriptedReply::ok(&format!(
                    "OBJ @1\n@organization_id cobalt\n@organization_window_id __focus__\n\
                     OBJ %755\n{markers}"
                )),
            ]);
            let observed =
                observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
                    .expect("waking focus observation");

            assert!(observed.panes.is_empty(), "waking furniture is not person ownership");
            assert!(
                observed.windows[0].protected_ui,
                "the rail's card window is not an empty managed window"
            );
        }
    }

    #[test]
    fn mixed_waking_and_asleep_markers_are_not_the_exact_sleeping_notice() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("cobalt-session\t$1"),
            ScriptedReply::ok("@organization_id cobalt"),
            ScriptedReply::ok("@1"),
            ScriptedReply::ok("%755\t@1\t/bin/sh"),
            ScriptedReply::ok(
                "OBJ @1\n@organization_id cobalt\n@organization_window_id __focus__\n\
                 OBJ %755\n@chief_asleep_for __focus__\n@chief_waking_person eli",
            ),
        ]);
        let observed = observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect("mixed furniture observation");
        assert!(
            !observed.windows[0].sleeping_notice,
            "mixed furniture is not the exact notice that converge may retire"
        );
    }

    #[test]
    fn unknown_unowned_panes_remain_observed_for_quarantine() {
        for options in ["", "@organization_sidebar 0", "@unknown_furniture 1"] {
            let observed = observe_one_pane_with(options);
            assert_eq!(
                observed.panes.len(),
                1,
                "unknown options {options:?} must not bypass quarantine"
            );
            assert!(
                !observed.windows[0].protected_ui,
                "unknown options {options:?} must not protect the window"
            );
        }
    }

    #[test]
    fn furniture_with_any_ownership_option_remains_observed_for_quarantine() {
        for ownership in [
            "@organization_id cobalt",
            "@organization_window_id eng",
            "@organization_person_id vera",
            "@organization_launch_hash hash",
        ] {
            let observed = observe_one_pane_with(&format!("@organization_sidebar 1\n{ownership}"));
            assert_eq!(
                observed.panes.len(),
                1,
                "partial ownership {ownership:?} must override the furniture marker"
            );
            assert!(
                !observed.windows[0].protected_ui,
                "partial ownership {ownership:?} must remain quarantined, not protected UI"
            );
        }
    }

    /// AN OBJECT WITH NO TAGS CONTRIBUTES ONLY ITS MARKER, and that is how a
    /// stray stays a stray.
    ///
    /// This is the load-bearing half of the batching. Format strings were
    /// rejected for this job precisely because `#{@tag}` INHERITS — an untagged
    /// pane inside one of our windows reads back the WINDOW's tags and therefore
    /// looks fully owned, which defeats the #410/#438 quarantine silently.
    /// `show-options -p` lists only the pane's OWN options, so an untagged pane
    /// emits nothing between its marker and the next.
    ///
    /// The neighbours are asserted too: an empty block must not shift the blocks
    /// on either side of it, which is the failure mode that killed the chained
    /// `show-options -qv` shape.
    #[test]
    fn an_untagged_pane_between_two_tagged_ones_stays_untagged() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("cobalt-session\t$1"),
            ScriptedReply::ok("@organization_id cobalt"),
            ScriptedReply::ok("@1"),
            ScriptedReply::ok("%5\t@1\tpi\n%6\t@1\tsh\n%7\t@1\tpi"),
            ScriptedReply::ok(
                "OBJ @1\n@organization_id cobalt\n@organization_window_id eng\n\
                 OBJ %5\n@organization_id cobalt\n@organization_window_id eng\n\
                 @organization_person_id vera\n@organization_launch_hash aaa\n\
                 OBJ %6\n\
                 OBJ %7\n@organization_id cobalt\n@organization_window_id eng\n\
                 @organization_person_id theo\n@organization_launch_hash bbb",
            ),
        ]);
        let observed = observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect("present");

        let stray = observed.panes.iter().find(|p| p.tmux_id == "%6").expect("the stray is listed");
        assert!(
            stray.organization_id.is_empty()
                && stray.person_id.is_empty()
                && stray.logical_window_id.is_empty(),
            "the stray inherited NOTHING from the window it sits in — this is the whole \
             reason a format string could not be used here: {stray:?}"
        );
        let vera = observed.panes.iter().find(|p| p.tmux_id == "%5").expect("%5");
        let theo = observed.panes.iter().find(|p| p.tmux_id == "%7").expect("%7");
        assert_eq!(vera.person_id, "vera", "the block BEFORE the stray is intact");
        assert_eq!(theo.person_id, "theo", "as is the one after it — no positional shift");
        assert_eq!(vera.launch_hash, "aaa");
        assert_eq!(theo.launch_hash, "bbb");
    }

    #[test]
    fn a_provably_absent_session_is_observed_empty_not_failed() {
        let scripted = ScriptedTmux::new([ScriptedReply::no_session("cobalt-session")]);
        let observed = observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect("absent is ok");
        assert!(!observed.session_exists);
        assert!(observed.windows.is_empty() && observed.panes.is_empty());
    }

    #[test]
    fn an_untrusted_presence_read_fails_closed() {
        // The transient ladder, exhausted: never read as absence.
        let scripted = ScriptedTmux::always(ScriptedReply::server_exited());
        let error = observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect_err("untrusted");
        assert!(matches!(error, ObserveError::Untrusted(_)));
    }

    #[test]
    fn a_present_empty_session_reads_its_org_tag_and_no_objects() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),                        // has-session: present
            ScriptedReply::ok("cobalt-session\t$1"),      // exact session id
            ScriptedReply::ok("@organization_id cobalt"), // local session identity
            ScriptedReply::ok(""),                        // list-windows: none
            ScriptedReply::ok(""),                        // list-panes: none
        ]);
        let observed = observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect("present");
        assert!(observed.session_exists);
        assert_eq!(observed.session_organization, "cobalt");
        assert!(observed.windows.is_empty() && observed.panes.is_empty());
    }

    /// ONE INVOCATION PER OBJECT, and the reply count is part of the assertion.
    ///
    /// This used to script a reply per TAG per object — one `show-options -qv`,
    /// and therefore one fork, exec and socket round trip, each. On the
    /// operator's own company (two windows, five panes) that was twenty-six
    /// process spawns for a single observation, and `actuator.round elapsed_ms`
    /// sat at 683-985 for a pass whose plan was `requested: 0`.
    ///
    /// Each object is now asked for ALL of its own options at once and the reply
    /// is read BY NAME, which is what makes an absent tag an absent LINE rather
    /// than a positional shift. See [`read_tags`] for the two shapes that look
    /// like they would work here and are both wrong.
    #[test]
    fn a_present_session_reads_window_and_pane_ownership_tags() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),                        // has-session: present
            ScriptedReply::ok("cobalt-session\t$1"),      // exact session id
            ScriptedReply::ok("@organization_id cobalt"), // local session identity
            ScriptedReply::ok("@1"),                      // list-windows -> @1
            // list-panes -> %5 in @1, with its `#{pane_start_command}` third field
            ScriptedReply::ok("%5\t@1\t/usr/bin/env ORG_LAUNCHER_PERSON=vera pi"),
            // EVERY object's tags, in ONE reply, blocks opened by the marker.
            ScriptedReply::ok(
                "OBJ @1\n@organization_id cobalt\n@organization_window_id eng\n\
                 OBJ %5\n@organization_id cobalt\n@organization_window_id eng\n\
                 @organization_person_id vera\n@organization_launch_hash 9f2c4a",
            ),
        ]);
        let exec = exec(scripted);
        let observed =
            observe(&exec, &socket(), "cobalt-session", &EverObserved::new()).expect("present");
        assert_eq!(
            exec.tmux_host().runner().calls().len(),
            6,
            "a WHOLE observation is six invocations — presence, exact session id, local session tag, the two \
             listings, and ONE batched tag read for every object. This number must not grow \
             with the company: a tmux command is a process, so the cost is spawns x ~25ms and \
             the spawn count is the only lever there is: {:?}",
            exec.tmux_host().runner().calls()
        );
        assert_eq!(observed.windows.len(), 1);
        assert_eq!(observed.windows[0].logical_id, "eng");
        assert_eq!(observed.panes.len(), 1);
        assert_eq!(observed.panes[0].person_id, "vera");
        assert_eq!(observed.panes[0].launch_hash, "9f2c4a");
        assert_eq!(observed.panes[0].tmux_window_id, "@1");
        // #64: the start command is captured verbatim (env-attribution evidence).
        assert_eq!(observed.panes[0].start_command, "/usr/bin/env ORG_LAUNCHER_PERSON=vera pi");
    }

    #[test]
    fn a_failed_window_list_fails_closed() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),                        // present
            ScriptedReply::ok("cobalt-session\t$1"),      // exact session id
            ScriptedReply::ok("@organization_id cobalt"), // local session identity
            ScriptedReply::failed("lost server"),         // list-windows fails
        ]);
        let error = observe(&exec(scripted), &socket(), "cobalt-session", &EverObserved::new())
            .expect_err("failed");
        assert!(matches!(error, ObserveError::Failed { .. }));
    }

    #[test]
    fn torn_pane_reap_fences_once_and_refreshes_the_final_observed_topology() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("org-cobalt_\t$1"),
            ScriptedReply::ok("@organization_id cobalt"),
            ScriptedReply::ok("@1\t"),
            ScriptedReply::ok("%1\t@1\tsleep 120\ttorn\n%2\t@1\tsleep 120\t"),
            ScriptedReply::ok(""),
            ScriptedReply::ok(
                "OBJ @1\n@organization_id cobalt\n@organization_window_id executive\n\
                 OBJ %2\n@organization_id cobalt\n@organization_window_id executive\n\
                 @organization_person_id chief\n@organization_launch_hash hash",
            ),
            ScriptedReply::ok(""),
        ])
        .recording_viewport_authority();
        let exec = exec(scripted);
        let observed = observe(&exec, &socket(), "org-cobalt_", &EverObserved::new())
            .expect("the surviving topology is authoritative");
        assert_eq!(observed.windows.len(), 1);
        assert_eq!(observed.panes.len(), 1);
        assert_eq!(observed.panes[0].tmux_id, "%2");
        let calls = exec.tmux_host().runner().calls();
        let invalidated = calls
            .iter()
            .position(|call| call.iter().any(|arg| arg.contains("@chief_viewport_topology_epoch")))
            .unwrap_or_else(|| panic!("topology epoch minted before reap: {calls:?}"));
        let killed = calls
            .iter()
            .position(|call| call.first().is_some_and(|verb| verb == "kill-pane"))
            .expect("torn pane reaped");
        let refreshed = calls
            .iter()
            .position(|call| {
                call.first().is_some_and(|verb| verb == "if-shell")
                    && call.iter().any(|arg| arg.contains("#{@chief_viewport_refresh_command} 1"))
            })
            .unwrap_or_else(|| panic!("final observed truth refresh requested: {calls:?}"));
        assert!(invalidated < killed && killed < refreshed, "calls: {calls:?}");
        assert_eq!(
            calls
                .iter()
                .filter(|call| {
                    call.iter().any(|arg| arg.contains("@chief_viewport_topology_epoch"))
                })
                .count(),
            1,
            "one reap group mints one epoch"
        );
    }

    #[test]
    fn one_empty_session_tag_is_never_reap_permission() {
        // Live viewport incident, 2026-08-17: one healthy session-tag read
        // returned empty. The old inference destroyed all eight windows and
        // the CEO. An absent value is not positive evidence of a torn mint.
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("cobalt-session\t$1"),
            ScriptedReply::ok(""),
        ]);
        let exec = exec(scripted);
        let error = observe(&exec, &socket(), "cobalt-session", &EverObserved::new())
            .expect_err("an ownerless session without positive mint evidence must fail closed");
        assert!(matches!(error, ObserveError::Failed { .. }));
        assert!(
            !exec.tmux_host().runner().ran_verb("kill-session"),
            "an empty ownership read must never destroy a prior healthy company"
        );
    }

    #[test]
    fn empty_session_organization_even_with_marker_one_never_authorizes_a_kill() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("cobalt-session\t$1"),
            ScriptedReply::ok("@organization_minting 1"),
        ]);
        let exec = exec(scripted);
        let error = observe(&exec, &socket(), "cobalt-session", &EverObserved::new())
            .expect_err("an unowned session always fails closed");
        assert!(matches!(error, ObserveError::Failed { .. }));
        assert!(
            exec.tmux_host()
                .runner()
                .calls()
                .iter()
                .all(|call| call.iter().all(|arg| !arg.contains("kill-session"))),
            "observe has no whole-session destruction path"
        );
    }

    #[test]
    fn valid_session_organization_ignores_a_stale_marker_and_never_kills() {
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),
            ScriptedReply::ok("cobalt-session\t$1"),
            ScriptedReply::ok("@organization_id cobalt\n@organization_minting 1"),
            ScriptedReply::ok(""),
            ScriptedReply::ok(""),
        ]);
        let exec = exec(scripted);
        let observed = observe(&exec, &socket(), "cobalt-session", &EverObserved::new())
            .expect("valid local ownership is sufficient");
        assert_eq!(observed.session_organization, "cobalt");
        assert!(
            exec.tmux_host()
                .runner()
                .calls()
                .iter()
                .all(|call| call.iter().all(|arg| !arg.contains("kill-session"))),
            "a stale marker is inert"
        );
    }

    #[test]
    fn real_prior_healthy_session_survives_one_empty_identity_read() {
        let socket = Socket(format!(
            "chief-session-empty-read-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let session = "org-empty-read_";
        let created = real_tmux(
            &socket,
            &["new-session", "-d", "-s", session, "sh", "-c", "while :; do sleep 60; done"],
        );
        assert_eq!(created.status, 0, "{}", created.stderr);
        assert_eq!(
            real_tmux(&socket, &["set-option", "-t", session, "@organization_id", "cobalt"]).status,
            0
        );
        let before = real_tmux(
            &socket,
            &["display-message", "-p", "-t", session, "#{pane_id}\t#{pane_pid}"],
        );
        assert_eq!(before.status, 0, "{}", before.stderr);

        let exec = RealHostExecutor::new(
            TmuxHost::new(
                EmptySessionIdentityOnce {
                    inner: SystemTmuxRunner::default(),
                    pending: std::sync::atomic::AtomicBool::new(true),
                },
                RecordingWaiter::default(),
            ),
            ProcReader::default(),
        );
        let error = observe(&exec, &socket, session, &EverObserved::new())
            .expect_err("one empty identity answer fails closed");
        assert!(matches!(error, ObserveError::Failed { .. }));

        let after = real_tmux(
            &socket,
            &["display-message", "-p", "-t", session, "#{pane_id}\t#{pane_pid}"],
        );
        let organization =
            real_tmux(&socket, &["show-options", "-qv", "-t", session, "@organization_id"]);
        let _ = real_tmux(&socket, &["kill-server"]);
        assert_eq!(after.status, 0, "the session must remain present: {}", after.stderr);
        assert_eq!(after.stdout, before.stdout, "the pane and process identity must survive");
        assert_eq!(organization.stdout.trim(), "cobalt", "the real session tag is unchanged");
    }

    #[test]
    fn real_unowned_session_and_its_process_are_never_reaped() {
        let socket = Socket(format!(
            "chief-session-unowned-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let session = "org-unowned_";
        let created = real_tmux(
            &socket,
            &["new-session", "-d", "-s", session, "sh", "-c", "while :; do sleep 60; done"],
        );
        assert_eq!(created.status, 0, "{}", created.stderr);
        let before = real_tmux(
            &socket,
            &["display-message", "-p", "-t", session, "#{session_id}\t#{pane_id}\t#{pane_pid}"],
        );
        assert_eq!(before.status, 0, "{}", before.stderr);

        let exec = RealHostExecutor::new(
            TmuxHost::new(SystemTmuxRunner::default(), RecordingWaiter::default()),
            ProcReader::default(),
        );
        let error = observe(&exec, &socket, session, &EverObserved::new())
            .expect_err("an unowned whole session is never adopted or destroyed");
        assert!(matches!(error, ObserveError::Failed { .. }));

        let after = real_tmux(
            &socket,
            &["display-message", "-p", "-t", session, "#{session_id}\t#{pane_id}\t#{pane_pid}"],
        );
        let _ = real_tmux(&socket, &["kill-server"]);
        assert_eq!(after.status, 0, "the unowned session must remain: {}", after.stderr);
        assert_eq!(after.stdout, before.stdout, "session, pane, and process identity must survive");
    }

    #[test]
    fn real_global_identity_options_never_authorize_session_ownership_or_reap() {
        let socket = Socket(format!(
            "chief-session-global-only-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let session = "org-global-only_";
        let created = real_tmux(
            &socket,
            &["new-session", "-d", "-s", session, "sh", "-c", "while :; do sleep 60; done"],
        );
        assert_eq!(created.status, 0, "{}", created.stderr);
        assert_eq!(
            real_tmux(&socket, &["set-option", "-g", "@organization_id", "cobalt"]).status,
            0
        );
        assert_eq!(
            real_tmux(&socket, &["set-option", "-g", "@organization_minting", "1"]).status,
            0
        );
        let before = real_tmux(
            &socket,
            &["display-message", "-p", "-t", session, "#{pane_id}\t#{pane_pid}"],
        );
        assert_eq!(before.status, 0, "{}", before.stderr);

        let exec = RealHostExecutor::new(
            TmuxHost::new(SystemTmuxRunner::default(), RecordingWaiter::default()),
            ProcReader::default(),
        );
        let error = observe(&exec, &socket, session, &EverObserved::new())
            .expect_err("global-only values are not session ownership or mint evidence");
        assert!(matches!(error, ObserveError::Failed { .. }));

        let after = real_tmux(
            &socket,
            &["display-message", "-p", "-t", session, "#{pane_id}\t#{pane_pid}"],
        );
        let _ = real_tmux(&socket, &["kill-server"]);
        assert_eq!(
            after.status, 0,
            "the global marker must not reap the session: {}",
            after.stderr
        );
        assert_eq!(after.stdout, before.stdout, "the pane and process identity must survive");
    }

    #[test]
    fn a_tagged_session_is_never_reaped() {
        // The positive-evidence rule's other half: any non-empty organization
        // tag — ours or not — means the session is somebody's live property
        // and observe() must not destroy it.
        let scripted = ScriptedTmux::new([
            ScriptedReply::ok(""),                          // has-session: present
            ScriptedReply::ok("cobalt-session\t$1"),        // exact session id
            ScriptedReply::ok("@organization_id somebody"), // local session identity
            ScriptedReply::ok(""),                          // list-windows: none
            ScriptedReply::ok(""),                          // list-panes: none
        ]);
        let exec = exec(scripted);
        let observed =
            observe(&exec, &socket(), "cobalt-session", &EverObserved::new()).expect("present");
        assert!(observed.session_exists);
        assert_eq!(observed.session_organization, "somebody");
        assert!(!exec.tmux_host().runner().ran_verb("kill-session"));
    }
}
