//! The tmux side of the real executor: retries, presence, ownership audit.
//!
//! Every tmux read chiefd makes for a *decision* goes through here, so the
//! trust rules in [`super::trust`] cannot be bypassed by a caller who reaches
//! for `HostExecutor::tmux` and reads the exit status itself.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::actuate::host::{HostErr, PaneId, Pid, Socket, TmuxCmd, TmuxOut};
use crate::ladder::{Ladder, LadderEvents};

use crate::actuate::runner::{TmuxRunner, Waiter};
use crate::actuate::trust::{
    classify_ownership, classify_presence, classify_tag_read, tags, ObservedTags, Ownership,
    RebuildDecision, SessionPresence, TagRead, TmuxObjectKind, UnprovenCause,
    SERVER_EXITED_RETRIES, SERVER_EXITED_RETRY_DELAY_MS,
};

/// The lines the "server exited unexpectedly" ladder writes.
///
/// `tmux.transient.exhausted` keeps its name and its `error` level: it is the
/// only outcome here a human can act on, and renaming an event somebody may
/// already grep for buys nothing.
const TRANSIENT_LADDER: LadderEvents = LadderEvents {
    waiting: "tmux.transient.wait",
    resolved: "tmux.transient.resolved",
    failed: "tmux.transient.exhausted",
};

/// One pane chiefd is willing to act on: fully tagged, and tagged for us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPane {
    /// tmux pane id (`%12`).
    pub pane: PaneId,
    /// The person the pane runs.
    pub person_id: String,
    /// The derived launch hash the pane was tagged with at launch.
    ///
    /// A string, never parsed: it is compared for EQUALITY against the hash
    /// chiefd published, and nothing else is ever asked of it. A tag that is
    /// missing or malformed therefore simply fails to match, which is the
    /// safe direction — the pane is replaced rather than adopted.
    pub launch_hash: String,
}

/// The result of a read-only ownership audit of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAudit {
    /// Proven presence or proven absence — never an unproven observation,
    /// which surfaces as [`HostErr::Untrusted`] instead.
    pub presence: SessionPresence,
    /// Logical window id → tmux window id, for our fully-tagged windows.
    pub windows: BTreeMap<String, String>,
    /// Person id → pane, for our fully-tagged panes.
    pub panes: BTreeMap<String, ObservedPane>,
}

impl SessionAudit {
    /// Whether the durable plan may be rebuilt from scratch (invariant 9).
    #[must_use]
    pub fn rebuild_decision(&self) -> RebuildDecision {
        super::trust::rebuild_decision(self.presence)
    }
}

/// tmux operations, with the trust rules applied.
#[derive(Debug)]
pub struct TmuxHost<R: TmuxRunner, W: Waiter> {
    runner: R,
    waiter: W,
}

impl<R: TmuxRunner, W: Waiter> TmuxHost<R, W> {
    /// Compose a runner and a waiter.
    pub const fn new(runner: R, waiter: W) -> Self {
        Self { runner, waiter }
    }

    /// The underlying runner, for callers that legitimately need raw tmux
    /// (styling, layout) where no ownership decision is being made.
    pub const fn runner(&self) -> &R {
        &self.runner
    }

    /// The waiter, exposed so tests can assert the ladder was walked.
    pub const fn waiter(&self) -> &W {
        &self.waiter
    }

