//! The host refusal: may this machine run a company at all?
//!
//! Ported from `apps/cli/src/legacy/foundation/preflight.ts`, which is deleted.
//! It also replaces `operator.rs`'s `launcher_prerequisites`, the second copy of
//! the same probe — two implementations of one refusal is exactly what Mandate 0
//! forbids, and they had already drifted (one checked tmux and pi, the other
//! checked tmux, ambient `$TMUX`, tmux reachability, pi and the release stamp).
//!
//! # Why this is business logic and not bootstrap
//!
//! It is a *decision*, not an observation a process can only make about itself:
//! "an operator command needs tmux installed, a live tmux server to project a
//! company into, a resolvable Pi runtime, and a completed release." Each arm
//! carries the operator's next move, because a refusal whose recovery is
//! obvious is the entire value of running this before anything is occupied.

use std::cell::OnceCell;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

/// Which gate refused, so a caller (and a test) can name the case rather than
/// matching on prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreflightCode {
    /// Every gate passed.
    Ready,
    /// `tmux` is not installed.
    TmuxMissing,
    /// Not running inside a tmux client.
    OutsideTmux,
    /// `$TMUX` is set but its server does not answer.
    TmuxUnreachable,
    /// The Pi runtime could not be resolved.
    PiMissing,
    /// The Pi runtime resolved to something only THIS process can find.
    ///
    // TOMBSTONE: `PiNotAbsolute`. A name is not a location, and the company
    // daemon and every pane tmux mints run with a different `PATH` from the
    // operator's shell — so a runtime named `pi` was three lookups with three
    // possible outcomes. The only way such a value could reach here was an
    // operator's own `TEAM_LAUNCHER_PI` string, and that pin is deleted: the
    // ladder is `PATH` alone now, and `candidates_on_path` keeps absolute
    // candidates only. The property this named is stronger than before, because
    // it is structural rather than checked.
    /// Something named `pi` is executable on `PATH` and answers no `--version`.
    ///
    /// A DIFFERENT FACT from [`Self::PiMissing`], and the reason they are two
    /// codes: absent is fixed by installing, unusable is made worse by it. The
    /// caller branches on this to decide whether to run the installer at all.
    PiUnusable,
    /// `bun run release` has never completed on this host.
    NotReleased,
}

/// Which kind of caller is being cleared.
///
/// The gates split in two, and the split is structural rather than cosmetic:
/// three of them ask *can this HOST run a company* (tmux installed, a Pi
/// runtime, a completed release) and two ask *does this CALLER have a terminal
/// to be moved out of* (`$TMUX` set, and its server answering).
///
/// # Why the terminal-owning verbs are three variants and not one
///
/// They used to be a single `OperatorTerminal`, and [`Surface::retry`] answered
/// `chief` for all of it. So an operator who typed
/// `chief attach <company>` outside tmux was told, by the module whose whole
/// stated value is that "a refusal whose recovery is obvious", to run a
/// DIFFERENT verb — one that creates another company instead of entering the
/// one they named. Found live on 2026-08-10: `chief attach northwind-labs
/// --yes` from a non-tmux pty answered "chief only runs inside tmux. Start one
/// with 'tmux new -s companies', then run 'chief' again." Two of the three
/// terminal-owning verbs carried recovery copy for the third. The gate itself
/// was right every time, which is why nothing caught it.
///
/// `chief actuate` is a resident process with no terminal of its own — it
/// CREATES the company's session rather than entering one — so demanding
/// `$TMUX` of it would refuse the only verb that can start anybody. It was
/// therefore given no preflight at all, which is the defect the live proof
/// found: on a host where `pi` was not on the actuator's PATH, every spawned
/// pane died the instant it was created, tmux destroyed the empty window, and
/// the next step failed against a window that no longer existed with
/// `unusable window dimensions "\t\n"`. The actuator then minted a fresh pane
/// and did it again, once per second, forever. Nothing in that sentence names
/// the actual problem, and the one gate that does — "no Pi runtime was found"
/// — was already written here and simply never asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// Bare `chief` — the Founder door.
    Founder,
    /// `chief attach <company>` — enter a company's CEO.
    Attach,
    /// `chief reset <company>` — shed a company back to CEO-only.
    Reset,
    /// `actuate`: resident, headless, and the only verb that spawns people.
    ResidentActuator,
}

/// What a surface needs from the caller's OWN terminal.
///
/// This was a boolean, and a boolean could only say "demand a live tmux client"
/// or "ask nothing". So `chief` — the one verb that CREATES the operator's
/// context — was filed with the two verbs that ENTER a context which already
/// exists, and an operator outside tmux was told to go and start a terminal
/// multiplexer by hand before the product would talk to them. Reported live on
/// 2026-08-10: *"Every time I run `chief` it asks me to be inside a
/// tmux."* [`super::founder::run`] has carried a create-or-reuse-and-attach
/// branch for exactly that case since it was ported, and this gate refused two
/// statements earlier — so the branch was unreachable and the refusal was the
/// only thing anyone ever saw. One front door does not hand the human a
/// prerequisite it can satisfy itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalNeed {
    /// Must ALREADY be sitting in a live tmux client: `reset`. It sheds a
    /// running company back to CEO-only from inside the context the operator
    /// is already in, and creates no context of its own.
    Required,
    /// Starts its own tmux client when there is none: `new` and `attach`. A
    /// `$TMUX` that IS set is still honoured, and still has to answer —
    /// `run` hosts the Founder in that session rather than creating a
    /// second one, and `attach` switches the client it is already in.
    ///
    /// `attach` was `Required` until an operator reported the same complaint
    /// this enum was created for, one door over: *"`chief attach` — when I run
    /// this it keeps telling me you need tmux. It should just tmux for me the
    /// way the `chief` command does."* The old reasoning — that a verb
    /// which ENTERS a context may not invent one — sounded principled and was
    /// wrong in practice: the context `attach` enters is the COMPANY's session,
    /// which already exists or is about to be started by `attach` itself. What
    /// it would have had to invent is a tmux CLIENT, which is exactly what
    /// `tmux attach-session` is for and what `new` was already allowed to do.
    /// One front door does not hand the human a prerequisite it can satisfy
    /// itself.
    BootsOwn,
    /// Has no terminal at all: the resident actuator, which MINTS the company's
    /// session rather than entering one.
    None,
}

impl Surface {
    /// What this caller needs from a terminal.
    const fn terminal(self) -> TerminalNeed {
        match self {
            Self::Founder | Self::Attach => TerminalNeed::BootsOwn,
            Self::Reset => TerminalNeed::Required,
            Self::ResidentActuator => TerminalNeed::None,
        }
    }

    /// The command the operator should run once they have fixed the gate.
    const fn retry(self) -> &'static str {
        match self {
            Self::Founder => "chief",
            Self::Attach => "chief attach <company>",
            Self::Reset => "chief reset <company>",
            Self::ResidentActuator => "chief actuate <company>",
        }
    }

    /// Where that command has to be run from, as a trailing clause.
    const fn retry_context(self) -> &'static str {
        match self {
            // Neither `new` nor `attach` has one: both start the session
            // themselves, so "again inside tmux" would name a prerequisite that
            // is gone. `attach` joined them when an operator reported being
            // told to start a multiplexer by hand to enter a company that was
            // already running.
            Self::Founder | Self::Attach => "",
            Self::Reset => " inside tmux",
            // Deliberately nowhere: the actuator is the process that makes the
            // session exist, so telling it to go and find one is circular.
            Self::ResidentActuator => "",
        }
    }
}

/// One preflight outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preflight {
    /// Which gate answered.
    pub(crate) code: PreflightCode,
    /// The operator-facing message, carrying the recovery command.
    pub(crate) message: String,
}

impl Preflight {
    /// Whether every gate passed.
    #[must_use]
    pub(crate) fn ok(&self) -> bool {
        self.code == PreflightCode::Ready
    }
}

/// The observations the decision is made from.
///
/// A trait, not five closures, so a test states a whole host in one value and
/// no arm can be left implicitly real.
pub(crate) trait HostProbe {
    /// Is `command` present and runnable?
    fn has_command(&self, command: &str) -> bool;
    /// The value of an environment variable, trimmed; `None` when unset/blank.
    fn env(&self, name: &str) -> Option<String>;
    /// Does the tmux server named by the environment answer?
    fn tmux_reachable(&self) -> bool;
    /// Is the Pi runtime resolvable, under what name — and if not, WHY not?
    ///
    /// Three answers, because "absent" and "unusable" have opposite remedies.
    /// See [`PiResolution`].
    fn pi_runtime(&self) -> PiResolution;
    // TOMBSTONE: `pinned_pi`. It answered "is a runtime NAMED, whether or not
    // it runs", which existed only so a broken `TEAM_LAUNCHER_PI` could be told
    // apart from an absent Pi. The pin is deleted, so those are the same fact
    // now and the installer is the answer to both.
    //
    // AND IT STAYS DELETED. A MINIMUM came back in its place — see
    // `host_primitives::pi_floor` — and the two are not the same object. The
    // pin asked "is this the ONE version we support", refused everything else,
    // and had to be edited every time Pi shipped, which is two or three times a
    // week. The floor asks "is this at least old-enough", passes every newer Pi
    // for ever, and is read by exactly two places: `warn_below_pi_floor` below,
    // which only talks, and `chief upgrade`, which is the one moment the
    // product can offer to fix it. Nothing here gates on a version, and this
    // probe is not coming back to serve one.

    /// Has `bun run release` completed on this host?
    fn released(&self) -> bool;
}

/// Decide, from one set of observations.
///
/// The order is the point: the cheapest, most-actionable refusal first, so a
/// genuinely fresh host is told what to run before tmux is even occupied.
pub(crate) fn decide(probe: &dyn HostProbe, surface: Surface) -> Preflight {
    let retry = surface.retry();
    let need = surface.terminal();
    if !probe.has_command(TMUX_PROGRAM) {
        // NAME THE INSTALL. A refusal must name what would be accepted, and of
        // every gate on this list this is the only one chiefd cannot satisfy
        // for the operator — so it is the one that has to carry a command
        // rather than a noun. "Install tmux" told somebody who does not have
        // tmux to install tmux.
        const INSTALL: &str =
            "Install tmux (macOS: 'brew install tmux'; Debian/Ubuntu: 'apt-get install -y tmux')";
        return Preflight {
            code: PreflightCode::TmuxMissing,
            message: match need {
                TerminalNeed::Required => format!(
                    "tmux is required. {INSTALL}, start a tmux session, then run '{retry}' again."
                ),
                TerminalNeed::BootsOwn => format!(
                    "tmux is required, and it is the one thing chief cannot start for you. \
                     {INSTALL}, then run '{retry}' again — chief starts the session itself."
                ),
                TerminalNeed::None => {
                    format!("tmux is required. {INSTALL}, then run '{retry}' again.")
                }
            },
        };
    }
    let inside = probe.env("TMUX").is_some();
    if need == TerminalNeed::Required && !inside {
        return Preflight {
            code: PreflightCode::OutsideTmux,
            message: format!("chief only runs inside tmux. Start one with 'tmux new -s companies', then run '{retry}' again."),
        };
    }
    // A `$TMUX` that IS set must ANSWER, on every surface that will act on it.
    // Not skipped for `new` just because the gate above is: `run` reads the
    // same variable, and a stale `$TMUX` sends it down the inside-tmux branch
    // to tag a pane on a server that is not there. The actuator is exempt for
    // the reason it always was — it holds no client of its own.
    if inside && need != TerminalNeed::None && !probe.tmux_reachable() {
        return Preflight {
            code: PreflightCode::TmuxUnreachable,
            message: match need {
                TerminalNeed::BootsOwn => format!(
                    "TMUX is set, but its server is unreachable. Enter a live tmux session, or \
                     unset TMUX so chief can start its own, then run '{retry}' again."
                ),
                TerminalNeed::Required | TerminalNeed::None => format!(
                    "TMUX is set, but its server is unreachable. Enter a live tmux session, then \
                     run '{retry}' again."
                ),
            },
        };
    }
    match probe.pi_runtime() {
        PiResolution::Resolved(_) => {}
        // ABSENT IS INSTALLABLE. Nothing named `pi` is executable anywhere on
        // PATH, so putting one there is exactly the remedy, and `require`
        // performs it before this refusal is ever shown to anybody.
        PiResolution::Absent => {
            return Preflight {
                code: PreflightCode::PiMissing,
                message: format!(
                    "Pi is required but no runtime was found on PATH. Install it with \
                     '{PI_INSTALL_COMMAND}', then run '{retry}' again{}.",
                    surface.retry_context()
                ),
            };
        }
        // UNUSABLE IS NOT INSTALLABLE, and this is the distinction the first
        // cut of this file got wrong.
        //
        // Measured on a live box: `/usr/local/bin/pi` is a real 0.84.3
        // entry point starting `#!/usr/bin/env node`, on a box with no node.
        // Executable, first on PATH, dead. Reporting that as "no Pi" ran the
        // installer — which cannot help, because the new install lands further
        // down a PATH the broken one still shadows — and then told the operator
        // to open a new shell, which is true for an absent Pi and misleading
        // here: their shell's PATH was never the problem.
        //
        // So it names EVERY candidate it tried and what happened, and does not
        // install. A refusal that sends somebody to the wrong place costs more
        // than one that says less.
        PiResolution::Unusable(candidates) => {
            let tried = candidates
                .iter()
                .map(|candidate| format!("{} ({})", candidate.path, candidate.reason))
                .collect::<Vec<String>>()
                .join("; ");
            return Preflight {
                code: PreflightCode::PiUnusable,
                message: format!(
                    "Pi is on PATH but cannot run. Tried, in PATH order: {tried}. **Installing \
                     Pi again will NOT help** — a new install lands later on PATH and stays \
                     shadowed by the one above. Pi's entry point starts '#!/usr/bin/env node', \
                     so the usual causes are a missing node or one too old for it: measured \
                     2026-08-24, Pi 0.84.3 needs node 22.19 or later and dies on 18.20.4 with a \
                     long stack. The message beside each path is what that candidate actually \
                     said. Make it runnable, remove it, or put a working Pi earlier on PATH, \
                     then run '{retry}' again."
                ),
            };
        }
    }
    if !probe.released() {
        return Preflight {
            code: PreflightCode::NotReleased,
            message: format!(
                "chief cannot find its own resources: no 'resources' directory is installed \
                 beside this binary. Install chief with the installer, or run 'bun run release' \
                 from a checkout, then run '{retry}' again."
            ),
        };
    }
    Preflight { code: PreflightCode::Ready, message: "tmux is ready".to_string() }
}