    /// Run one tmux command, retrying **only** the transient
    /// "server exited unexpectedly" condition, 20 times at 25 ms.
    ///
    /// Exhausting the ladder is [`HostErr::Untrusted`], never an authoritative
    /// answer: the port of `org-runtime-ownership.ts:133-151`, where the loop
    /// re-throws on `attempt === 19` rather than concluding absence.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] if tmux cannot be run;
    /// [`HostErr::Untrusted`] if every attempt hit the transient condition.
    pub fn run_retrying_transients(
        &self,
        socket: &Socket,
        cmd: &TmuxCmd,
    ) -> Result<TmuxOut, HostErr> {
        let backoff = Duration::from_millis(SERVER_EXITED_RETRY_DELAY_MS);
        let mut ladder = Ladder::new(
            TRANSIENT_LADDER,
            cmd.argv.first().map_or("", String::as_str),
            backoff * SERVER_EXITED_RETRIES,
            backoff,
        );
        let mut attempt = 0;
        loop {
            let out = self.runner.run(socket, cmd)?;
            let transient = matches!(
                classify_presence(out.status, &out.stdout, &out.stderr),
                SessionPresence::Unproven(UnprovenCause::ServerExitedUnexpectedly)
            );
            if !transient {
                // Only when this ladder actually walked: a verb that answered
                // first time is not a wait and writes no line at all.
                if ladder.attempts() > 0 {
                    ladder.resolved();
                }
                return Ok(out);
            }
            attempt += 1;
            if attempt >= SERVER_EXITED_RETRIES {
                ladder.failed("tmux reported a transient for the whole retry ladder");
                return Err(HostErr::Untrusted {
                    reason: "tmux reported 'server exited unexpectedly' for the whole retry ladder; absence was never proven".into(),
                });
            }
            // A transient that RESOLVES is a wait, not a failure: this ladder
            // used to write one `warn` per attempt, up to nineteen of them, for
            // a condition the twentieth attempt then cleared. The loud line is
            // the `failed` arm above, which is the one a human can act on.
            ladder.waiting();
            self.waiter.wait(backoff);
        }
    }

    /// Three-valued presence check (`has-session`), transients retried.
    ///
    /// # Errors
    /// [`HostErr::Untrusted`] when tmux never answered.
    pub fn session_presence(
        &self,
        socket: &Socket,
        session: &str,
    ) -> Result<SessionPresence, HostErr> {
        let cmd = TmuxCmd { argv: vec!["has-session".into(), "-t".into(), session.to_owned()] };
        let out = self.run_retrying_transients(socket, &cmd)?;
        match classify_presence(out.status, &out.stdout, &out.stderr) {
            proven @ (SessionPresence::Present | SessionPresence::ProvablyAbsent) => Ok(proven),
            SessionPresence::Unproven(UnprovenCause::InvalidOption) => Err(HostErr::Untrusted {
                reason: "tmux rejected the has-session invocation; presence is unproven".into(),
            }),
            SessionPresence::Unproven(_) => Err(HostErr::Untrusted {
                reason: "tmux failed with an unrecognized diagnostic; presence is unproven".into(),
            }),
        }
    }

    /// Read one ownership tag off a tmux object.
    ///
    /// `scope` is `-t`-preceded tmux scope flags: `[]` for a session,
    /// `["-w"]` for a window, `["-p"]` for a pane. `quiet` selects `-qv`
    /// (unset reads as empty) over `-v` (unset is an `invalid option` error);
    /// the session ownership check uses the strict form, exactly as
    /// `org-tmux.ts:633` does.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] if tmux cannot be run.
    pub fn read_tag(
        &self,
        socket: &Socket,
        scope: &[&str],
        target: &str,
        tag: &str,
        quiet: bool,
    ) -> Result<TagRead, HostErr> {
        let mut argv = vec!["show-options".to_owned()];
        argv.extend(scope.iter().map(|s| (*s).to_owned()));
        argv.push(if quiet { "-qv".to_owned() } else { "-v".to_owned() });
        argv.push("-t".to_owned());
        argv.push(target.to_owned());
        argv.push(tag.to_owned());
        let out = self.run_retrying_transients(socket, &TmuxCmd { argv })?;
        Ok(classify_tag_read(out.status, &out.stdout, &out.stderr))
    }