/// The multiplexer, named once. Both the gate that asks whether it is
/// installed and the clearance that can answer for it read the same name, so
/// the two cannot come to mean different programs.
const TMUX_PROGRAM: &str = "tmux";

/// The name the parent's proof travels under, across the tmux re-exec only.
///
/// Deliberately NOT a member of [`super::tmux::PANE_ENVIRONMENT`]. That list is
/// forwarded to every pane tmux mints for an agent, and to the actuator; this
/// value is appended to ONE respawn — the founder re-exec — so it reaches the
/// process that was cleared and nothing else.
pub(crate) const CLEARANCE_ENV: &str = "CHIEFD_PREFLIGHT_CLEARED";

/// How long a clearance may be honoured after it is minted.
///
/// The re-exec happens within a second of the mint (measured: 794 ms between
/// the outer `preflight.passed` and the inner `process.start`). The bound is
/// generous against a slow host and still far short of a session an operator
/// would leave sitting; anything older is refused and the probe runs.
const CLEARANCE_MAX_AGE: Duration = Duration::from_secs(60);

/// How many hex characters of the `PATH` digest a clearance carries. Wide
/// enough that two different `PATH`s do not collide in practice, short enough
/// to read in a `tmux show-environment` dump.
const PATH_DIGEST_WIDTH: usize = 16;

/// The field separator inside a clearance. A unit separator, so neither a
/// `PATH` digest (hex) nor an absolute path can contain one.
const CLEARANCE_SEPARATOR: char = '\u{1f}';

/// One process's proof, handed to the process it re-execs.
///
/// # Why this exists
///
/// `chief` outside tmux clears the host, mints a session, re-execs itself
/// into the pane, and the inner process clears the same host again. Measured on
/// one real launch: `preflight.passed elapsed_ms=673` in the outer process and
/// `elapsed_ms=657` in the inner one, ~1.3 s of a launch spent asking one
/// question twice.
///
/// # Why it is not simply "skip the inner preflight"
///
/// Because NO gate is unconditionally invariant across the re-exec, and the
/// most important one is the least invariant. The inner process runs under the
/// tmux server's `PATH`, not the operator's — the difference the deleted
/// `PiNotAbsolute` gate was written to name — and Pi's CLI starts
/// with `#!/usr/bin/env node`, so even an absolute Pi path is executed through
/// an interpreter found on that `PATH`. A blanket skip would skip precisely the
/// check that fails.
///
/// What IS invariant, conditionally, is a probe whose answer is a function of
/// (program, `PATH`) alone. Exactly two of those run, and both are
/// subprocesses:
///
/// * `tmux -V` — 190 ms cold on the measured host.
/// * `pi --version` for one absolute candidate — 530 ms.
///
/// So the clearance carries the digest of the `PATH` the proof was made under,
/// and the consumer compares it against its own. The condition is CHECKED, not
/// assumed, and a mismatch runs the full probe.
///
/// # What it can never do
///
/// It cannot clear `$TMUX` being set, the tmux server answering, the release
/// stamp, the Pi ladder RESOLUTION, the executable-bit stat, or the
/// not-absolute gate. The first two are the gates that genuinely differ across
/// the re-exec, and they are the ones the inner process needs most. It is also
/// honoured only on [`Surface::Founder`], the one verb that re-execs, so `attach`,
/// `reset` and the resident actuator ignore it even when the variable is set —
/// the actuator preflight, whose absence once killed every pane it minted, is
/// unreachable from here.
///
/// The remaining exposure of a forged value is one Pi path treated as runnable
/// when it is not, which the founder Pi spawn a few statements later fails on
/// at once. Nothing durable is written on the strength of a clearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Clearance {
    /// When the proof was made, epoch milliseconds.
    minted_at_ms: u128,
    /// Digest of the `PATH` the proof was made under.
    path_digest: String,
    /// The absolute Pi runtime that answered `--version`.
    pi: String,
}

impl Clearance {
    /// Mint a clearance for a decision that PASSED, naming the Pi it proved.
    ///
    /// `None` when this process cannot state the condition its proof depends
    /// on — no `PATH`, or no readable clock. Failing to mint is always safe:
    /// the consumer simply probes for itself.
    pub(crate) fn mint(pi: &str) -> Option<Self> {
        Some(Self {
            minted_at_ms: epoch_millis()?,
            path_digest: path_digest(&trimmed_env("PATH")?),
            pi: pi.to_string(),
        })
    }

    /// The clearance as it travels: one environment pair, or nothing.
    pub(crate) fn forwarded(&self) -> (&'static str, String) {
        (
            CLEARANCE_ENV,
            format!(
                "{}{CLEARANCE_SEPARATOR}{}{CLEARANCE_SEPARATOR}{}",
                self.minted_at_ms, self.path_digest, self.pi
            ),
        )
    }

    /// Parse a clearance out of one environment value.
    ///
    /// Every malformed shape answers `None` rather than a partly-read value:
    /// a clearance that cannot be read whole is not a proof of anything.
    fn parse(raw: &str) -> Option<Self> {
        let mut fields = raw.split(CLEARANCE_SEPARATOR);
        let minted_at_ms = fields.next()?.trim().parse::<u128>().ok()?;
        let path_digest = fields.next()?.trim().to_string();
        let pi = fields.next()?.trim().to_string();
        if fields.next().is_some() || path_digest.is_empty() || pi.is_empty() {
            return None;
        }
        Some(Self { minted_at_ms, path_digest, pi })
    }

    /// Is this clearance honoured by a process whose `PATH` is `path_var`, at
    /// `now_ms`?
    ///
    /// Both halves matter and neither is sufficient. The digest is the
    /// invariance condition itself. The age bound is what stops a value that
    /// outlived the launch it was minted for — a clearance is a statement about
    /// a host a moment ago, not a standing permission.
    fn honoured(&self, path_var: Option<&str>, now_ms: u128) -> bool {
        let Some(path_var) = path_var else { return false };
        if path_digest(path_var) != self.path_digest {
            return false;
        }
        let age = now_ms.saturating_sub(self.minted_at_ms);
        // A clearance stamped in the future is refused as firmly as a stale
        // one: it means the two processes do not share a clock, so the age
        // this bound reasons about is not a quantity either of them knows.
        now_ms >= self.minted_at_ms && age <= CLEARANCE_MAX_AGE.as_millis()
    }

    /// Does this clearance cover the `--version` probe for `candidate`?
    ///
    /// The path must match EXACTLY. A clearance proves that one binary ran; it
    /// says nothing about a different candidate the consumer's own ladder
    /// happened to resolve, and treating it as a blanket "Pi is fine" would
    /// clear the case where the two processes disagree about which Pi to run —
    /// which is the case worth catching.
    fn covers_pi(&self, candidate: &Path) -> bool {
        Path::new(&self.pi) == candidate
    }
}

/// The clearance this process was handed, if it is honoured here and now.
///
/// Read from the environment at the one place that may consume it, so no other
/// caller can pick it up by accident.
fn honoured_clearance() -> Option<Clearance> {
    let raw = trimmed_env(CLEARANCE_ENV)?;
    let clearance = Clearance::parse(&raw)?;
    let now_ms = epoch_millis()?;
    if clearance.honoured(trimmed_env("PATH").as_deref(), now_ms) {
        Some(clearance)
    } else {
        None
    }
}

/// Epoch milliseconds, or `None` when the clock cannot be read.
fn epoch_millis() -> Option<u128> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since| since.as_millis())
}

/// A stable, short digest of one `PATH` value.
///
/// Hashed rather than carried verbatim because a `PATH` is long, is quoted in
/// diagnostics, and is nobody's business but this comparison's. Equality of the
/// digest is the only property used.
fn path_digest(path_var: &str) -> String {
    let mut hex = String::with_capacity(PATH_DIGEST_WIDTH);
    for byte in Sha256::digest(path_var.as_bytes()).iter().take(PATH_DIGEST_WIDTH / 2) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The real host.
pub(crate) struct RealHost {
    // TOMBSTONE: the `home` field. It existed for ONE reader — `released()`,
    // which tested `~/.chief/launcher-root`.is_file(). That marker is deleted
    // with the pointer it was, `released()` now asks the running binary where
    // its own resources are, and a `$HOME` this struct never reads would be a
    // second answer to "which install is this" waiting to disagree with the
    // first. It is removed from `require`, `require_ready` and
    // `require_ready_to_actuate` for the same reason rather than left as an
    // ignored argument.
    /// The parent's proof, when this surface may honour one. `None` on every
    /// surface that does not re-exec, so a clearance in the environment of an
    /// `attach`, a `reset` or the resident actuator is simply not read.
    clearance: Option<Clearance>,
    /// The Pi ladder's answer, resolved at most once per process.
    ///
    /// [`decide`] asks for it, and [`require`] needs the same answer afterwards
    /// to mint the clearance it hands to the re-exec. Resolving twice would pay
    /// the probe this whole packet exists to stop paying twice.
    pi: OnceCell<HostPi>,
}

/// Walk `path_var` for `program` and answer with EVERY absolute, executable
/// candidate, in `PATH` order.
///
/// `Command::new("pi")` already does a PATH walk, so this is not new capability
/// — it is the walk done in a place that can RETURN the locations instead of
/// throwing them away. That difference is the whole packet: the located path is
/// what chiefd hands to the company daemon, and a daemon told `pi` has to look
/// it up again in an environment nobody checked.
///
/// # Why a LIST, and not the first match
///
/// It returned the first executable candidate, and that is the wrong stopping
/// rule when the candidate can be executable and dead. Measured on
/// a live box: `/usr/local/bin/pi` is a 0.84.3 entry point beginning
/// `#!/usr/bin/env node` on a box with **no node at all**. It is executable, it
/// is first on `PATH`, and it cannot run. Stopping there made a working Pi
/// further along the path unreachable, and — because the caller then reported
/// "no Pi" — made the installer run on every single invocation, forever, on a
/// box whose fix was somewhere else entirely.
///
/// So the walk collects and the CALLER decides. `is_executable` stays a
/// parameter so the walk is provable without laying files on disk, and the rule
/// ("every entry that is executable, in order; empty entries mean nothing
/// here") is stated once.
fn candidates_on_path(
    program: &str,
    path_var: &str,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Vec<std::path::PathBuf> {
    path_var
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| Path::new(entry).join(program))
        .filter(|candidate| candidate.is_absolute() && is_executable(candidate))
        .collect()
}

/// Can this path be executed as a program?
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// THE Pi runtime ladder — the ONE answer to "where is Pi?", as a pure
/// function.
///
/// **One rung: `PATH`.** Operator ruling, 2026-08-24 — *"what do you mean it's
/// too old? just use the installed pi system."* Pi is the operator's own
/// program, on their own box, at whatever version they keep it at, and chief
/// runs the one they installed. There is no minimum version, no auto-upgrade,
/// no refusal on an old Pi and no compatibility matrix.
///
/// # What was here, and why each rung went
///
/// 1. `TEAM_LAUNCHER_PI`, an operator pin, read FIRST and verbatim. Deleted
///    from production resolution. A pin is a compatibility mechanism, and there
///    is nothing left to be compatible with once the answer is "the installed
///    Pi" — keeping it would leave a host able to run a Pi that nothing outside
///    that host's environment can see, which is the confusion the ruling ends.
/// 2. `node_modules/.bin/pi` under the recorded launcher checkout — the Pi the
///    FLEET ran, pinned to one version and attested against a table of artifact
///    hashes. Deleted with the pin: it is the version gate wearing a path.
///
/// There is no test-only environment seam either, and that is deliberate rather
/// than an omission. This function is PURE and takes its `PATH` as a parameter,
/// so a unit test injects by argument and an integration test that needs a Pi
/// puts one on `PATH` — which is what an operator does too. A resolution seam
/// nothing uses is a second answer to the product's one question, waiting.
///
/// # The absolute guarantee
///
/// [`candidates_on_path`] keeps only candidates that are already absolute, so this
/// cannot return a relative path. That is what retired `PiNotAbsolute` and the
/// blank-path gate beside it: with the pin gone there is no way for an
/// operator's own string to reach a pane, and a gate on a branch that cannot be
/// taken reports a dead check as a live one.
pub(crate) fn resolve_pi_runtime(
    path_var: Option<&str>,
    is_executable: &dyn Fn(&Path) -> bool,
    probe: &dyn Fn(&Path) -> Result<(), String>,
) -> PiResolution {
    let Some(path_var) = path_var else { return PiResolution::Absent };
    let mut unusable = Vec::new();
    for candidate in candidates_on_path("pi", path_var, is_executable) {
        match probe(&candidate) {
            Ok(()) => return PiResolution::Resolved(candidate.display().to_string()),
            Err(reason) => {
                unusable.push(UnusablePi { path: candidate.display().to_string(), reason });
            }
        }
    }
    if unusable.is_empty() {
        PiResolution::Absent
    } else {
        PiResolution::Unusable(unusable)
    }
}

/// One candidate that exists, is executable, and did not run — with WHY.
///
/// # The reason is not decoration
///
/// "No Pi" and "Pi is here and its runtime is not" are different operator
/// ACTIONS — install Pi, versus install node — and a refusal naming only the
/// first sends somebody to reinstall a file already on disk. BOTH halves were
/// live on a live box in one afternoon: a `#!/usr/bin/env node` Pi on a
/// box with NO node, and then, once node 18.20.4 was installed, that same Pi
/// crashing with a 55KB stack because Pi needs 22.19 or later. Two remedies,
/// one symptom — the probe failed — so the probe's own words are what travel.
///
/// # The node floor is NAMED IN PROSE AND IS NOT A CHECK
///
/// The refusal quotes "22.19 or later, measured 2026-08-24" because an operator
/// told only "install node" has to go and find which node. It is dated, and it
/// is guidance: nothing here reads a node version, compares one, or refuses on
/// one. A version FLOOR in code would be the version gate this product
/// deliberately does not have, one layer down — and it would be wrong the day
/// Pi changed its own requirement, with no way for anyone to notice.
///
/// Quoted rather than classified. A matcher over another program's stderr would
/// be this file guessing, and it would be wrong the first time Pi reworded a
/// diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnusablePi {
    /// The absolute path that was tried.
    pub(crate) path: String,
    /// The first line the probe failed with, bounded.
    pub(crate) reason: String,
}

/// What this host has, which is three answers and not two.
///
/// # Why "absent" and "unusable" must not collapse
///
/// They have OPPOSITE remedies, and collapsing them produced a loop with a
/// wrong diagnosis. An absent Pi is installed. An unusable one — executable,
/// first on `PATH`, and dead, which is what a `#!/usr/bin/env node` entry point
/// is on a box with no node — is not fixed by installing anything, because the
/// new install lands further down a `PATH` the broken one still shadows. The
/// old code reported both as absent, so `chief` ran `curl … | sh` on every
/// invocation and then told the operator to open a new shell — advice that is
/// true for the absent case and actively misleading here, since their shell's
/// `PATH` was never the problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PiResolution {
    /// One absolute path that answered `--version`.
    Resolved(String),
    /// Executable candidates, in `PATH` order, none of which answered, each
    /// carrying the reason it failed.
    Unusable(Vec<UnusablePi>),
    /// Nothing named `pi` is executable anywhere on `PATH`.
    Absent,
}

/// The one command that installs Pi, quoted verbatim when it fails so the
/// operator has nothing to reconstruct.
///
/// Pi's own published installer. Named once because the spawn, the log line and
/// the refusal must be the same string: an operator told to run a command by
/// hand that is not the command that was run is being sent somewhere else.
pub(crate) const PI_INSTALL_COMMAND: &str = "curl -fsSL https://pi.dev/install.sh | sh";

/// Install Pi, once, on a host that does not have it.
///
/// # Why this is a first-RUN action and not a first-CONVERGE one
///
/// It runs from the preflight: a per-command gate, in the operator's own
/// process, with the operator watching. A converge pass is a loop — an install
/// there would run against every person, on a cadence, with nobody to read what
/// it said, and on a slow network the first thing it would do is make every
/// pass slow for a reason nothing reports.
///
/// Output is INHERITED rather than swallowed. An install is the one gate here
/// that can take a minute, and a silent minute reads as a hang.
fn install_pi() -> std::result::Result<(), String> {
    eprintln!("chief: Pi is not installed. Installing it once: {PI_INSTALL_COMMAND}");
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(PI_INSTALL_COMMAND)
        .stdin(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "Pi is required and is not installed. The installer ran and exited {}. Run it by hand \
             and read what it says: {PI_INSTALL_COMMAND}",
            status.code().map_or_else(|| "on a signal".to_string(), |code| code.to_string())
        )),
        Err(error) => Err(format!(
            "Pi is required and is not installed, and the installer could not be started \
             ({error}). Run it by hand: {PI_INSTALL_COMMAND}"
        )),
    }
}

/// What this host answered about Pi, and the version it reported.
///
/// The version is carried out of the SAME `--version` probe the gate already
/// runs, never a second subprocess: the preflight runs before every operator
/// command and each gate here is a real process, so a report that cost an extra
/// spawn would be a report that made every command slower.
#[derive(Debug, Clone)]
pub(crate) struct HostPi {
    /// Resolved, unusable, or absent.
    pub(crate) resolution: PiResolution,
    /// Whatever `pi --version` printed, trimmed to its first line.
    ///
    /// `None` when nothing answered, and also when the probe was SKIPPED
    /// because a parent's clearance already proved this candidate — the version
    /// is a REPORT and never a gate, so it is simply absent rather than worth a
    /// subprocess to recover.
    pub(crate) version: Option<String>,
}

/// [`resolve_pi_runtime`] against this host: the one value every process uses,
/// or the refusal that says WHY there is none.
///
/// Called by [`super::daemon`] (which tells the company daemon what panes must
/// exec) and by [`super::founder`]. Two resolutions would be two answers, and
/// the second answer is the one nobody checks — which is exactly what shipped
/// twice: the preflight resolved its own value and the daemon it started
/// resolved another.
///
/// # Why this returns the REASON and not an `Option`
///
/// It was `real_pi_runtime() -> Option<String>`, and both callers turned `None`
/// into "install Pi". `None` now covers `Unusable` as well as `Absent`, so on
/// the box this packet is about, those two backstops would have handed the
/// operator the ABSENT remedy for the UNUSABLE case — the precise misdirection
/// the rest of this file exists to delete, reintroduced one layer down. One
/// symptom, two remedies, one message is the defect; a function that can only
/// answer "nothing" makes its callers commit it.
///
/// Caught by Sanchez reviewing #1243, in the same PR that fixes the original.
/// Both call sites are reachable only by a caller that skipped the gate, so
/// nothing was on fire — but a fail-closed backstop that misdirects is worth
/// less than one that says "ask the preflight".
pub(crate) fn pi_runtime_or_refusal() -> std::result::Result<std::path::PathBuf, String> {
    match host_pi(None).resolution {
        PiResolution::Resolved(path) => Ok(std::path::PathBuf::from(path.trim())),
        PiResolution::Unusable(candidates) => Err(format!(
            "a Pi is on PATH and cannot run: {}. Installing another will NOT help — it lands \
             later on PATH and stays shadowed. Make it runnable, remove it, or put a working Pi \
             earlier on PATH.",
            candidates
                .iter()
                .map(|candidate| format!("{} ({})", candidate.path, candidate.reason))
                .collect::<Vec<String>>()
                .join("; ")
        )),
        PiResolution::Absent => {
            Err(format!("no Pi was found on PATH. Install it with '{PI_INSTALL_COMMAND}'."))
        }
    }
}

/// [`pi_runtime_or_refusal`], with the parent's proof allowed to stand in for the
/// `--version` probe of the ONE candidate it names.
///
/// The ladder still runs in full, in THIS process's own environment: the `PATH`
/// walk happens here. Only the subprocess is skipped, and only for a candidate
/// the parent proved under a `PATH` identical to this one. So a clearance cannot
/// change WHICH Pi this process chooses — if the two processes resolve different
/// candidates, [`Clearance::covers_pi`] refuses and the probe runs.
fn host_pi(clearance: Option<&Clearance>) -> HostPi {
    // The version travels out of the probe closure rather than being returned
    // by it: `resolve_pi_runtime` is pure and answers a yes/no question, and
    // widening its contract so one caller can collect a diagnostic would make
    // every OTHER caller carry it.
    let reported: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let resolution =
        resolve_pi_runtime(trimmed_env("PATH").as_deref(), &is_executable_file, &|candidate| {
            if clearance.is_some_and(|cleared| cleared.covers_pi(candidate)) {
                tracing::debug!(
                    event = "preflight.probe.cleared",
                    command = %candidate.display(),
                    "the parent process proved this runtime under this PATH"
                );
                return Ok(());
            }
            match command_version(candidate) {
                Ok(version) => {
                    *reported.borrow_mut() = version;
                    Ok(())
                }
                Err(reason) => Err(reason),
            }
        });
    HostPi { resolution, version: reported.into_inner() }
}

/// One environment read, trimmed, with blank treated as unset.
fn trimmed_env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|v| !v.is_empty())
}