    /// Read-only ownership proof for a session, ported from
    /// `auditOrganizationTmux` (`org-tmux.ts:622-660`).
    ///
    /// Returns an empty audit when the session is *provably* absent. Refuses —
    /// never guesses — when tmux did not answer, when the session belongs to
    /// another company, or when any object inside it is only partly tagged.
    ///
    /// # Errors
    /// [`HostErr::Untrusted`] when an observation is unproven;
    /// [`HostErr::ToolFailed`] when ownership is foreign, partial or ambiguous.
    pub fn audit_session(
        &self,
        socket: &Socket,
        session: &str,
        organization: &str,
    ) -> Result<SessionAudit, HostErr> {
        let presence = self.session_presence(socket, session)?;
        if presence == SessionPresence::ProvablyAbsent {
            return Ok(SessionAudit { presence, windows: BTreeMap::new(), panes: BTreeMap::new() });
        }

        // The session tag is read strictly: a session we cannot prove is ours
        // is a session we do not touch.
        match self.read_tag(socket, &[], session, tags::ORGANIZATION, false)? {
            TagRead::Untrusted(_) => {
                return Err(HostErr::Untrusted {
                    reason: "tmux could not report the session ownership tag; it is not evidence the session is free".into(),
                })
            }
            TagRead::Value(value) if value != organization => {
                return Err(HostErr::ToolFailed {
                    tool: "tmux",
                    detail: format!(
                        "refusing lifecycle on session '{session}': ownership tag is '{}', expected '{organization}'",
                        if value.is_empty() { "missing" } else { &value }
                    ),
                })
            }
            TagRead::Value(_) => {}
        }

        let mut windows = BTreeMap::new();
        let mut window_of_tmux_id = BTreeMap::new();
        for tmux_window in self.list_ids(socket, session, ListKind::Windows)? {
            let observed = self.read_object_tags(socket, &["-w"], &tmux_window)?;
            match classify_ownership(TmuxObjectKind::Window, &observed, organization) {
                Ownership::Unrelated => continue,
                Ownership::Ours => {}
                verdict => return Err(ownership_refusal("window", &tmux_window, verdict)),
            }
            if windows.insert(observed.window_id.clone(), tmux_window.clone()).is_some() {
                return Err(HostErr::ToolFailed {
                    tool: "tmux",
                    detail: format!(
                        "ambiguous duplicate organization window '{}'",
                        observed.window_id
                    ),
                });
            }
            window_of_tmux_id.insert(tmux_window, observed.window_id);
        }

        let mut panes = BTreeMap::new();
        for tmux_pane in self.list_ids(socket, session, ListKind::Panes)? {
            let observed = self.read_object_tags(socket, &["-p"], &tmux_pane)?;
            match classify_ownership(TmuxObjectKind::Pane, &observed, organization) {
                Ownership::Unrelated => continue,
                Ownership::Ours => {}
                verdict => return Err(ownership_refusal("pane", &tmux_pane, verdict)),
            }
            // The launch hash is NOT parsed or validated here. There is no
            // such thing as an "invalid" hash to this reader: an unexpected
            // value is a pane built from something other than what chiefd
            // wants, which is precisely what the diff is for.
            let launch_hash = observed.launch_hash.clone();
            let person_id = observed.person_id.clone();
            let previous = panes.insert(
                person_id.clone(),
                ObservedPane { pane: PaneId(tmux_pane.clone()), person_id, launch_hash },
            );
            if previous.is_some() {
                return Err(HostErr::ToolFailed {
                    tool: "tmux",
                    detail: format!(
                        "ambiguous duplicate organization person '{}'",
                        observed.person_id
                    ),
                });
            }
        }

        Ok(SessionAudit { presence, windows, panes })
    }

    /// The pane ids in a session tmux reports `#{pane_dead}` for.
    ///
    /// Port of `deadPaneIds` (`org-health-monitor.ts:915-921`): one
    /// `list-panes -s -F "#{pane_id}\t#{pane_dead}"`, keeping ids whose dead
    /// field is exactly `"1"`. Goes through [`Self::checked`] like every other
    /// decision read, so an unanswered tmux is [`HostErr::Untrusted`] and an
    /// absent session is [`HostErr::ToolFailed`] — never a silently empty list.
    ///
    /// # Errors
    /// [`HostErr::ToolFailed`] when tmux answered no; [`HostErr::Untrusted`]
    /// when it did not answer.
    pub fn dead_pane_ids(&self, socket: &Socket, session: &str) -> Result<Vec<PaneId>, HostErr> {
        let out = self.checked(
            socket,
            &TmuxCmd {
                argv: vec![
                    "list-panes".into(),
                    "-s".into(),
                    "-t".into(),
                    session.to_owned(),
                    "-F".into(),
                    "#{pane_id}\t#{pane_dead}".into(),
                ],
            },
        )?;
        Ok(out
            .stdout
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t').map(str::trim);
                let id = fields.next().unwrap_or_default();
                let dead = fields.next().unwrap_or_default();
                (!id.is_empty() && dead == "1").then(|| PaneId(id.to_owned()))
            })
            .collect())
    }

    /// Spawn one pane and tag it. Via tmux, so the pane outlives chiefd.
    ///
    /// # Errors
    /// [`HostErr::ToolFailed`] if tmux refused, [`HostErr::Untrusted`] if it
    /// did not answer.
    pub fn spawn_pane(
        &self,
        socket: &Socket,
        session: &str,
        window: &str,
        argv: &[String],
        pane_tags: &[(String, String)],
    ) -> Result<PaneId, HostErr> {
        let mut command = vec![
            "new-window".to_owned(),
            "-d".to_owned(),
            "-P".to_owned(),
            "-F".to_owned(),
            "#{pane_id}".to_owned(),
            "-t".to_owned(),
            session.to_owned(),
            "-n".to_owned(),
            // Use the shared bounded canonical label. See
            // `tmux::safe_window_name`.
            super::safe_window_name(window),
        ];
        command.push("--".to_owned());
        command.extend(argv.iter().cloned());
        let out = self.checked(socket, &TmuxCmd { argv: command })?;
        let pane = out.stdout.trim().to_owned();
        if pane.is_empty() {
            return Err(HostErr::Untrusted {
                reason: "tmux returned no pane id for the spawn; the pane's existence is unproven"
                    .into(),
            });
        }
        for (tag, value) in pane_tags {
            self.checked(
                socket,
                &TmuxCmd {
                    argv: vec![
                        "set-option".into(),
                        "-p".into(),
                        "-t".into(),
                        pane.clone(),
                        tag.clone(),
                        value.clone(),
                    ],
                },
            )?;
        }
        Ok(PaneId(pane))
    }

    /// Read a pane's pid **now**. Never cached: tmux spawns the process, so a
    /// recorded pid goes stale on `respawn-pane` and on the native
    /// fresh-session path (plan §6.2).
    ///
    /// # Errors
    /// [`HostErr::ToolFailed`] when tmux answered without a usable pid.
    pub fn pane_pid(&self, socket: &Socket, pane: &PaneId) -> Result<Pid, HostErr> {
        let out = self.checked(
            socket,
            &TmuxCmd {
                argv: vec![
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    pane.0.clone(),
                    "-F".into(),
                    "#{pane_pid}".into(),
                ],
            },
        )?;
        out.stdout.trim().parse::<i32>().map(Pid).map_err(|_| HostErr::ToolFailed {
            tool: "tmux",
            detail: format!("pane {} reported no usable pid", pane.0),
        })
    }

    /// Read a pane's pid and ownership tags in **one** observation — the
    /// authentication read (plan §6.2, port of `observeManagedPane`,
    /// `org-caller-auth.ts:100-112`).
    ///
    /// Two properties are load-bearing and are what make this a separate
    /// method rather than five `read_tag` calls:
    ///
    /// * the pid and the tags come from the same `display-message`, so a
    ///   `respawn-pane` cannot land between them and pair a fresh pid with
    ///   stale tags;
    /// * an incomplete answer is an error. A pane with an unreadable
    ///   ownership tag is never treated as a pane with an empty one.
    ///
    /// # Errors
    /// [`HostErr::ToolFailed`] for a missing or unparseable field;
    /// [`HostErr::Untrusted`] when tmux did not answer.
    pub fn pane_identity(
        &self,
        socket: &Socket,
        pane: &PaneId,
    ) -> Result<crate::actuate::host::PaneIdentity, HostErr> {
        let format = format!(
            "#{{pane_pid}}\t#{{session_name}}\t#{{{}}}\t#{{{}}}\t#{{{}}}",
            tags::ORGANIZATION,
            tags::PERSON,
            tags::LAUNCH_HASH,
        );
        let out = self.checked(
            socket,
            &TmuxCmd {
                argv: vec![
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    pane.0.clone(),
                    "-F".into(),
                    format,
                ],
            },
        )?;
        parse_pane_identity(pane, &out.stdout)
    }

    /// Run a command that must succeed, mapping a refusal onto the right
    /// error class: `Untrusted` when tmux did not answer, `ToolFailed` when it
    /// answered no.
    fn checked(&self, socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr> {
        let out = self.run_retrying_transients(socket, cmd)?;
        if out.status == 0 {
            return Ok(out);
        }
        let verb = cmd.argv.first().cloned().unwrap_or_default();
        match classify_presence(out.status, &out.stdout, &out.stderr) {
            // tmux answered, and the answer was no.
            SessionPresence::Present | SessionPresence::ProvablyAbsent => {
                Err(HostErr::ToolFailed {
                    tool: "tmux",
                    detail: format!("tmux {verb} failed: {}", out.stderr.trim()),
                })
            }
            SessionPresence::Unproven(_) => Err(HostErr::Untrusted {
                reason: "tmux did not answer; the effect of this command is unknown".into(),
            }),
        }
    }

    fn list_ids(
        &self,
        socket: &Socket,
        session: &str,
        kind: ListKind,
    ) -> Result<Vec<String>, HostErr> {
        let mut argv = vec![kind.verb().to_owned()];
        if kind == ListKind::Panes {
            argv.push("-s".to_owned());
        }
        argv.extend([
            "-t".to_owned(),
            session.to_owned(),
            "-F".to_owned(),
            kind.format().to_owned(),
        ]);
        let out = self.checked(socket, &TmuxCmd { argv })?;
        Ok(out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }

    fn read_object_tags(
        &self,
        socket: &Socket,
        scope: &[&str],
        target: &str,
    ) -> Result<ObservedTags, HostErr> {
        let mut observed = ObservedTags::default();
        for (tag, field) in [
            (tags::ORGANIZATION, TagField::Organization),
            (tags::WINDOW, TagField::Window),
            (tags::PERSON, TagField::Person),
            (tags::LAUNCH_HASH, TagField::LaunchHash),
        ] {
            // Quiet reads: an unset tag is legitimately empty here, and the
            // ownership classification — not the read — decides what a missing
            // tag means. A *failed* read still never becomes an empty value.
            let value = match self.read_tag(socket, scope, target, tag, true)? {
                TagRead::Value(value) => value,
                TagRead::Untrusted(_) => {
                    return Err(HostErr::Untrusted {
                        reason: "tmux could not report an ownership tag; a pane whose ownership is unreadable is never adopted or killed".into(),
                    })
                }
            };
            match field {
                TagField::Organization => observed.organization_id = value,
                TagField::Window => observed.window_id = value,
                TagField::Person => observed.person_id = value,
                TagField::LaunchHash => observed.launch_hash = value,
            }
        }
        Ok(observed)
    }
}

enum TagField {
    Organization,
    Window,
    Person,
    LaunchHash,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Windows,
    Panes,
}

impl ListKind {
    const fn verb(self) -> &'static str {
        match self {
            Self::Windows => "list-windows",
            Self::Panes => "list-panes",
        }
    }

    const fn format(self) -> &'static str {
        match self {
            Self::Windows => "#{window_id}",
            Self::Panes => "#{pane_id}",
        }
    }
}