/// Is `command` runnable? Probed by running its version flag, because presence
/// on disk is not the same fact as "this binary works on this host".
fn command_answers(command: &Path, version_flag: &str) -> bool {
    let started = std::time::Instant::now();
    let answered = Command::new(command)
        .arg(version_flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    // The preflight runs before every operator command, and each gate here is
    // a real subprocess. A Pi or tmux binary that takes seconds to answer its
    // own version flag makes every command slow for a reason nothing else
    // reports.
    tracing::debug!(
        event = "preflight.probe",
        command = %command.display(),
        answered,
        elapsed_ms = chiefd_log::elapsed_ms(started),
        "probed a host command"
    );
    answered
}

impl RealHost {
    /// This host's Pi answer, resolved at most once.
    fn host_pi(&self) -> &HostPi {
        self.pi.get_or_init(|| host_pi(self.clearance.as_ref()))
    }

    /// The absolute path, when one answered.
    fn resolved_path(&self) -> Option<&str> {
        match &self.host_pi().resolution {
            PiResolution::Resolved(path) => Some(path),
            PiResolution::Unusable(_) | PiResolution::Absent => None,
        }
    }
}

/// Run `pi --version`: `Ok` with whatever it printed, or `Err` with the line it
/// failed on.
///
/// One subprocess doing three jobs. The gate needs a yes/no, the report needs
/// the version string, and the refusal needs the REASON — and the preflight runs
/// before every operator command, so a second spawn to recover any of them would
/// make every command slower.
///
/// `Ok(None)` — it ran and printed nothing useful — is deliberately
/// representable: a Pi that runs is a Pi, whatever it says about itself, since
/// no version gate exists to disappoint.
///
/// The failure line is BOUNDED and taken from stderr's first non-empty line.
/// Pi on too-old node emits a 55KB stack; a refusal that pasted all of it would
/// bury the one line that names the cause.
fn command_version(command: &Path) -> Result<Option<String>, String> {
    let started = std::time::Instant::now();
    let output = Command::new(command)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("could not be started: {error}"))?;
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string);
    tracing::debug!(
        event = "preflight.probe",
        command = %command.display(),
        answered = output.status.success(),
        version = version.as_deref().unwrap_or("unreported"),
        elapsed_ms = chiefd_log::elapsed_ms(started),
        "probed the Pi runtime"
    );
    if output.status.success() {
        return Ok(version);
    }
    let first_line = String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(PROBE_FAILURE_LINE_LIMIT).collect::<String>());
    Err(first_line.unwrap_or_else(|| {
        output
            .status
            .code()
            .map_or_else(|| "died on a signal".to_string(), |code| format!("exited {code}"))
    }))
}

/// How much of a failing probe's first stderr line is quoted.
///
/// Bounded because the measured case is Pi on node 18 printing a 55KB stack:
/// the refusal must carry the cause, not the transcript.
const PROBE_FAILURE_LINE_LIMIT: usize = 200;

impl HostProbe for RealHost {
    fn has_command(&self, command: &str) -> bool {
        // A clearance exists only for a decision that PASSED, and this gate is
        // the first one that decision cleared — so a clearance is itself the
        // proof that `tmux -V` answered, under a `PATH` this process has just
        // been shown to share. Not skipped for want of an answer: skipped
        // because the answer is already in hand.
        //
        // Named, rather than blanket-cleared for whatever it is asked about: a
        // clearance is a statement about the programs the parent probed, and
        // `tmux` is the only one this gate ever asks for.
        if command == TMUX_PROGRAM && self.clearance.is_some() {
            tracing::debug!(
                event = "preflight.probe.cleared",
                command,
                "the parent process proved this command under this PATH"
            );
            return true;
        }
        command_answers(Path::new(command), "-V")
    }

    fn env(&self, name: &str) -> Option<String> {
        trimmed_env(name)
    }