/// Parse the tab-joined authentication read.
///
/// Split out from the tmux call so the parsing — which is where the bugs are —
/// is testable without a tmux server. Every field must be present and
/// non-empty, and the two numeric fields must be positive integers, exactly as
/// `org-caller-auth.ts:107-111` requires.
fn parse_pane_identity(
    pane: &PaneId,
    stdout: &str,
) -> Result<crate::actuate::host::PaneIdentity, HostErr> {
    let incomplete = || HostErr::ToolFailed {
        tool: "tmux",
        detail: format!("pane {} has incomplete ChiefD ownership tags", pane.0),
    };
    let line = stdout.lines().next().ok_or_else(incomplete)?;
    let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
    let [pid, session, organization, person_id, launch_hash] = fields.as_slice() else {
        return Err(incomplete());
    };
    let pid = positive_i64(pid).ok_or_else(incomplete)?;
    // The launch hash must be PRESENT — an untagged pane is not a pane this
    // client may act on — but it is never parsed. Its only use is equality
    // against the hash chiefd published.
    if session.is_empty()
        || organization.is_empty()
        || person_id.is_empty()
        || launch_hash.is_empty()
    {
        return Err(incomplete());
    }
    Ok(crate::actuate::host::PaneIdentity {
        pane: pane.clone(),
        pid: Pid(i32::try_from(pid).map_err(|_| incomplete())?),
        session: (*session).to_owned(),
        organization: (*organization).to_owned(),
        person_id: (*person_id).to_owned(),
        launch_hash: (*launch_hash).to_owned(),
    })
}