    fn tmux_reachable(&self) -> bool {
        let mut command = Command::new("tmux");
        if let Some(socket) = self.env("TEAM_LAUNCHER_TMUX_SOCKET") {
            command.arg("-L").arg(socket);
        }
        command
            .args(["display-message", "-p", "#{session_id}"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn pi_runtime(&self) -> PiResolution {
        self.host_pi().resolution.clone()
    }

    fn released(&self) -> bool {
        // TOMBSTONE: this was `~/.chief/launcher-root`.is_file() — a MARKER
        // that `bun run release` had run on this box. The marker is deleted
        // with the pointer it was, so the question is asked of the thing that
        // actually matters: does this running binary have its resources beside
        // it? That is a stronger question than the marker answered, because a
        // marker left behind by a release whose files were since removed said
        // "installed" about an install that no longer worked.
        super::paths::resource_root().is_some()
    }
}

/// Refuse before a terminal-owning lifecycle verb touches a company.
///
/// The caller names its own verb, because the refusal quotes it back as the
/// command to re-run once the gate is fixed. A default here would be the same
/// defect the [`Surface`] doc records, one layer down.
///
/// # Errors
/// [`super::LifecycleError::Preflight`] carrying the failing gate's message.
/// Answers the clearance this pass proved, for the one caller that re-execs
/// itself. Every other caller drops it, which costs nothing: an unused
/// clearance is never read by anybody.
pub(crate) fn require_ready(surface: Surface) -> super::Result<Option<Clearance>> {
    debug_assert!(
        surface.terminal() != TerminalNeed::None,
        "require_ready clears a terminal-owning verb"
    );
    require(surface)
}

/// Refuse before `chief actuate` becomes a company's actuator.
///
/// The same decision, on the surface that has no terminal of its own. It is a
/// refusal at start-up rather than a failure per round because this process
/// SPAWNS every person: a host that cannot run Pi cannot run anybody, and
/// discovering that once per second inside a tmux error is not discovering it.
///
/// # Errors
/// [`super::LifecycleError::Preflight`] carrying the failing gate's message.
pub(crate) fn require_ready_to_actuate() -> super::Result<()> {
    require(Surface::ResidentActuator).map(|_| ())
}

/// Which surfaces may honour a parent's proof: the one that re-execs itself,
/// and no other.
///
/// `attach`, `reset` and the resident actuator are started by tmux or by an
/// operator, never by a chiefd process that has just cleared this host, so a
/// clearance in their environment is somebody else's value and is not read.
const fn honours_clearance(surface: Surface) -> bool {
    matches!(surface, Surface::Founder)
}

fn require(surface: Surface) -> super::Result<Option<Clearance>> {
    let started = std::time::Instant::now();
    let clearance = honours_clearance(surface).then(honoured_clearance).flatten();
    let cleared = clearance.is_some();
    let mut host = RealHost { clearance: clearance.clone(), pi: OnceCell::new() };
    let mut result = decide(&host, surface);
    // FIRST RUN INSTALLS PI. Absent is not a refusal on a box that has never
    // had it — it is the one thing chief can fix for the operator, once, with
    // Pi's own installer, before it asks them to do anything.
    //
    // NOT on the resident actuator. That surface is headless and on a cadence,
    // there is nobody to read an installer's output, and a company that reached
    // it already ran a Pi. An install there would be a network call in a
    // supervisor loop.
    //
    // `PiMissing` ONLY, never `PiUnusable`. An install cannot fix a broken
    // candidate that shadows it — the new binary lands further down the same
    // PATH — so running the installer there would be a `curl … | sh` on every
    // invocation of `chief`, forever, on a box whose fix is elsewhere. That is
    // not a hypothetical: it is what a live box would have done, with a
    // node-shebang Pi first on PATH and no node installed.
    if result.code == PreflightCode::PiMissing && surface != Surface::ResidentActuator {
        install_pi().map_err(super::LifecycleError::Preflight)?;
        // A FRESH PROBE, because the old one CACHED the absence. Re-deciding
        // against the same host would re-read a `OnceCell` that already holds
        // the old answer and report the install as having changed nothing.
        host = RealHost { clearance, pi: OnceCell::new() };
        result = decide(&host, surface);
        // A POST-INSTALL `PiUnusable` FALLS THROUGH TO ITS OWN REFUSAL, which
        // is the one that names the shadowing candidate. It must not be
        // answered with the sentence below: the installer worked, the binary is
        // on PATH, and something ahead of it is the problem.
        if result.code == PreflightCode::PiMissing {
            // THE INSTALLER SUCCEEDED AND PATH STILL CANNOT SEE IT, which is
            // the ordinary outcome rather than an exotic one: an installer
            // writes a binary and appends to a shell profile, and THIS shell
            // was started before that line existed. Saying "install Pi" here
            // would send the operator to reinstall what they just installed.
            return Err(super::LifecycleError::Preflight(format!(
                "Pi was installed, but it is not on this shell's PATH — the installer appends to \
                 your shell profile, and this shell started before that. Open a new shell (or \
                 source your profile), then run '{}' again.",
                surface.retry()
            )));
        }
    }
    if result.ok() {
        // WHICH PI, AND WHICH VERSION. The version is now the operator's own
        // choice and nothing gates on it, so the only way anybody can tell what
        // a company is running is for the resolution to say so. One line, from
        // a probe that already ran.
        if let Some(path) = host.resolved_path() {
            let reported = host.host_pi().version.clone();
            tracing::info!(
                event = "preflight.pi",
                path,
                version = reported.as_deref().unwrap_or("unreported"),
                "resolved the Pi runtime"
            );
            warn_below_pi_floor(reported.as_deref());
        }
        tracing::info!(
            event = "preflight.passed",
            elapsed_ms = chiefd_log::elapsed_ms(started),
            cleared,
            "the host preflight passed"
        );
        // Minted from the Pi this decision actually resolved, read back out of
        // the probe's own cache rather than resolved a second time.
        let proved = host.resolved_path().map(str::to_string);
        Ok(proved.as_deref().and_then(Clearance::mint))
    } else {
        tracing::error!(
            event = "preflight.refused",
            elapsed_ms = chiefd_log::elapsed_ms(started),
            reason = %result.message,
            "the host preflight refused"
        );
        Err(super::LifecycleError::Preflight(result.message))
    }
}

/// Say so, once, when this host's Pi is below the declared floor.
///
/// # A WARNING, NOT A GATE, and the distinction is the whole design
///
/// `host_primitives::pi_floor::MINIMUM_PI_VERSION` is a floor with exactly two
/// readers, and they treat it differently on purpose:
///
///   * here, in the preflight that runs before EVERY operator verb, it only
///     talks. A company that boots today keeps booting tomorrow, because a
///     version gate that refuses work an operator was already doing is a
///     product that broke itself over a number;
///   * in `chief upgrade`, it refuses, because that is the one moment the
///     product can offer to fix it — it prompts to run Pi's own updater.
///
/// An UNREADABLE version says nothing at all. `meets_floor` answers `None` when
/// it cannot find a version in what `pi --version` printed, and `None` is
/// "unknown", never "below": Pi is free to change how it prints its banner, and
/// a warning fired by a formatting change would train the operator to ignore
/// this line.
fn warn_below_pi_floor(reported: Option<&str>) {
    use host_primitives::pi_floor::MINIMUM_PI_VERSION;
    let Some(message) = pi_floor_warning(reported) else { return };
    eprintln!("{message}");
    tracing::warn!(
        event = "preflight.pi.below_floor",
        version = reported.unwrap_or(""),
        floor = MINIMUM_PI_VERSION,
        "the installed Pi is below the declared minimum"
    );
}

/// The warning line, or `None` when there is nothing to say.
///
/// Split out from [`warn_below_pi_floor`] so the RULE is testable without
/// capturing stderr: three inputs — below the floor, at or above it, and
/// unreadable — and only the first produces a line.
pub(crate) fn pi_floor_warning(reported: Option<&str>) -> Option<String> {
    use host_primitives::pi_floor::{meets_floor, MINIMUM_PI_VERSION};
    let version = reported?;
    if meets_floor(version) != Some(false) {
        return None;
    }
    Some(format!(
        "chief: Pi {version} is below the minimum this chief declares ({MINIMUM_PI_VERSION}). \
         Everything still runs; update when convenient with 'pi update', or run 'chief upgrade', \
         which offers to do it for you."
    ))
}

/// The absolute Pi runtime a company daemon must be TOLD, rather than left to
/// look up for itself.
///
/// # The defect this closes
///
/// `chiefd` publishes a `piBinary` in every person's launch-catalog
/// entry, and that string is what the pane execs. It read `CHIEFD_PI_BINARY`
/// and, finding nothing, defaulted to the bare name `pi` — a value nothing in
/// the product ever set, so EVERY company that has ever run shipped a bare name
/// to its panes. On a host whose operator pinned Pi with `TEAM_LAUNCHER_PI`
/// (the variable the preflight itself reads, the Founder pane reads, and
/// `tmux::PANE_ENVIRONMENT` forwards) the pin was therefore dropped at
/// the one process that decides what a person execs. The CEO pane ran `pi`
/// against the tmux server's PATH, did not find it, died at creation, tmux
/// reaped the empty window, and the actuator reported
/// `unusable window dimensions "\t\n"` once per second — while the preflight
/// that had just cleared the host was, in its own process, entirely correct.
///
/// Resolved HERE, at the operator's own spawn site, from the same function the
/// preflight decided on: not forwarded through an environment allowlist,
/// because an allowlist is a hand-maintained inventory of what somebody once
/// remembered a child needed, and this is a value chiefd already holds.
///
/// # Errors
/// [`super::LifecycleError::Preflight`] naming the same recovery the preflight
/// gate would have named. Reachable only by a caller that skipped the gate, so
/// it fails closed rather than passing a guess down.
pub(crate) fn pi_binary_for_daemon() -> super::Result<std::path::PathBuf> {
    // NO SECOND ABSOLUTE CHECK. The ladder resolves through `candidates_on_path`,
    // which keeps absolute candidates only, so the branch this used to guard
    // cannot be reached — see the tombstone on `PreflightCode`.
    pi_runtime_or_refusal().map_err(|why| {
        super::LifecycleError::Preflight(format!(
            "The company daemon cannot be told what its panes must exec, because {why}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decide, HostProbe, PiResolution, PreflightCode, Surface, TerminalNeed, UnusablePi,
    };

    /// One unusable candidate, for the fake host.
    fn unusable(path: &str, reason: &str) -> UnusablePi {
        UnusablePi { path: path.to_string(), reason: reason.to_string() }
    }

    #[derive(Clone)]
    struct FakeHost {
        tmux_installed: bool,
        inside_tmux: bool,
        tmux_reachable: bool,
        pi: PiResolution,
        released: bool,
    }

    impl FakeHost {
        fn ready() -> Self {
            Self {
                tmux_installed: true,
                inside_tmux: true,
                tmux_reachable: true,
                pi: PiResolution::Resolved("/installed/pi".to_string()),
                released: true,
            }
        }
    }

    impl HostProbe for FakeHost {
        fn has_command(&self, command: &str) -> bool {
            command == "tmux" && self.tmux_installed
        }
        fn env(&self, name: &str) -> Option<String> {
            (name == "TMUX" && self.inside_tmux).then(|| "/tmp/tmux-0/default,1,0".to_string())
        }
        fn tmux_reachable(&self) -> bool {
            self.tmux_reachable
        }
        fn pi_runtime(&self) -> PiResolution {
            self.pi.clone()
        }
        fn released(&self) -> bool {
            self.released
        }
    }

    #[test]
    fn a_ready_host_passes() {
        assert_eq!(decide(&FakeHost::ready(), Surface::Founder).code, PreflightCode::Ready);
    }

    #[test]
    fn each_gate_refuses_with_its_own_code_and_a_recovery_instruction() {
        let cases: [(FakeHost, PreflightCode, &str); 4] = [
            (
                FakeHost { tmux_installed: false, ..FakeHost::ready() },
                PreflightCode::TmuxMissing,
                "Install tmux",
            ),
            (
                FakeHost { tmux_reachable: false, ..FakeHost::ready() },
                PreflightCode::TmuxUnreachable,
                "Enter a live tmux session",
            ),
            (
                FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() },
                PreflightCode::PiMissing,
                // THE EXACT COMMAND, not the word "install". With no version
                // gate left, an absent Pi is the only Pi refusal there is, and
                // the operator should not have to go and find the installer.
                "curl -fsSL https://pi.dev/install.sh | sh",
            ),
            (
                FakeHost { released: false, ..FakeHost::ready() },
                PreflightCode::NotReleased,
                "bun run release",
            ),
        ];
        for (host, expected, recovery) in cases {
            let result = decide(&host, Surface::Founder);
            assert_eq!(result.code, expected);
            assert!(!result.ok());
            assert!(
                result.message.contains(recovery),
                "{expected:?} must tell the operator what to run; got {}",
                result.message
            );
        }
    }

    /// THE LIVE-PROOF REGRESSION (#751/P8).
    ///
    /// `chief actuate` is the only verb that spawns anybody, and it asked for
    /// no clearance at all. On a host whose PATH had no `pi` — the exact shape
    /// of a resident actuator started from a non-login shell — it looped
    /// forever, once per second, minting a pane whose command died instantly
    /// and then reporting `unusable window dimensions "\t\n"`, which names
    /// neither the cause nor anything an operator can act on.
    #[test]
    fn the_resident_actuator_refuses_a_host_with_no_pi_runtime() {
        let host = FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() };
        let result = decide(&host, Surface::ResidentActuator);
        assert_eq!(result.code, PreflightCode::PiMissing);
        assert!(!result.ok(), "an actuator that cannot spawn must never start");
        assert!(result.message.contains(super::PI_INSTALL_COMMAND), "{}", result.message);
        assert!(
            result.message.contains("chief actuate <company>"),
            "the recovery must name the verb that was refused; got {}",
            result.message
        );
    }

    /// The other half, and the reason `actuate` had no preflight to begin with:
    /// the resident actuator CREATES the company's session, so demanding that
    /// it already be sitting in one would refuse the only verb that can start
    /// anybody. Both terminal gates are skipped, and the host gates are not.
    #[test]
    fn the_resident_actuator_is_cleared_outside_tmux_because_it_creates_the_session() {
        let host = FakeHost { inside_tmux: false, tmux_reachable: false, ..FakeHost::ready() };
        assert_eq!(decide(&host, Surface::ResidentActuator).code, PreflightCode::Ready);
        // `reset` is the one verb left that still demands a live client.
        assert_eq!(decide(&host, Surface::Reset).code, PreflightCode::OutsideTmux);
    }

    /// `attach` now STARTS an actuator, so it clears the actuator's own gate
    /// before creating the window — and this is the property that makes doing
    /// so honest rather than decorative: the operator surface is strictly
    /// stricter than the actuator surface, so a host attach has already cleared
    /// can never be refused by the second call, and the second call can still
    /// refuse a host the first one never asked about.
    ///
    /// It matters because the two decisions are made in two different
    /// environments: attach reads its own, and the tmux server the actuator
    /// lands in reads whatever it was started with. The forwarded pane
    /// environment (`attach::ACTUATOR_ENVIRONMENT`) is what keeps them the
    /// same; this asserts the ordering the forwarding relies on.
    #[test]
    fn a_host_cleared_for_the_operator_surface_is_always_cleared_for_the_actuator() {
        let hosts = [
            FakeHost::ready(),
            FakeHost { inside_tmux: false, ..FakeHost::ready() },
            FakeHost { tmux_reachable: false, ..FakeHost::ready() },
            FakeHost { tmux_installed: false, ..FakeHost::ready() },
            FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() },
            FakeHost { released: false, ..FakeHost::ready() },
        ];
        for host in hosts {
            if decide(&host, Surface::Founder).ok() {
                assert!(
                    decide(&host, Surface::ResidentActuator).ok(),
                    "the operator surface must subsume the actuator surface"
                );
            }
        }
    }

    /// The host gates apply to both surfaces; only their recovery text differs.
    #[test]
    fn the_host_gates_hold_on_the_actuator_surface_too() {
        for (host, expected) in [
            (FakeHost { tmux_installed: false, ..FakeHost::ready() }, PreflightCode::TmuxMissing),
            (FakeHost { released: false, ..FakeHost::ready() }, PreflightCode::NotReleased),
        ] {
            let result = decide(&host, Surface::ResidentActuator);
            assert_eq!(result.code, expected);
            assert!(
                result.message.contains("chief actuate <company>"),
                "{expected:?} must name the actuator verb; got {}",
                result.message
            );
        }
    }

    /// The operator-facing text is unchanged by the surface split — these are
    /// the exact sentences that shipped, and a caller reading them must not
    /// have to relearn them because a second surface was added beside them.
    ///
    /// THE PI SENTENCE MOVED ON PURPOSE, and is the one line here that is not
    /// the shipped text. It read *"Pi is required but no runtime was found.
    /// Install Pi, then run 'chief' again."* — which named a remedy without
    /// naming how to perform it, and was written when a pin or a checkout build
    /// might also have been the cause. There is one cause now, so the sentence
    /// says where it looked and quotes the command that fixes it.
    #[test]
    fn the_operator_surface_keeps_its_wording_verbatim() {
        assert_eq!(
            decide(&FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() }, Surface::Founder)
                .message,
            format!(
                "Pi is required but no runtime was found on PATH. Install it with \
                 '{}', then run 'chief' again.",
                super::PI_INSTALL_COMMAND
            )
        );
        assert_eq!(
            decide(&FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() }, Surface::Attach)
                .message,
            format!(
                "Pi is required but no runtime was found on PATH. Install it with \
                 '{}', then run 'chief attach <company>' again.",
                super::PI_INSTALL_COMMAND
            )
        );
        // `attach` boots its own session now, so it takes the SAME tmux-missing
        // sentence `new` does: install tmux, and chief does the rest. It must
        // not tell an operator to start a session by hand for a verb that would
        // have started one for them.
        assert_eq!(
            decide(&FakeHost { tmux_installed: false, ..FakeHost::ready() }, Surface::Attach)
                .message,
            "tmux is required, and it is the one thing chief cannot start for you. Install tmux \
             (macOS: 'brew install tmux'; Debian/Ubuntu: 'apt-get install -y tmux'), then run \
             'chief attach <company>' again — chief starts the session itself."
        );
        // `reset` is the verb that still needs a live client, and still says so.
        assert_eq!(
            decide(&FakeHost { tmux_installed: false, ..FakeHost::ready() }, Surface::Reset)
                .message,
            "tmux is required. Install tmux (macOS: 'brew install tmux'; Debian/Ubuntu: \
             'apt-get install -y tmux'), start a tmux session, then run 'chief reset <company>' again."
        );
        // THE NOT-RELEASED SENTENCE ALSO MOVED, and for the same reason the Pi
        // one did: the question changed. `released()` no longer asks whether a
        // `bun run release` marker exists — it asks whether a `resources`
        // directory sits beside this binary, which is true of the installer's
        // output and the checkout build alike. So the sentence names what is
        // actually missing (the resources) and both ways to supply it, rather
        // than naming one build command as though it were the only path.
        assert_eq!(
            decide(&FakeHost { released: false, ..FakeHost::ready() }, Surface::Founder).message,
            "chief cannot find its own resources: no 'resources' directory is installed beside \
             this binary. Install chief with the installer, or run 'bun run release' from a \
             checkout, then run 'chief' again."
        );
    }

    /// THE LIVE-PROOF REGRESSION (2026-08-10).
    ///
    /// Every terminal-owning verb shared one `OperatorTerminal` surface whose
    /// recovery text was the literal `chief`. So the two verbs that name a
    /// company — `attach` and `reset` — refused with instructions to run the
    /// verb that CREATES one instead. Reproduced live: `chief attach
    /// northwind-labs --yes` from a pty outside tmux answered "ChiefD only runs
    /// inside tmux. Start one with 'tmux new -s companies', then run 'chiefd
    /// new' again." An operator who followed that would have made a second
    /// company rather than entering the one they asked for.
    ///
    /// Asserted on every gate, not just the one that was hit: the defect was in
    /// the surface's `retry()`, so it applied to all five refusals at once, and
    /// pinning only `OutsideTmux` would leave the other four free to regress.
    #[test]
    fn a_refusal_names_the_verb_the_operator_actually_typed() {
        // Every gate that refuses on ALL THREE verbs. `inside_tmux: false` left
        // this list when `new` started booting its own session: it is no longer
        // a refusal there, and a host that PASSES cannot demonstrate anything
        // about refusal copy. It is still asserted for the two verbs that keep
        // the gate, below.
        let gates = [
            FakeHost { tmux_installed: false, ..FakeHost::ready() },
            FakeHost { tmux_reachable: false, ..FakeHost::ready() },
            FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() },
            FakeHost { released: false, ..FakeHost::ready() },
        ];
        for (surface, invocation) in [
            (Surface::Attach, "chief attach <company>"),
            (Surface::Reset, "chief reset <company>"),
            (Surface::Founder, "chief"),
        ] {
            assert_eq!(surface.retry(), invocation, "each surface owns its exact retry command");
            for host in gates.clone() {
                let result = decide(&host, surface);
                assert!(!result.ok(), "this host must refuse; got {}", result.message);
                assert!(
                    result.message.contains(invocation),
                    "{surface:?} must quote back '{invocation}'; got {}",
                    result.message
                );
            }
        }
    }

    /// THE OPERATOR-REPORTED DEFECT (2026-08-10).
    ///
    /// *"Every time I run `chief` it asks me to be inside a tmux. Can you
    /// boot a tmux into `chief` instead?"* A host that is ready in every
    /// other respect and simply has no ambient tmux client must CLEAR the
    /// preflight for `chief`, because clearing it is what lets
    /// [`super::super::founder::run`] reach the branch that starts the
    /// session. While this said `OutsideTmux` that branch was dead code.
    #[test]
    fn chiefd_new_is_cleared_outside_tmux_because_it_starts_its_own_session() {
        let host = FakeHost { inside_tmux: false, tmux_reachable: false, ..FakeHost::ready() };

        let result = decide(&host, Surface::Founder);

        assert_eq!(result.code, PreflightCode::Ready, "got: {}", result.message);
        assert!(result.ok());
        // `attach` boots its own client for the same reason `new` does — the
        // operator asked for exactly this, in the same words, one door over.
        assert_eq!(decide(&host, Surface::Attach).code, PreflightCode::Ready);
        // `reset` keeps the gate: it sheds a company from inside the context
        // the operator is already in, and creates none of its own.
        assert_eq!(decide(&host, Surface::Reset).code, PreflightCode::OutsideTmux);
        assert_eq!(decide(&host, Surface::ResidentActuator).code, PreflightCode::Ready);
    }

    /// The reachability gate is NOT skipped for `new` along with the
    /// outside-tmux one.
    ///
    /// `run` reads `$TMUX` itself and hosts the Founder in that session
    /// when it is set. A stale `$TMUX` — a shell that outlived its server —
    /// therefore sends it to tag a pane on a server that is not there, which is
    /// a raw tmux error rather than a refusal. So `BootsOwn` boots its own
    /// session only when there is genuinely none.
    #[test]
    fn a_stale_tmux_still_refuses_chiefd_new_rather_than_booting_a_second_server() {
        let host = FakeHost { inside_tmux: true, tmux_reachable: false, ..FakeHost::ready() };

        let result = decide(&host, Surface::Founder);

        assert_eq!(result.code, PreflightCode::TmuxUnreachable);
        // ...and the recovery names BOTH ways out, including the one only this
        // surface has: chiefd will start a session if `$TMUX` is not lying.
        assert!(result.message.contains("unset TMUX"), "{}", result.message);
        assert!(result.message.contains("chief"), "{}", result.message);
    }

    /// The refusal chiefd genuinely cannot fix names the install command.
    ///
    /// A host with no tmux at all is the one gate left that a human must clear,
    /// so it is the one that must say what to run. "Install tmux" told somebody
    /// who does not have tmux to install tmux.
    #[test]
    fn a_host_with_no_tmux_is_told_what_to_install_not_just_what_is_missing() {
        let host = FakeHost { tmux_installed: false, inside_tmux: false, ..FakeHost::ready() };

        let result = decide(&host, Surface::Founder);

        assert_eq!(result.code, PreflightCode::TmuxMissing);
        assert!(result.message.contains("brew install tmux"), "{}", result.message);
        assert!(result.message.contains("apt-get install -y tmux"), "{}", result.message);
        // And it must NOT send the operator to start a session by hand — that
        // is the very step this packet removed.
        assert!(!result.message.contains("start a tmux session"), "{}", result.message);
        assert!(result.message.contains("chief starts the session itself"), "{}", result.message);
        // `attach` takes this same arm now — it starts its own session too.
        assert!(
            decide(&host, Surface::Attach).message.contains("chief starts the session itself"),
            "{}",
            decide(&host, Surface::Attach).message
        );
        // `reset` is the one verb left that needs a live client, and still says so.
        assert!(
            decide(&host, Surface::Reset).message.contains("start a tmux session"),
            "{}",
            decide(&host, Surface::Reset).message
        );
    }

    /// The two verbs that put an operator into a company boot their own
    /// terminal; the one that acts on a company from inside does not.
    #[test]
    fn the_verbs_that_seat_an_operator_boot_their_own_terminal() {
        assert_eq!(Surface::Founder.terminal(), TerminalNeed::BootsOwn);
        assert_eq!(Surface::Attach.terminal(), TerminalNeed::BootsOwn);
        assert_eq!(Surface::Reset.terminal(), TerminalNeed::Required);
        assert_eq!(Surface::ResidentActuator.terminal(), TerminalNeed::None);
    }

    #[test]
    fn the_cheapest_most_actionable_refusal_wins_when_several_gates_would_fail() {
        // A completely unprepared host is told to install tmux, not that its
        // release is missing — the ordering is the ported contract.
        let host = FakeHost {
            tmux_installed: false,
            inside_tmux: false,
            tmux_reachable: false,
            pi: PiResolution::Absent,
            released: false,
        };
        assert_eq!(decide(&host, Surface::Founder).code, PreflightCode::TmuxMissing);
    }

    // TOMBSTONE: `a_blank_pi_path_is_a_refusal_not_a_pass`. A blank could only
    // come from an exported-but-empty `TEAM_LAUNCHER_PI`; the pin is deleted
    // and `candidates_on_path` cannot answer with an empty string.