/// `positiveInteger` from `org-caller-auth.ts:21-24`: a safe positive integer,
/// or nothing. An unparseable value is never a zero.
fn positive_i64(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().filter(|parsed| *parsed > 0)
}

fn ownership_refusal(kind: &str, id: &str, verdict: Ownership) -> HostErr {
    let reason = match verdict {
        Ownership::Foreign => "is tagged for another company",
        _ => "is not fully ownership-tagged",
    };
    HostErr::ToolFailed {
        tool: "tmux",
        detail: format!("refusing to reconcile: tmux {kind} {id} {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::TmuxHost;
    use crate::actuate::fake::{ScriptedReply, ScriptedTmux};
    use crate::actuate::host::{HostErr, Socket, TmuxCmd};
    use crate::actuate::runner::RecordingWaiter;
    use crate::actuate::trust::{SERVER_EXITED_RETRIES, SERVER_EXITED_RETRY_DELAY_MS};
    use crate::ladder::test_support::{levels, loud, recorded};

    fn has_session() -> TmuxCmd {
        TmuxCmd { argv: vec!["has-session".into(), "-t".into(), "org-acme".into()] }
    }

    /// The rule, at the ladder that logged one `warn` per attempt: a transient
    /// that RESOLVES is a wait, and a wait a human never had to act on writes
    /// nothing loud.
    #[test]
    fn a_transient_ladder_that_resolves_emits_no_warning() {
        let host = TmuxHost::new(
            ScriptedTmux::new([
                ScriptedReply::server_exited(),
                ScriptedReply::server_exited(),
                ScriptedReply::server_exited(),
                ScriptedReply::ok(""),
            ]),
            RecordingWaiter::default(),
        );

        let lines = recorded("transient-resolves", || {
            let out = host
                .run_retrying_transients(&Socket("chiefd".into()), &has_session())
                .expect("the fourth attempt answers");
            assert_eq!(out.status, 0);
        });

        assert!(
            loud(&lines).is_empty(),
            "a transient the ladder cleared is not a failure, got {:?}",
            levels(&lines)
        );
        assert_eq!(
            levels(&lines),
            vec!["info", "debug", "debug", "info"],
            "one info on entry, quiet repeats, one info on resolution"
        );
        assert_eq!(lines[0]["event"], "tmux.transient.wait");
        assert_eq!(lines[0]["detail"]["subject"], "has-session");
        assert_eq!(lines[0]["detail"]["attempt"], 1);
        assert_eq!(lines[0]["detail"]["backoff_ms"], SERVER_EXITED_RETRY_DELAY_MS);

        let resolved = lines.last().expect("a resolution line");
        assert_eq!(resolved["event"], "tmux.transient.resolved");
        assert_eq!(resolved["detail"]["attempt"], 4, "the attempt that answered is counted");
        assert!(resolved["detail"]["waited_ms"].as_u64().is_some(), "the wait must be timed");
    }

    /// A verb that answers first time is not a wait at all, and costs no lines.
    #[test]
    fn a_verb_that_never_hit_a_transient_writes_nothing() {
        let host =
            TmuxHost::new(ScriptedTmux::new([ScriptedReply::ok("")]), RecordingWaiter::default());
        let lines = recorded("transient-none", || {
            host.run_retrying_transients(&Socket("chiefd".into()), &has_session())
                .expect("it answered");
        });
        assert!(lines.is_empty(), "a ladder nobody walked writes nothing, got {lines:?}");
    }

    /// The signal that must never be lost. A ladder that EXHAUSTS its budget is
    /// a real failure: it stays loud, and it still refuses rather than
    /// concluding absence.
    #[test]
    fn a_transient_ladder_that_exhausts_its_budget_stays_loud() {
        let host = TmuxHost::new(
            ScriptedTmux::always(ScriptedReply::server_exited()),
            RecordingWaiter::default(),
        );

        let lines = recorded("transient-exhausted", || {
            let refusal = host
                .run_retrying_transients(&Socket("chiefd".into()), &has_session())
                .expect_err("a ladder that never cleared must refuse");
            assert!(
                matches!(refusal, HostErr::Untrusted { .. }),
                "absence was never proven, so this is Untrusted: {refusal:?}"
            );
        });

        let failures = loud(&lines);
        assert_eq!(failures.len(), 1, "exactly one loud line, got {:?}", levels(&lines));
        assert_eq!(failures[0]["event"], "tmux.transient.exhausted");
        assert_eq!(failures[0]["level"], "error");
        assert_eq!(
            failures[0]["detail"]["reason"],
            "tmux reported a transient for the whole retry ladder"
        );
        assert_eq!(failures[0]["detail"]["subject"], "has-session");
        assert_eq!(failures[0]["detail"]["attempt"], u64::from(SERVER_EXITED_RETRIES));
        assert_eq!(
            failures[0]["detail"]["budget_ms"],
            u64::from(SERVER_EXITED_RETRIES) * SERVER_EXITED_RETRY_DELAY_MS
        );
    }

    /// The compatibility contract: this change is levels and shape only. The
    /// ladder still walks exactly the attempts and the backoff it always did.
    #[test]
    fn the_retry_count_and_the_backoff_are_unchanged() {
        let host = TmuxHost::new(
            ScriptedTmux::always(ScriptedReply::server_exited()),
            RecordingWaiter::default(),
        );
        let _ = host.run_retrying_transients(&Socket("chiefd".into()), &has_session());

        let waits = host.waiter().waits();
        assert_eq!(
            u32::try_from(waits.len()).unwrap_or(u32::MAX),
            SERVER_EXITED_RETRIES - 1,
            "the last attempt refuses instead of waiting again"
        );
        assert!(
            waits.iter().all(|wait| wait.as_millis() == u128::from(SERVER_EXITED_RETRY_DELAY_MS)),
            "every backoff is unchanged: {waits:?}"
        );
    }
}