    /// AN ABSENT PI NAMES THE COMMAND THAT INSTALLS IT.
    ///
    /// TOMBSTONES, because five tests stood here and all five were about the
    /// deleted pin:
    ///
    /// * `a_pinned_pi_that_cannot_run_says_so_instead_of_install_pi` — a
    ///   `TEAM_LAUNCHER_PI` naming a file that exists and cannot execute (Pi's
    ///   CLI starts `#!/usr/bin/env node`, so a box with bun and no node has
    ///   every file in place and can run none of them). With no pin, that is
    ///   simply a Pi on `PATH` that does not answer, which is this case.
    /// * `no_pi_at_all_still_says_install_pi` — subsumed here, and sharpened:
    ///   the word "install" is not the remedy, the COMMAND is.
    /// * `a_pi_runtime_only_this_process_could_find_is_refused_by_name`,
    ///   `the_not_absolute_refusal_names_the_verb_that_was_refused` and
    ///   `a_relative_pin_is_not_reported_as_a_missing_pi` — all three drove a
    ///   bare or relative name through `pi_runtime`, which only an operator's
    ///   pin could produce. `candidates_on_path` keeps absolute candidates only,
    ///   so the state cannot be constructed any more; the guarantee moved to
    ///   `the_ladder_can_only_answer_with_an_absolute_path` below, where it is
    ///   asserted of the resolver rather than of a gate downstream of it.
    #[test]
    fn an_absent_pi_names_the_exact_install_command_on_every_surface() {
        let host = FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() };
        for (surface, verb) in [
            (Surface::Founder, "chief"),
            (Surface::Attach, "chief attach <company>"),
            (Surface::Reset, "chief reset <company>"),
            (Surface::ResidentActuator, "chief actuate <company>"),
        ] {
            let result = decide(&host, surface);
            assert_eq!(result.code, PreflightCode::PiMissing, "{surface:?}");
            assert!(
                result.message.contains(super::PI_INSTALL_COMMAND),
                "{surface:?} must quote the installer verbatim: {}",
                result.message
            );
            // Each surface still names its OWN verb, so the operator is told
            // what to re-run rather than a default somebody guessed.
            assert!(result.message.contains(verb), "{surface:?}: {}", result.message);
        }
    }

    /// **AN UNUSABLE PI IS A DIFFERENT REFUSAL, AND MUST NOT BE INSTALLED
    /// OVER.**
    ///
    /// The measured box: a real 0.84.3 `pi` at `/usr/local/bin/pi` beginning
    /// `#!/usr/bin/env node`, first on PATH, on a host with no node. Executable
    /// and dead. Reported as "no Pi", it made `chief` run `curl … | sh` on
    /// every invocation — the install cannot win, because the new binary lands
    /// further down the same PATH — and then told the operator to open a new
    /// shell, which is the remedy for a DIFFERENT problem.
    #[test]
    fn an_unusable_pi_names_every_candidate_and_refuses_to_reinstall() {
        let host = FakeHost {
            pi: PiResolution::Unusable(vec![
                unusable("/usr/local/bin/pi", "env: node: No such file or directory"),
                unusable("/usr/bin/pi", "SyntaxError: Unexpected token"),
            ]),
            ..FakeHost::ready()
        };

        let result = decide(&host, Surface::Founder);

        assert_eq!(result.code, PreflightCode::PiUnusable, "not PiMissing: the remedies differ");
        assert!(!result.ok());
        // EVERY candidate, so the operator knows which file to fix or remove.
        assert!(result.message.contains("/usr/local/bin/pi"), "{}", result.message);
        assert!(result.message.contains("/usr/bin/pi"), "{}", result.message);
        // THE REASON EACH CANDIDATE GAVE, which is what separates "install Pi"
        // from "install node" — different operator actions, one symptom. Both
        // were live on the same box in one afternoon.
        assert!(
            result.message.contains("env: node: No such file or directory"),
            "the refusal must quote what the candidate actually said; got {}",
            result.message
        );
        assert!(result.message.contains("SyntaxError: Unexpected token"), "{}", result.message);
        // The likely causes, named, because they are the two that took a box down.
        assert!(result.message.contains("#!/usr/bin/env node"), "{}", result.message);
        // THE NODE FLOOR, NAMED AND DATED. An operator told only "install node"
        // has to go and find which node; the refusal says which, and says when
        // it was measured. It is prose, not a check — nothing in this file
        // reads or compares a node version, and a floor in code would be the
        // version gate this product deliberately does not have.
        assert!(result.message.contains("node 22.19 or later"), "{}", result.message);
        assert!(result.message.contains("2026-08-24"), "{}", result.message);
        assert!(result.message.contains("18.20.4"), "{}", result.message);
        // AND IT MUST NOT SEND THEM TO THE INSTALLER. `require` gates the
        // install on `PiMissing`, so this code is what keeps `curl … | sh` from
        // running on every invocation — the refusal says so out loud too,
        // because an operator who reaches for it anyway has wasted their time.
        assert!(
            !result.message.contains(super::PI_INSTALL_COMMAND),
            "an install cannot fix a shadow; got {}",
            result.message
        );
        assert!(result.message.contains("shadowed"), "{}", result.message);
        assert!(result.message.contains("chief"), "{}", result.message);
    }

    /// THE TWO PI CODES ARE DISTINCT VALUES, because `require` branches on
    /// exactly this to decide whether the installer runs at all.
    #[test]
    fn absent_and_unusable_are_not_the_same_code() {
        assert_ne!(PreflightCode::PiMissing, PreflightCode::PiUnusable);
        assert_eq!(
            decide(&FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() }, Surface::Founder)
                .code,
            PreflightCode::PiMissing
        );
        assert_eq!(
            decide(
                &FakeHost {
                    pi: PiResolution::Unusable(vec![unusable("/usr/bin/pi", "exited 1")]),
                    ..FakeHost::ready()
                },
                Surface::Founder
            )
            .code,
            PreflightCode::PiUnusable
        );
    }

    /// THE INSTALL IS GATED ON `PiMissing` AND NOTHING ELSE — asserted at the
    /// source, because driving `require` would run a real installer.
    ///
    /// A source assertion is the honest instrument here: the alternative is a
    /// test that shells out to `curl`. What it pins is the one line that
    /// decides, and the surrounding tests pin that the two codes really are
    /// produced by the two different worlds.
    #[test]
    fn the_installer_runs_only_for_an_absent_pi() {
        let source = include_str!("preflight.rs");
        let body = source
            .split("fn require(surface: Surface)")
            .nth(1)
            .expect("require is defined in this file")
            .split("\n}\n")
            .next()
            .expect("the body of require");
        assert!(
            body.contains(
                "result.code == PreflightCode::PiMissing && surface != Surface::ResidentActuator"
            ),
            "the install gate must name PiMissing and exclude the resident actuator"
        );
        assert!(
            !body.contains("PreflightCode::PiUnusable"),
            "require must not branch the installer on an unusable Pi; the refusal owns that case"
        );
        // NON-VACUITY: the body was captured and still contains the install.
        assert!(body.contains("install_pi()"), "the require body was captured");
    }

    /// NO VERSION GATE, ANYWHERE. Operator ruling: *"what do you mean it's too
    /// old? just use the installed pi system."*
    ///
    /// A source assertion, because a version gate is defined by its ABSENCE and
    /// there is no state to drive it from. The decision reads exactly one fact
    /// about Pi — whether a runtime resolved — and a reader that compared
    /// versions, minimums or a pinned constant would have to say so here.
    #[test]
    fn the_decision_asks_nothing_about_which_version_of_pi_this_is() {
        let source = include_str!("preflight.rs");
        let decision = source
            .split("pub(crate) fn decide(")
            .nth(1)
            .expect("decide is defined in this file")
            .split("\n}\n")
            .next()
            .expect("the body of decide");
        // NO EXCLUSION HERE, DELIBERATELY. A draft of this test stripped the
        // literal `--version` before applying the ban, because an earlier
        // wording of the unusable-Pi refusal named the probe flag. The shipped
        // wording does not — it says "the message beside each path is what that
        // candidate actually said" — so the exclusion stripped nothing and its
        // comment described a sentence that no longer existed. An unused
        // exception on a banned-word list is where a future editor learns the
        // wrong rule, so it is deleted rather than kept "just in case": if a
        // refusal ever does name the flag, this ban fires, and whoever is
        // holding it then can add the exclusion with a live reason.
        let code: String = decision
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<&str>>()
            .join("\n");
        for banned in ["version", "PINNED_PI", "minimum", "semver"] {
            assert!(
                !code.contains(banned),
                "the preflight decision must not reason about Pi versions, found '{banned}'"
            );
        }
        // NON-VACUITY: the body really was captured, and it really does still
        // ask the one question it is allowed to ask.
        assert!(decision.contains("probe.pi_runtime()"), "the decision body was captured");
    }

    /// The absolute host stays cleared.
    #[test]
    fn an_absolute_pi_runtime_still_passes_every_surface() {
        for surface in
            [Surface::Founder, Surface::Attach, Surface::Reset, Surface::ResidentActuator]
        {
            assert_eq!(decide(&FakeHost::ready(), surface).code, PreflightCode::Ready);
        }
    }

    /// The subsumption property `67abb6d2f` pinned must survive the new gate:
    /// attach clears the operator surface and then starts an actuator, so a
    /// host the first call cleared can never be refused by the second.
    #[test]
    fn the_new_gate_keeps_the_operator_surface_subsuming_the_actuator_surface() {
        let hosts = [
            FakeHost::ready(),
            FakeHost { pi: PiResolution::Absent, ..FakeHost::ready() },
            FakeHost { released: false, ..FakeHost::ready() },
        ];
        for host in hosts {
            for surface in [Surface::Founder, Surface::Attach, Surface::Reset] {
                if decide(&host, surface).ok() {
                    assert!(decide(&host, Surface::ResidentActuator).ok(), "{surface:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod resolver_tests {
    use std::path::{Path, PathBuf};

    use super::{candidates_on_path, PiResolution};

    /// The walk returns LOCATIONS, which is the whole point: `Command::new`
    /// does this lookup already and throws the answer away, leaving every later
    /// process to redo it against its own PATH.
    #[test]
    fn the_path_walk_answers_with_absolute_locations() {
        let executable = |path: &Path| path == Path::new("/opt/harnesses/bin/pi");

        let found = candidates_on_path("pi", "/usr/bin:/opt/harnesses/bin:/bin", &executable);

        assert_eq!(found, vec![PathBuf::from("/opt/harnesses/bin/pi")]);
        assert!(found.iter().all(|candidate| candidate.is_absolute()));
    }

    /// EVERY executable entry, in PATH order — not the first.
    ///
    /// It returned the first, and that is the wrong stopping rule when a
    /// candidate can be executable and dead. The order is still the shell's
    /// order, so the caller can keep the shell's answer when it works and go on
    /// when it does not.
    #[test]
    fn every_executable_entry_is_returned_in_path_order() {
        let executable =
            |path: &Path| path == Path::new("/first/pi") || path == Path::new("/second/pi");

        assert_eq!(
            candidates_on_path("pi", "/first:/second", &executable),
            vec![PathBuf::from("/first/pi"), PathBuf::from("/second/pi")]
        );
    }

    /// A present-but-not-executable file is not a runtime.
    #[test]
    fn a_non_executable_candidate_is_skipped() {
        let executable = |path: &Path| path == Path::new("/late/pi");

        assert_eq!(
            candidates_on_path("pi", "/early:/late", &executable),
            vec![PathBuf::from("/late/pi")]
        );
    }

    /// An empty PATH entry means the current directory to a shell. It must
    /// mean NOTHING here: a Pi resolved relative to whatever directory chiefd
    /// happened to be started in is precisely the value that cannot be handed
    /// to another process.
    #[test]
    fn empty_path_entries_never_resolve_to_the_current_directory() {
        let executable = |_: &Path| true;

        assert!(candidates_on_path("pi", "::", &executable).is_empty());
        assert_eq!(
            candidates_on_path("pi", ":/usr/bin:", &executable),
            vec![PathBuf::from("/usr/bin/pi")]
        );
    }

    /// A relative PATH entry cannot produce a portable answer either.
    #[test]
    fn a_relative_path_entry_is_never_accepted() {
        let executable = |_: &Path| true;

        assert!(candidates_on_path("pi", "node_modules/.bin", &executable).is_empty());
    }

    /// Nothing on PATH is an empty list, which the caller turns into `Absent`.
    #[test]
    fn no_candidate_anywhere_is_empty() {
        assert!(candidates_on_path("pi", "/usr/bin:/bin", &|_: &Path| false).is_empty());
    }

    /// **A BROKEN PI THAT SHADOWS A GOOD ONE MUST NOT WIN.**
    ///
    /// Measured on a live box: `/usr/local/bin/pi` is a real 0.84.3
    /// entry point beginning `#!/usr/bin/env node`, on a box with no node
    /// anywhere. Executable, first on PATH, dead. The first cut of this
    /// resolver stopped at the first EXECUTABLE candidate and gated the whole
    /// answer on that one probe, so a working Pi further along was unreachable
    /// and the caller reported "no Pi at all".
    #[test]
    fn a_candidate_that_does_not_answer_is_stepped_over_for_one_that_does() {
        let good = Path::new("/home/operator/.local/bin/pi");
        // The shadow fails the way the real one did: a node shebang with no node.
        let answers = |path: &Path| {
            if path == good {
                Ok(())
            } else {
                Err("env: node: No such file or directory".to_string())
            }
        };

        assert_eq!(
            super::resolve_pi_runtime(
                Some("/usr/local/bin:/home/operator/.local/bin"),
                &|_| true,
                &answers
            ),
            PiResolution::Resolved(good.display().to_string()),
            "the shadowing candidate is stepped over, not fatal"
        );
    }

    /// **UNUSABLE IS NOT ABSENT, and the list is every candidate that failed.**
    ///
    /// The two have opposite remedies — absent is installed, unusable is made
    /// WORSE by installing, because the new binary lands behind the shadow — so
    /// they must not collapse into one answer. The names travel because the
    /// refusal has to say which files it tried.
    #[test]
    fn executable_candidates_that_all_fail_are_unusable_and_named() {
        assert_eq!(
            super::resolve_pi_runtime(Some("/usr/local/bin:/usr/bin"), &|_| true, &|_| {
                Err("env: node: No such file or directory".to_string())
            }),
            PiResolution::Unusable(vec![
                super::UnusablePi {
                    path: "/usr/local/bin/pi".to_string(),
                    reason: "env: node: No such file or directory".to_string()
                },
                super::UnusablePi {
                    path: "/usr/bin/pi".to_string(),
                    reason: "env: node: No such file or directory".to_string()
                }
            ]),
            "every candidate tried is reported, in PATH order"
        );
        assert_eq!(
            super::resolve_pi_runtime(Some("/usr/bin"), &|_| false, &|_| Ok(())),
            PiResolution::Absent,
            "nothing executable anywhere is ABSENT, which is the installable case"
        );
    }

    /// THE LADDER HAS ONE RUNG, AND IT CAN ONLY ANSWER WITH AN ABSOLUTE PATH.
    ///
    /// The absolute property is asserted HERE, of the resolver, rather than at
    /// the gate that used to catch it downstream. That is the whole reason
    /// `PiNotAbsolute` could be retired: a guarantee made where the value is
    /// produced needs no check where it is consumed.
    ///
    /// TOMBSTONES, all pin-and-checkout tests:
    /// `the_checkout_pi_is_found_when_nothing_is_pinned_and_path_has_none`,
    /// `the_pi_ladder_is_pin_then_checkout_then_path_and_never_another_order`
    /// and `the_checkout_rung_is_founder_pis_own_function`. They pinned the
    /// ORDER of three rungs; there is one rung, so the order is not a fact any
    /// more and asserting it would be asserting nothing.
    #[test]
    fn the_ladder_can_only_answer_with_an_absolute_path() {
        let on_path = PathBuf::from("/usr/bin/pi");
        let executable = |candidate: &Path| candidate == on_path;
        let answers =
            |candidate: &Path| if candidate == on_path { Ok(()) } else { Err("no".to_string()) };

        let resolved = super::resolve_pi_runtime(Some("/usr/bin"), &executable, &answers);
        assert_eq!(resolved, PiResolution::Resolved(on_path.display().to_string()));
        let PiResolution::Resolved(path) = resolved else { panic!("resolved") };
        assert!(
            Path::new(&path).is_absolute(),
            "every answer is absolute, which is what lets the not-absolute gate be deleted"
        );

        // A RELATIVE PATH ENTRY CANNOT SMUGGLE ONE THROUGH. This is the one way
        // a caller could still have supplied a relative string, and
        // `candidates_on_path` drops it rather than joining it — so it is not
        // even an UNUSABLE candidate, it is nothing.
        assert_eq!(
            super::resolve_pi_runtime(Some("node_modules/.bin"), &|_| true, &|_| Ok(())),
            PiResolution::Absent,
            "a relative PATH entry is not a location a pane could use"
        );

        // No PATH at all is ABSENT, which becomes the install.
        assert_eq!(super::resolve_pi_runtime(None, &|_| true, &|_| Ok(())), PiResolution::Absent);
    }

    /// NO PIN IS READ, AND NO CHECKOUT. A source assertion, because both are
    /// defined by absence: the resolver's whole body is the `PATH` walk, and a
    /// re-grown rung would have to appear in it.
    #[test]
    fn the_resolver_reads_no_pin_and_no_checkout() {
        let source = include_str!("preflight.rs");
        let body = source
            .split("pub(crate) fn resolve_pi_runtime(")
            .nth(1)
            .expect("the resolver is defined in this file")
            .split("\n}\n")
            .next()
            .expect("the resolver body");
        for banned in ["TEAM_LAUNCHER_PI", "founder_pi", "node_modules", "launcher_root"] {
            assert!(!body.contains(banned), "the resolver must not name '{banned}'");
        }
        // NON-VACUITY: the body was captured and still does the one thing it is
        // for.
        assert!(body.contains("candidates_on_path(\"pi\""), "the resolver body was captured");
    }

    /// A Pi that is PRESENT and cannot run is not a Pi. Presence on disk is not
    /// the same fact as "this binary works here" — Pi's CLI starts with
    /// `#!/usr/bin/env node`, so a box with bun and no node has the file and
    /// can execute none of them.
    #[test]
    fn a_candidate_that_does_not_answer_its_version_flag_is_not_the_answer() {
        assert_eq!(
            super::resolve_pi_runtime(
                Some("/usr/bin"),
                &|_| true,
                &|_| Err("exited 1".to_string())
            ),
            PiResolution::Unusable(vec![super::UnusablePi {
                path: "/usr/bin/pi".to_string(),
                reason: "exited 1".to_string()
            }]),
            "an executable that answers no version is not adopted -- and is not ABSENT either"
        );
    }

    // TOMBSTONE: `the_not_absolute_code_is_its_own_case`. The variant it
    // distinguished is deleted, so distinguishing it is not a property.
}

/// The clearance carried across the tmux re-exec.
///
/// Every test here drives the pure seam — [`Clearance::honoured`] and
/// [`Clearance::covers_pi`] take the `PATH` and the clock as parameters — so
/// none of them mutates process state, and the conditions are stated rather
/// than arranged.
#[cfg(test)]
mod clearance_tests {
    use std::cell::{Cell, OnceCell};
    use std::path::{Path, PathBuf};

    use super::{
        decide, path_digest, resolve_pi_runtime, Clearance, HostProbe, PreflightCode, RealHost,
        Surface, CLEARANCE_ENV, CLEARANCE_MAX_AGE, CLEARANCE_SEPARATOR,
    };

    /// The operator's `PATH` on the host the proof was made on.
    const OUTER_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
    /// A tmux server's `PATH`, started from a login shell that never saw the
    /// operator's additions. This is the difference the whole design turns on.
    const INNER_PATH: &str = "/usr/bin:/bin";
    /// An arbitrary "now", far from any epoch boundary.
    const NOW_MS: u128 = 1_760_000_000_000;

    fn proof(pi: &str) -> Clearance {
        Clearance { minted_at_ms: NOW_MS, path_digest: path_digest(OUTER_PATH), pi: pi.to_string() }
    }

    /// The round trip a clearance actually makes: minted here, rendered into
    /// one environment pair, read back by the process tmux respawns.
    #[test]
    fn a_clearance_survives_the_environment_it_travels_through() {
        let minted = proof("/opt/homebrew/bin/pi");
        let (name, value) = minted.forwarded();

        assert_eq!(name, CLEARANCE_ENV);
        assert_eq!(Clearance::parse(&value), Some(minted.clone()));
        assert!(
            minted.honoured(Some(OUTER_PATH), NOW_MS + 500),
            "the inner process runs under the same PATH, half a second later"
        );
    }

    /// THE INVARIANCE CONDITION, enforced rather than assumed.
    ///
    /// The whole design rests on one claim: `pi --version` and `tmux -V` answer
    /// the same in both processes BECAUSE `PATH` is the same. When it is not,
    /// the claim does not hold and the clearance must buy nothing — Pi's CLI
    /// starts with `#!/usr/bin/env node`, so even an absolute Pi is executed
    /// through an interpreter found on the `PATH` of whichever process runs it.
    #[test]
    fn a_clearance_is_refused_under_a_different_path() {
        let minted = proof("/opt/homebrew/bin/pi");

        assert!(minted.honoured(Some(OUTER_PATH), NOW_MS));
        assert!(
            !minted.honoured(Some(INNER_PATH), NOW_MS),
            "a tmux server with its own PATH must re-probe, not inherit a verdict"
        );
        assert!(!minted.honoured(None, NOW_MS), "a process with no PATH proves nothing");
    }

    /// A clearance is a statement about a host a moment ago, never a standing
    /// permission.
    #[test]
    fn a_clearance_outside_the_freshness_bound_is_refused() {
        let minted = proof("/opt/homebrew/bin/pi");
        let max_age = CLEARANCE_MAX_AGE.as_millis();

        assert!(
            minted.honoured(Some(OUTER_PATH), NOW_MS + max_age),
            "the bound itself is honoured"
        );
        assert!(
            !minted.honoured(Some(OUTER_PATH), NOW_MS + max_age + 1),
            "one millisecond past the bound is past the bound"
        );
        assert!(
            !minted.honoured(Some(OUTER_PATH), NOW_MS - 1),
            "a clearance stamped in the future means the two clocks disagree, so the age this \
             bound reasons about is not a quantity either process knows"
        );
    }

    /// A clearance proves that ONE binary ran. It says nothing about a
    /// different candidate this process's own ladder resolved — and the case
    /// where the two processes disagree about which Pi to run is exactly the
    /// case worth catching.
    #[test]
    fn a_clearance_covers_only_the_runtime_it_names() {
        let minted = proof("/opt/homebrew/bin/pi");

        assert!(minted.covers_pi(Path::new("/opt/homebrew/bin/pi")));
        assert!(!minted.covers_pi(Path::new("/usr/local/bin/pi")));
        assert!(
            !minted.covers_pi(Path::new("/opt/homebrew/bin/pi/../pi")),
            "an unnormalised path that happens to resolve the same is not the same claim"
        );
    }

    /// Fail closed on anything that cannot be read WHOLE. A clearance that is
    /// half-read is not a proof of half a thing; it is not a proof.
    #[test]
    fn a_malformed_clearance_is_refused_rather_than_half_read() {
        let sep = CLEARANCE_SEPARATOR;
        let digest = path_digest(OUTER_PATH);
        for broken in [
            String::new(),
            "not-a-number".to_string(),
            format!("{NOW_MS}"),
            format!("{NOW_MS}{sep}{digest}"),
            format!("{NOW_MS}{sep}{sep}/opt/pi"),
            format!("{NOW_MS}{sep}{digest}{sep}"),
            format!("{NOW_MS}{sep}{digest}{sep}/opt/pi{sep}extra"),
            format!("-1{sep}{digest}{sep}/opt/pi"),
        ] {
            assert_eq!(Clearance::parse(&broken), None, "must refuse {broken:?}");
        }
    }

    /// THE SAVING, measured at the seam it is made at.
    ///
    /// `pi --version` is 530 ms on the measured host, and it is the whole
    /// reason this packet exists. The ladder still runs in full either way —
    /// the same `PATH` walk — and only the subprocess is skipped.
    #[test]
    fn the_version_probe_runs_once_when_cleared_and_every_time_when_not() {
        let pi = PathBuf::from("/usr/bin/pi");
        let minted = proof(&pi.display().to_string());
        let on_path = |candidate: &Path| candidate == pi;

        // `Cell`, not a `mut` capture: `resolve_pi_runtime` takes `&dyn Fn`,
        // which is the right signature for a probe and the reason the count
        // needs interior mutability here.
        let probes = Cell::new(0_u32);
        let resolved = resolve_pi_runtime(Some("/usr/bin"), &on_path, &|_| {
            probes.set(probes.get() + 1);
            Ok(())
        });
        assert_eq!(resolved, super::PiResolution::Resolved(pi.display().to_string()));
        assert_eq!(probes.get(), 1, "with no clearance the probe is paid for");

        let cleared_probes = Cell::new(0_u32);
        let resolved_cleared =
            resolve_pi_runtime(Some("/usr/bin"), &on_path, &|candidate: &Path| {
                if minted.covers_pi(candidate) {
                    return Ok(());
                }
                cleared_probes.set(cleared_probes.get() + 1);
                Ok(())
            });
        assert_eq!(
            resolved_cleared,
            super::PiResolution::Resolved(pi.display().to_string()),
            "the same answer, either way"
        );
        assert_eq!(cleared_probes.get(), 0, "a covered candidate costs no subprocess");
    }

    /// `has_command` is the other subprocess, and a clearance exists only for a
    /// decision that already passed this gate — so holding one IS the answer.
    /// Asserted through the real probe type, which would otherwise run
    /// `tmux -V`.
    #[test]
    fn a_clearance_answers_the_installed_check_without_a_subprocess() {
        let cleared =
            RealHost { clearance: Some(proof("/opt/homebrew/bin/pi")), pi: OnceCell::new() };

        assert!(cleared.has_command(super::TMUX_PROGRAM), "the program the parent proved");
        assert!(
            !cleared.has_command("a-program-no-host-has"),
            "a clearance is a statement about the programs the parent probed; anything else is \
             still a real question, asked of the real host"
        );
    }

    /// THE GATES A CLEARANCE MUST NEVER REACH.
    ///
    /// `$TMUX` being set and the tmux server answering are the two gates whose
    /// answers genuinely differ across the re-exec, and they are the two the
    /// inner process needs most. Driven with `has_command` already true — the
    /// most a clearance can ever do — to show the refusal is decided elsewhere.
    #[test]
    fn a_clearance_cannot_reach_the_gates_that_differ_across_the_re_exec() {
        struct ClearedButUnreachable;
        impl HostProbe for ClearedButUnreachable {
            fn has_command(&self, _: &str) -> bool {
                true
            }
            fn env(&self, name: &str) -> Option<String> {
                (name == "TMUX").then(|| "/tmp/tmux-0/default,1,0".to_string())
            }
            fn tmux_reachable(&self) -> bool {
                false
            }
            fn pi_runtime(&self) -> super::PiResolution {
                super::PiResolution::Resolved("/opt/homebrew/bin/pi".to_string())
            }
            fn released(&self) -> bool {
                true
            }
        }

        assert_eq!(
            decide(&ClearedButUnreachable, Surface::Founder).code,
            PreflightCode::TmuxUnreachable,
            "the re-exec'd founder is the process that reads $TMUX for real; a parent that had no \
             tmux client cannot have proved this"
        );
    }

    /// The release stamp is a `stat`, and it is read in the process that will
    /// act on it. Nothing about a clearance touches it.
    #[test]
    fn a_clearance_cannot_clear_the_release_stamp() {
        struct ClearedButUnreleased;
        impl HostProbe for ClearedButUnreleased {
            fn has_command(&self, _: &str) -> bool {
                true
            }
            fn env(&self, _: &str) -> Option<String> {
                None
            }
            fn tmux_reachable(&self) -> bool {
                true
            }
            fn pi_runtime(&self) -> super::PiResolution {
                super::PiResolution::Resolved("/opt/homebrew/bin/pi".to_string())
            }
            fn released(&self) -> bool {
                false
            }
        }

        assert_eq!(
            decide(&ClearedButUnreleased, Surface::Founder).code,
            PreflightCode::NotReleased
        );
    }

    /// Only the verb that re-execs may honour a proof. `attach`, `reset` and
    /// the resident actuator are started by tmux or by an operator, never by a
    /// chiefd process that has just cleared this host — so a clearance in their
    /// environment belongs to somebody else and is not read.
    ///
    /// The actuator matters most: its preflight is the one whose ABSENCE once
    /// killed every pane it minted, once per second, forever.
    #[test]
    fn only_the_re_execing_verb_honours_a_clearance() {
        assert!(super::honours_clearance(Surface::Founder));
        for surface in [Surface::Attach, Surface::Reset, Surface::ResidentActuator] {
            assert!(!super::honours_clearance(surface), "{surface:?} must probe for itself");
        }
    }

    /// The clearance reaches ONE child. `PANE_ENVIRONMENT` is forwarded to
    /// every pane tmux mints for an agent and to the actuator; a clearance
    /// there would outlive the launch that earned it.
    #[test]
    fn the_clearance_is_not_in_the_pane_environment_allowlist() {
        assert!(
            !crate::tmux::PANE_ENVIRONMENT.contains(&CLEARANCE_ENV),
            "a clearance must never be inherited by an agent pane, a daemon, or the actuator"
        );
    }

    // ── The Pi floor ───────────────────────────────────────────────────────
    // A FLOOR, WARNED ABOUT HERE AND ENFORCED ONLY BY `chief upgrade`. These
    // three tests are the whole rule: below says something, at-or-above says
    // nothing, unreadable says nothing. The third is the one worth having —
    // `pi --version` is a string Pi is free to change, and a warning that fires
    // on a banner change trains an operator to ignore the line.

    #[test]
    fn a_pi_below_the_floor_produces_a_warning_that_names_both_numbers() {
        let message = super::pi_floor_warning(Some("0.79.0")).expect("below the floor must warn");
        assert!(message.contains("0.79.0"), "the warning must name what is installed: {message}");
        assert!(
            message.contains(host_primitives::pi_floor::MINIMUM_PI_VERSION),
            "the warning must name the floor: {message}"
        );
        assert!(
            message.contains("chief upgrade"),
            "a warning that names no remedy is noise: {message}"
        );
    }

    #[test]
    fn the_exact_floor_and_anything_above_it_warn_about_nothing() {
        assert_eq!(
            super::pi_floor_warning(Some(host_primitives::pi_floor::MINIMUM_PI_VERSION)),
            None,
            "the floor is a minimum, not a pin — meeting it exactly is passing"
        );
        assert_eq!(super::pi_floor_warning(Some("99.0.0")), None);
    }

    #[test]
    fn an_unreadable_pi_version_is_unknown_and_never_warned_about() {
        assert_eq!(super::pi_floor_warning(None), None);
        assert_eq!(super::pi_floor_warning(Some("unreported")), None);
        assert_eq!(super::pi_floor_warning(Some("")), None);
    }
}
