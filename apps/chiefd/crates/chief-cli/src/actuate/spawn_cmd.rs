//! The single place a `SpawnSpec` becomes an actual pane launch command.
//!
//! The design (Q1) is explicit: isolate *"`SpawnSpec` → actual launch command
//! line (env, pi-home, cwd)"* in ONE function and conformance-test it
//! against fixtures.
//!
//! # The current boot contract (SA-2/SA-3)
//!
//! [`launch_command`] is a faithful port of `organizationPersonPiCommand`
//! (`src/organization/org-runtime.ts:372-427`) as it stands after the person
//! boot-contract cutover:
//!
//! * **The argv is a `/usr/bin/env` invocation.** The pane's environment
//!   travels as `NAME=value` assignments in argv (`env -C <workspace>
//!   PI_CODING_AGENT_SESSION_DIR=… pi …`), so
//!   the effective non-secret contract is auditable from the pane command
//!   itself. The retired `provider.env` shell prelude is gone, #748 removed
//!   the launcher provider-concurrency bound, and the credential channel that
//!   replaced them is gone too: chiefd holds no credential at any point, so
//!   there is nothing to keep out of argv (invariant 32 now holds by
//!   construction rather than by discipline).
//! * **`--tools` plus the fixed organization extensions.** Pi discovers the
//!   company skills through the agent home's symlink. Chief passes the exact
//!   three shipped organization extensions by path, because loading the whole
//!   checkout directory would also load the Founder extension. Per-person
//!   `--skill` / `--prompt-template` arguments and discovery suppressors are
//!   gone with the retired `resources.json`.
//! * **No `--system-prompt`.** The person contract reaches Pi as its standard
//!   `workspace/AGENTS.md` project context file — the only on-disk projection
//!   of the SQL `person-contracts` authority, MD5-verified and refreshed from
//!   SQL by the TypeScript boot path. The retired `role.md` fan-out went with
//!   it.
//! * **Nothing here is derived from operator-supplied DATA.** Every word is an
//!   id or a path. `ORG_CUSTOM_PROVIDERS` was the one exception — a whole
//!   provider registry projected onto argv — and it is deleted with provider
//!   management. [`MAX_PANE_ARGV_BYTES`] survives it: the bound exists because
//!   an argv that can grow without anybody choosing to grow it is the failure,
//!   and a future field could be one again.
//! * **No appearance argument at all.** `--no-themes` and the generated
//!   `--theme <file>` trio are deleted with the themes chief used to write into
//!   each home. A pane inherits the operator's own Pi appearance, the same way
//!   it inherits their route. It is TOLD the terminal's colour facts, though —
//!   `COLORTERM` and `COLORFGBG` — because a daemon-launched pane inherits the
//!   daemon's environment and not a terminal's, so those two are stale or
//!   missing rather than absent-and-harmless. Stating a fact about the screen
//!   is not choosing an appearance: Pi still decides what to do with it.
//!
//! Two properties are load-bearing and are what the fixtures pin:
//!
//! * **A resumed session is told NOTHING.** Under desired-state-only there are
//!   no graceful handoffs: a person whose launch hash drifts is killed and
//!   relaunched against their existing transcript, and Pi restores that
//!   transcript. chiefd used to publish a sentence per cause explaining why,
//!   and this module selected one by the cause it was acting on. Operator
//!   ruling: *"don't insert anything ever to anything. just boot the agent."*
//!   The pane is handed its session and the argv ENDS there — anything after
//!   it is a positional prompt, which is what the deleted feature was.
//! * **No credential ever reaches argv (invariant 32).** Every value inlined
//!   here is non-secret by construction: pane identity markers and placement.

use std::path::{Path, PathBuf};

use crate::actuate::plan::SpawnSpec;
use crate::appearance::Appearance;

/// Where this actuator is drawing the pane: the tmux server it is talking to,
/// and the session the pane lands in.
///
/// A borrowed pair rather than two loose arguments so the two can never be
/// passed in the wrong order, and so a caller cannot supply one without the
/// other — the pane-env contract requires both or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanePlacement<'a> {
    /// The tmux socket name (`tmux -L <socket>`).
    pub socket: &'a str,
    /// The tmux session the pane is created in.
    pub session: &'a str,
}

/// The per-person, host-resolved launch inputs the planner deliberately omits
/// from [`SpawnSpec`] (which carries only person and launch hash). Built by
/// the caller ([`crate::actuate::interpret`]) from
/// the person's manifest record plus their already-materialized pi-home
/// discovery directories (`skills/`, `sessions/`) and the exact shipped
/// extension paths chiefd resolved from the launcher checkout.
///
/// Every path here is read back from what materialization already wrote and
/// already validated — this struct trusts that content rather than
/// re-deriving or re-validating it (that re-validation is
/// `validateResources`'s job in the TypeScript CLI, not the actuator's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The pinned pi binary to exec in the pane.
    pub pi_binary: PathBuf,
    /// The non-Chief person's home: their cwd and their session store. Config
    /// does NOT live under here — Pi inherits the operator's own agent dir,
    /// never on the command line.
    pub pi_home: PathBuf,
    /// The person's workspace, used as the pane's working directory.
    pub workspace: PathBuf,
    /// `<organization name> · <person title>`, passed as `--name`.
    pub display_name: String,
    /// The person's display name, for the fresh-session initial message
    /// (`"You are <name> (<id>) at work in your company"`).
    pub person_name: String,
    /// This person's identity accent, `#rrggbb`, as chiefd allocated it.
    ///
    /// The one field here that is NOT an argv word: it is a display fact the
    /// rail fills the role chip with. It rides on the launch catalog because
    /// that is the pass on which chiefd already publishes it, and because the
    /// accent it replaced arrived the same way — as the generated theme files
    /// the rail opened and parsed to recover this exact hex.
    pub accent: Option<String>,
    /// Granted tool names, joined with `,` for `--tools` — the only capability
    /// argument; skills are discovered from the agent home.
    pub tools: Vec<String>,
    /// Exact shipped extension source paths, emitted as repeated
    /// `--extension <path>` pairs.
    pub extensions: Vec<PathBuf>,
    /// The transcript to resume (`--session <path>`), or `None` to start with
    /// the fresh-session initial message instead.
    pub session: Option<PathBuf>,
    /// Whether a message is waiting unread in this person's mailbox, as chiefd
    /// read it on the pass that published this catalog.
    ///
    /// The one durable claim on a person's attention that outlives their Pi
    /// session, and therefore the whole of "does this person have assigned
    /// work" — goals were deleted in #1047 and nothing replaced them. It is
    /// chiefd's mailbox and chiefd's answer; this client only selects a
    /// sentence from it ([`BootStanding::from_company`]) and never re-derives
    /// it, exactly as it never re-derives whether a transcript exists.
    pub pending_mail: bool,
    /// Non-secret pane identity environment (organization/person markers,
    /// socket/session, launcher roots, `PI_CODING_AGENT_SESSION_DIR`), emitted as argv
    /// env assignments in order. Must never carry a credential.
    pub env: Vec<(String, String)>,
}

/// A fully-resolved pane launch: the argv to run, the directory to run it in,
/// and any extra non-secret environment to set on the pane via tmux `-e`.
///
/// Under the current contract `env` is empty for pane launches: every pane
/// variable travels as an argv env assignment inside the `/usr/bin/env`
/// invocation, mirroring `organizationPersonPiCommand`. The field remains so
/// the tmux spawn seam (`interpret::push_launch_flags`) keeps one channel for
/// genuinely tmux-level variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommand {
    /// The command argv tmux runs after `--`.
    pub argv: Vec<String>,
    /// The pane's working directory (`new-window -c`).
    pub cwd: PathBuf,
    /// Extra non-secret pane environment (`new-window -e NAME=value`).
    pub env: Vec<(String, String)>,
}

/// The `$0` handed to the final-pane startup shell. This wrapper runs inside
/// the newly minted tmux pane, before Pi can sanitize `TMUX_PANE`.
const PANE_STARTUP_WRAPPER_ARG0: &str = "chiefd-pane-startup";

/// Capture tmux's pane identifier before Pi starts. The shell never parses
/// caller-controlled argv: `"$@"` forwards every Pi argument exactly once.
/// The preserved namespaced value is validated again by the intercom runtime,
/// then copied back to the fenced child as raw `TMUX_PANE` for strict ancestry
/// authentication.
const PANE_STARTUP_SCRIPT: &str =
    // Scrub PATH first: the pane inherits the tmux server's PATH, and a
    // broken foreign entry there (an unreadable dir returning EBADMSG on
    // lookup) aborts resolution for every PATH-relative executable — `env`,
    // even the interpreter in Pi's own shebang — killing the pane silently.
    // Standard dirs go first; the inherited PATH stays appended so operator
    // additions still resolve. The wrapper itself is absolute (`/bin/sh`).
    "export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin${PATH:+:$PATH}; pane=${TMUX_PANE:?missing tmux pane}; case \"$pane\" in %[0-9]*) ;; *) exit 125 ;; esac; person=$1; shift; printf '\\033[2J\\033[H%s is starting…\\n' \"$person\"; exec /usr/bin/env ORG_LAUNCHER_PANE_ID=\"$pane\" \"$@\"";

/// The ceiling a pane argv must stay under, in bytes, summed over every word.
///
/// This is not an OS limit. `interpret::create_session` packs `start-server`,
/// six input-configuration commands, `new-session`, the whole pane argv and
/// seven `set-option` identity tags into ONE tmux client message ("§2.0(2) ONE
/// SHOT" — the property that makes session minting crash-atomic). The tmux
/// client transmits that as a single `MSG_COMMAND`, and the server caps it.
/// **Measured on tmux 3.3a: 16000 bytes accepted, 18000 rejected with
/// `command too long`.** 12 KiB leaves the tags, the flags and a long
/// workspace path comfortable room under the smaller of those.
///
/// The bound exists because exceeding it is not a degraded pane — it is no
/// pane. `new-session` fails outright, the actuator retries forever and trips
/// its circuit breaker to shadow, and the company never boots. That is what a
/// registry declaring 337 models did: `ORG_CUSTOM_PROVIDERS` alone measured
/// 50213 bytes, three times the whole message's ceiling.
///
/// Only a value derived from operator-sized data can approach this; every
/// other word is a path, an id or a flag, and
/// `a_pane_argv_stays_far_under_the_tmux_message_ceiling_for_a_large_catalog`
/// is what fails if anything ever unbounds it again.
pub const MAX_PANE_ARGV_BYTES: usize = 12 * 1024;

/// Total bytes of a pane argv as tmux will carry it: every word, plus one
/// separator each.
#[must_use]
pub fn pane_argv_bytes(argv: &[String]) -> usize {
    argv.iter().map(|word| word.len() + 1).sum()
}

/// Build the concrete pane launch command for one admitted spawn.
///
/// The base command is the real pi CLI invocation: `--tools <csv>` (the only
/// capability argument), `--name`, followed by either `--session <transcript>`
/// (resume) or the fresh-session initial message. That base is prefixed with
/// `/usr/bin/env -C <workspace>` and the pane's non-secret environment as
/// `NAME=value` assignments.
///
/// TOMBSTONE: `--no-themes` and the generated `--theme <file>` trio, alongside
/// `--provider` / `--model` / `--thinking` / `ORG_CUSTOM_PROVIDERS`. An agent
/// is plain Pi on the operator's own defaults — chief chooses its route and
/// its appearance no more than it chooses its editor. The fixed organization
/// `--extension` paths are launcher code, not a per-person capability choice.
///
/// TOMBSTONE: the admission-delay wrapper. A spawn used to carry a `delay_ms`
/// and a non-zero one wrapped the whole argv in `sh -c 'sleep <secs>; exec
/// "$@"'`. The ramp is deleted by operator ruling and every pane now execs
/// immediately.
///
/// # The founding boot is told to do nothing
///
/// `standing` selects between the two fresh-session messages
/// ([`fresh_session_message`]). It is the caller's fact, derived per pass from
/// the roster and this person's transcript by [`BootStanding::from_company`] —
/// nothing durable records it, deliberately.
///
/// # The pane is told the ground it is drawn on, because nothing else can
/// tell it
///
/// `appearance` is what `/run/tribes-theme` says right now, resolved by the
/// caller through [`crate::appearance::read_declared`] — the same authority,
/// through the same reader, that the rail draws itself from. See the comment
/// at the `COLORFGBG` assignment below for what goes wrong without it. [`None`]
/// means the authority said NOTHING, and then nothing is emitted.
///
/// # AC6: this client states the pane's placement, because it owns it
///
/// `ORG_LAUNCHER_RUNTIME_SOCKET` and `ORG_LAUNCHER_RUNTIME_SESSION` used to
/// arrive inside `launch.env`, published by chiefd from its `ActuatorConfig`.
/// That made the backend assert a placement fact it cannot observe and this
/// client derives independently; they agreed only because this client had
/// handed chiefd the socket at daemon start. They are injected HERE now, from
/// the socket and session this actuator is actually driving, so the pane's
/// environment cannot disagree with the tmux server the pane is on. The
/// contract itself is unchanged: `organization-intercom.ts` requires the pair
/// and refuses to load with only one of them, so both are always written and
/// they are always written together.
#[must_use]
pub fn launch_command(
    spec: &SpawnSpec,
    launch: &LaunchSpec,
    placement: &PanePlacement<'_>,
    standing: BootStanding,
    appearance: Option<Appearance>,
) -> PaneCommand {
    let mut argv = vec!["/usr/bin/env".to_owned(), "-C".to_owned(), path_arg(&launch.workspace)];
    // Pi resolves its colour mode ONCE per process from the environment: under
    // tmux, `getCapabilities().trueColor` is exactly
    // `COLORTERM === "truecolor" || "24bit"`, and a false there puts the entire
    // Theme into 256-colour, quantising every hex through `rgbTo256`. chiefd is
    // a daemon, so `/usr/bin/env` inherited no COLORTERM and every pane it
    // launched resolved 256-colour: a person's identity accent rendered as its
    // nearest palette index (#e5c07b -> 38;5;180) instead of the exact hex tmux
    // sets as the pane header's `@accent`. Measured live: 0 of 72 pi processes
    // had it set. Identity-exactness (#11/#62) is unachievable without this.
    //
    // First, so a catalog-supplied COLORTERM in `launch.env` still wins (later
    // `env` assignments override earlier ones).
    argv.push("COLORTERM=truecolor".to_owned());
    // The same defect as COLORTERM above, one variable over, and with a longer
    // tail: `COLORFGBG` is how a terminal tells its client whether the ground
    // it is drawing on is light or dark, and a daemon-launched pane inherits
    // the DAEMON's copy of it — which was frozen at whatever terminal started
    // chiefd, or is simply absent. Pi's `detectTerminalBackgroundFromEnv` reads
    // that stale hint at startup, and for the Chief pane (the one person with
    // no redirect at all, and since #1307 nobody has one)
    // it PERSISTS the answer into the operator's `~/.pi/agent/settings.json` as
    // a bare explicit `"theme"`. An explicit theme deliberately does not follow
    // the live bridge (DECISIONS.md, 2026-08-17), so a single stale `15;0` at
    // one chief start pins that operator's Chief pane to dark FOREVER, while
    // the rail beside it — which re-reads the bridge on every draw — stays
    // light. Measured exactly that way on a live box: `/run/tribes-theme` said
    // `light`, the rail drew light, the pane drew `#d4d4d4` body text.
    //
    // So the launcher SETS it, from the same authority the rail resolves, and
    // does not merely scrub it: an absent `COLORFGBG` makes Pi fall through to
    // its own `"no terminal background hint found"` default, which is dark, and
    // that default would be persisted just as durably as the stale hint was.
    // Seeding the true value makes Pi's own derivation CORRECT, so whatever it
    // writes down is right the first time.
    //
    // ABSENT IS ABSENT, on the rule this module already follows for a resume
    // prompt: if the bridge said nothing, this launcher knows nothing, and a
    // guess written into a file that never expires is worse than the
    // inheritance it replaced. No authority, no assignment — the pane keeps
    // whatever it would have inherited.
    //
    // Placed with COLORTERM, before `launch.env`, so a catalog-supplied value
    // would still win (later `env` assignments override earlier ones).
    if let Some(appearance) = appearance {
        argv.push(format!("COLORFGBG={}", appearance.colorfgbg()));
    }
    // Before `launch.env`, on the same rule COLORTERM rides: a catalog value
    // still wins if one is ever published again. Nothing publishes one today
    // (`a_pane_carries_this_actuators_own_placement_and_never_a_published_one`
    // asserts the catalog does not), so in practice these ARE the pane's
    // placement.
    argv.push(format!("ORG_LAUNCHER_RUNTIME_SOCKET={}", placement.socket));
    argv.push(format!("ORG_LAUNCHER_RUNTIME_SESSION={}", placement.session));
    for (name, value) in &launch.env {
        argv.push(format!("{name}={value}"));
    }
    // This is the actual tmux execution boundary on host-driven respawns.
    // The `/usr/bin/env` assignments stay in force for the wrapper and Pi;
    // only the launcher-owned pane token is added after validation.
    argv.push("/bin/sh".to_owned());
    argv.push("-c".to_owned());
    argv.push(PANE_STARTUP_SCRIPT.to_owned());
    argv.push(PANE_STARTUP_WRAPPER_ARG0.to_owned());
    argv.push(launch.person_name.clone());
    argv.push(launch.pi_binary.display().to_string());
    // `--approve` IS HOW THE ROLE SKILL AND THE IDENTITY THEME REACH THE
    // PERSON. It reads like a convenience that suppresses a prompt, and it was
    // one until chief stopped redirecting `PI_CODING_AGENT_DIR`. It is now
    // load-bearing, and its failure mode is SILENT.
    //
    // The chain, so nobody has to rediscover it: `--approve` sets
    // `projectTrustOverride` (`cli/args.js`), which becomes `isProjectTrusted()`,
    // which gates BOTH `if (projectTrusted)` blocks in `package-manager.js`
    // that admit project-scope resources — `{extensions, skills}` in the first
    // and `{prompts, themes}` in the second. Chief installs the role skill at
    // `<home>/.pi/skills` and the identity theme at `<home>/.pi/themes`, both
    // project scope, both inside those blocks.
    //
    // So dropping this flag does not produce a trust prompt for a headless
    // agent to hang on. It produces every person launching with NO ROLE SKILL
    // and NO IDENTITY THEME, with no prompt, no error, and nothing in any log
    // — an unexplained product regression. See
    // `dropping_approve_would_cost_every_person_their_role_skill_and_their_identity_theme`.
    argv.push("--approve".to_owned());
    argv.push("--tools".to_owned());
    argv.push(launch.tools.join(","));
    for extension in &launch.extensions {
        argv.push("--extension".to_owned());
        argv.push(path_arg(extension));
    }
    argv.push("--name".to_owned());
    argv.push(launch.display_name.clone());
    match &launch.session {
        Some(session) => {
            argv.push("--session".to_owned());
            argv.push(path_arg(session));
            // NOTHING ELSE. A relaunched pane is handed its session and no
            // synthesized text at all. Operator ruling: "don't insert
            // anything ever to anything. just boot the agent."
        }
        None => {
            argv.push(fresh_session_message(&launch.person_name, &spec.person_id, standing));
        }
    }

    PaneCommand { argv, cwd: launch.workspace.clone(), env: Vec::new() }
}

/// Whether the company this pane is being drawn into has ANYTHING in it yet.
///
/// # Why a person-less session is not the question
///
/// "This person has no transcript" is true of every hire a company ever makes,
/// and a new hire on a running company SHOULD get to work the moment its pane
/// comes up: there is a mandate, a manager who asked for it, and usually mail
/// already waiting. Handing that person a greeting instead of the work would be
/// the bug in the other direction.
///
/// The founding boot is the case where that same sentence is a lie. Seconds
/// after genesis the company holds exactly one person — the CEO chiefd's
/// normalizer minted — and nothing else: no departments, no goals, no schedule,
/// no history, and nobody has asked for anything. Told to "continue the next
/// real piece of work", the only way for the CEO to comply is to INVENT the
/// work, and it did exactly that: the operator watched a company they had just
/// created start building departments and hiring into them while they were
/// still reading the first screen.
///
/// # The discriminator, and why it is this one
///
/// TWO facts, both already in front of the actuator, neither of them persisted:
///
/// * **The company holds exactly one person.** Every department in this product
///   has a head, and a head LIVES in the department it heads (`CLAUDE.md`:
///   "heading a department means living in it"), so a company with one person
///   provably has no department but the root and has hired nobody. The count is
///   `placement::Topology::known_person_ids`, which the interpreter already
///   derives from the desired roster on every pass.
/// * **That person has never had a session.** A CEO that has already run and
///   been relaunched has a transcript, and a transcript means a history to
///   resume — the ordinary case, whatever the roster size.
///
/// Deliberately NOT a persisted "first boot" flag on the company. A flag is a
/// second answer to a question the store can already answer, and it is the
/// answer nobody updates: it survives a `chief rm` of everybody but the CEO, it
/// has to be cleared by somebody, and the clearing is what gets forgotten. Both
/// facts above are read fresh from the state the pass already holds, so a
/// company that grows past this shape stops matching it without anybody
/// remembering to say so.
///
/// # No assigned work is the third answer, and it needed a third fact
///
/// Operator ruling, 2026-08-18: *"what is assigned work? you mean no message or
/// goals? that's fine. Just let them idle until the 2min passes. never force
/// kill them."*
///
/// The founding arm above only catches the very first boot of a company. The
/// same hole reopens on every LATER one: a company was created, staffed with
/// five sleeping people, and nothing was ever asked of anybody. Two `Wake Up`
/// clicks handed two of them [`BootStanding::Established`] — *"continue the
/// next real piece of work. Do not send a startup or acknowledgement-only
/// message"* — with no work to continue. They went looking, found the chief
/// SOURCE TREE at the launcher root (which is mounted only so Pi can be
/// resolved), read it as the organization's mandate, created an Engineering
/// department, hired a head into it, recalled a third person and sent six
/// messages about "critical chiefd blockers". Two minutes, about half a dollar,
/// none of it asked for.
///
/// So the standing needs the one fact that says whether anything is asked of
/// this person: **is a message waiting in their mailbox?** Company goals are
/// not the other half of that question — they do not exist. #1047 dropped
/// `manager_goals`, `delegated_goals`, `goal_watches` and `goal_intents`
/// outright, and `organization-intercom.ts` states the consequence: "With goals
/// deleted, the mailbox IS the work queue." Nothing else counts, and
/// specifically nothing DISCOVERABLE counts: a repository, a source tree or any
/// other file the person can see is plumbing they were not given.
///
/// The fact travels the same way `session` does — published per pass by chiefd
/// on the launch catalog, read here, never persisted and never derived by this
/// client. It is chiefd's mailbox, so it is chiefd's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootStanding {
    /// A company seconds old: one person, nothing built, no transcript.
    Founding,
    /// A person with nothing asked of them: no transcript to resume and no
    /// message waiting. They come up, say so, and do nothing until asked.
    Idle,
    /// Work is waiting — a message in the mailbox, or a transcript to resume.
    Established,
}

impl BootStanding {
    /// Read the standing off the three facts the actuator already has: how many
    /// people the company holds, whether this person has a transcript, and
    /// whether a message is waiting for them.
    #[must_use]
    pub fn from_company(
        people_in_company: usize,
        session: Option<&Path>,
        pending_mail: bool,
    ) -> Self {
        if session.is_some() {
            // A transcript is a history to resume, whatever else is true.
            Self::Established
        } else if people_in_company <= 1 {
            Self::Founding
        } else if pending_mail {
            Self::Established
        } else {
            Self::Idle
        }
    }
}

/// The fresh-session initial message.
///
/// # Two messages, because there are two situations
///
/// [`BootStanding::Established`] keeps the sentence this function has always
/// sent (ported from `organizationPersonPiCommand`'s no-session branch,
/// `org-runtime.ts:425`): a person materialized into a running company has a
/// mandate, a manager and usually mail, so "continue the next real piece of
/// work" names something that actually exists, and the ban on an
/// acknowledgement-only message stops a fresh hire from spending its first turn
/// announcing itself to a room that did not ask.
///
/// [`BootStanding::Founding`] gets the opposite instruction, and the ban is
/// LIFTED for it. On a company that is seconds old an acknowledgement is not
/// noise — it is the entire correct output. There is no work to continue, so
/// the old sentence could only be complied with by inventing some, and the
/// operator's report is what that looks like from the outside: "it started
/// creating departments and stuff. It should not do anything. The very first
/// time, just start and let the user do anything."
///
/// [`BootStanding::Idle`] is the same lift for the same reason, one situation
/// wider: a person nobody has asked for anything. It says outright that what
/// they can SEE is not work they were given, because the reasoning that adopted the
/// launcher's own checkout as the company's project was otherwise flawless —
/// there was a source tree, there was no other candidate, and the message
/// demanded work.
///
/// TOMBSTONE: "Review your company goals and schedule in the focused recovery
/// check that follows" is gone from the founding and idle messages. The check
/// it refers to is the intercom's own work-resume prompt
/// (`organization-intercom.ts`'s `workResumePrompt`), which on both of those
/// boots now says the same thing this message does — pointing a person at it
/// while it asked for an orientation pass was the second half of the same push.
/// It also names "company goals", which have not existed since #1047.
fn fresh_session_message(person_name: &str, person_id: &str, standing: BootStanding) -> String {
    match standing {
        BootStanding::Founding => format!(
            "You are {person_name} ({person_id}), and your company was created moments ago. You \
             are the only person in it: there are no departments, no goals, no schedule and no \
             history, so there is no work in flight and nothing to continue. Read your AGENTS.md, \
             then introduce yourself in two or three sentences — who you are and what you can do \
             — and stop there. Create no department, hire nobody, and start no work of any kind \
             until you are asked for something."
        ),
        BootStanding::Idle => format!(
            "You are {person_name} ({person_id}), and nothing is assigned to you: no message is \
             waiting for you. Read your AGENTS.md, then say in one line that you are up and \
             available, and stop there. Do not go looking for something to do — a file, a \
             repository or a source tree you can see is not work anybody gave you. Create no \
             department, hire nobody, send no message, and start no work of any kind until \
             somebody asks you for something."
        ),
        BootStanding::Established => format!(
            "You are {person_name} ({person_id}) at work in your company. Read your AGENTS.md. \
             Review your company goals and schedule in the focused recovery check that follows, \
             then continue the next real piece of work. Do not send a startup or \
             acknowledgement-only message."
        ),
    }
}

/// Render a path for argv. A lossy display is acceptable here (matching the
/// rest of this module): pi-home paths are launcher-controlled, never
/// arbitrary user input.
fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The placement every test in this module launches against, unless it is
    /// specifically about placement.
    fn placement() -> PanePlacement<'static> {
        PanePlacement { socket: "cobalt-sock", session: "org-cobalt_" }
    }

    fn launch() -> LaunchSpec {
        LaunchSpec {
            pi_binary: PathBuf::from("/opt/pi/bin/pi"),
            pi_home: PathBuf::from("/data/cobalt/.chief/agent/vera"),
            workspace: PathBuf::from("/data/cobalt/people/vera/workspace"),
            display_name: "Cobalt · Quant Head".to_owned(),
            person_name: "Vera".to_owned(),
            accent: Some("#3c7adf".to_owned()),
            tools: vec!["read".to_owned(), "bash".to_owned()],
            extensions: [
                "/opt/chief/packages/piing/extensions/organization-intercom.ts",
                "/opt/chief/packages/piing/extensions/team-ui.ts",
                "/opt/chief/packages/piing/extensions/tribes-welcome.ts",
            ]
            .map(PathBuf::from)
            .to_vec(),
            session: None,
            pending_mail: false,
            env: vec![
                ("ORG_LAUNCHER_ORGANIZATION".to_owned(), "cobalt".to_owned()),
                ("ORG_LAUNCHER_PERSON".to_owned(), "vera".to_owned()),
                (
                    "PI_CODING_AGENT_SESSION_DIR".to_owned(),
                    "/data/cobalt/.chief/agent/vera/sessions".to_owned(),
                ),
            ],
        }
    }

    fn spec() -> SpawnSpec {
        SpawnSpec { person_id: "vera".to_owned(), launch_hash: "hash-3".to_owned() }
    }

    #[test]
    fn a_pane_carries_this_actuators_own_placement_and_never_a_published_one() {
        // AC6, both directions. FORWARD: the pair is always written, and
        // always together — `organization-intercom.ts` throws at extension
        // load if it sees one without the other, which kills the pane before
        // it can be tagged. BACKWARD: the values come from THIS actuator's
        // placement argument, not from the catalog, so pointing the same
        // catalog entry at a different tmux server moves the pane env with it.
        let here = launch_command(
            &spec(),
            &launch(),
            &PanePlacement { socket: "cobalt-sock", session: "org-cobalt_" },
            BootStanding::Established,
            None,
        );
        assert!(here.argv.iter().any(|w| w == "ORG_LAUNCHER_RUNTIME_SOCKET=cobalt-sock"));
        assert!(here.argv.iter().any(|w| w == "ORG_LAUNCHER_RUNTIME_SESSION=org-cobalt_"));

        let elsewhere = launch_command(
            &spec(),
            &launch(),
            &PanePlacement { socket: "other-sock", session: "org-other_" },
            BootStanding::Established,
            None,
        );
        assert!(elsewhere.argv.iter().any(|w| w == "ORG_LAUNCHER_RUNTIME_SOCKET=other-sock"));
        assert!(elsewhere.argv.iter().any(|w| w == "ORG_LAUNCHER_RUNTIME_SESSION=org-other_"));
        assert!(
            !elsewhere.argv.iter().any(|w| w.contains("cobalt-sock")),
            "the placement is the actuator's, so nothing may carry the old one: {:?}",
            elsewhere.argv
        );

        // Neither half is ever emitted alone, on either placement.
        for cmd in [&here, &elsewhere] {
            let socket =
                cmd.argv.iter().filter(|w| w.starts_with("ORG_LAUNCHER_RUNTIME_SOCKET=")).count();
            let session =
                cmd.argv.iter().filter(|w| w.starts_with("ORG_LAUNCHER_RUNTIME_SESSION=")).count();
            assert_eq!((socket, session), (1, 1), "exactly one of each, always both");
        }
    }

    /// The regression fixture: the pi CLI shape a fresh spawn must produce
    /// under the SA-2/SA-3 boot contract — a `/usr/bin/env` invocation with
    /// the non-secret pane environment as argv assignments, `--tools`, the
    /// exact shipped organization extensions, and no `--system-prompt` /
    /// `--skill` / `--prompt-template` / provider-env shell prelude.
    #[test]
    fn a_fresh_spawn_execs_the_real_pi_cli_shape_via_argv_env_assignments() {
        // `Some(Light)` rather than `None`, because a box with the browser
        // bridge running is the ordinary case and the seeded ground is part of
        // the shape: the fixture is the whole argv a real launch produces, not
        // the subset that survives a missing authority.
        let cmd = launch_command(
            &spec(),
            &launch(),
            &placement(),
            BootStanding::Established,
            Some(Appearance::Light),
        );
        assert_eq!(
            cmd.argv,
            vec![
                "/usr/bin/env",
                "-C",
                "/data/cobalt/people/vera/workspace",
                // Pi resolves its colour mode from the environment; a daemon-
                // launched pane inherits none, so the launcher must supply it
                // or every identity accent is quantised to 256-colour (#80).
                "COLORTERM=truecolor",
                // The ground the pane is drawn on, from `/run/tribes-theme` —
                // the same authority the rail draws itself from. Inherited, it
                // is the daemon's frozen copy, and Pi persists what it derives
                // from it as an explicit theme that no later bridge change can
                // take back.
                "COLORFGBG=0;15",
                // AC6: the pane's PLACEMENT, stated by the process that is
                // actually drawing it. chiefd published these two out of its
                // `ActuatorConfig` until this change; it cannot see a display,
                // and this actuator knows the socket and session it is on.
                "ORG_LAUNCHER_RUNTIME_SOCKET=cobalt-sock",
                "ORG_LAUNCHER_RUNTIME_SESSION=org-cobalt_",
                "ORG_LAUNCHER_ORGANIZATION=cobalt",
                "ORG_LAUNCHER_PERSON=vera",
                "PI_CODING_AGENT_SESSION_DIR=/data/cobalt/.chief/agent/vera/sessions",
                "/bin/sh",
                "-c",
                PANE_STARTUP_SCRIPT,
                PANE_STARTUP_WRAPPER_ARG0,
                "Vera",
                "/opt/pi/bin/pi",
                "--approve",
                "--tools",
                "read,bash",
                "--extension",
                "/opt/chief/packages/piing/extensions/organization-intercom.ts",
                "--extension",
                "/opt/chief/packages/piing/extensions/team-ui.ts",
                "--extension",
                "/opt/chief/packages/piing/extensions/tribes-welcome.ts",
                "--name",
                "Cobalt · Quant Head",
                "You are Vera (vera) at work in your company. Read your AGENTS.md. Review your \
                 company goals and schedule in the focused recovery check that follows, then \
                 continue the next real piece of work. Do not send a startup or \
                 acknowledgement-only message.",
            ]
        );
        assert_eq!(cmd.cwd, PathBuf::from("/data/cobalt/people/vera/workspace"));
        // The whole environment travels in argv; nothing is left for tmux -e.
        assert!(cmd.env.is_empty());
    }

    #[test]
    fn fresh_session_copy_speaks_to_a_person_in_their_company_not_about_the_launcher() {
        let message = fresh_session_message("Vera", "vera", BootStanding::Established);
        assert_eq!(
            message,
            "You are Vera (vera) at work in your company. Read your AGENTS.md. Review your \
             company goals and schedule in the focused recovery check that follows, then continue \
             the next real piece of work. Do not send a startup or acknowledgement-only message."
        );
        for implementation_term in ["ChiefD", "chiefd", "daemon", "system owner", "system-owner"] {
            assert!(
                !message.contains(implementation_term),
                "fresh-session copy must not expose {implementation_term}"
            );
        }
    }

    /// The operator's report, as a test: "I just boot the founder ... it
    /// started creating departments and stuff. It should not do anything. The
    /// very first time, just start and let the user do anything."
    ///
    /// A company seconds old holds one person and nothing else, so the pane's
    /// first message must ask for an introduction and forbid the building. The
    /// assertion is on the ARGV — the message is a pi CLI argument, and the
    /// only thing that reaches the model is what lands there.
    #[test]
    fn the_founding_boot_is_told_to_introduce_itself_and_build_nothing() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Founding, None);
        let message = cmd.argv.last().expect("the fresh-session message is the final argument");
        assert!(message.contains("You are Vera (vera)"), "{message}");
        assert!(message.contains("created moments ago"), "{message}");
        assert!(message.contains("introduce yourself"), "{message}");
        assert!(
            message.contains("Create no department, hire nobody"),
            "the founding boot must be told not to build the org: {message}"
        );
        // The instruction that produced the defect, and the ban that stopped
        // the CEO from doing the one correct thing instead.
        assert!(
            !message.contains("continue the next real piece of work"),
            "there is no next piece of work in a company with no work: {message}"
        );
        assert!(
            !message.contains("acknowledgement-only"),
            "an acknowledgement IS the correct founding output, so the ban is lifted: {message}"
        );
    }

    /// The other half of the same rule, and the reason the discriminator is not
    /// merely "this person has no session": a hire on a running company still
    /// gets to work the moment its pane comes up.
    #[test]
    fn a_later_hire_with_no_session_still_gets_to_work() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        let message = cmd.argv.last().expect("the fresh-session message is the final argument");
        assert!(message.contains("continue the next real piece of work"), "{message}");
        assert!(
            message.contains("Do not send a startup or acknowledgement-only message"),
            "{message}"
        );
        assert!(!message.contains("introduce yourself"), "{message}");
    }

    /// THE founding discriminator. Both facts are required, and each one alone
    /// is the wrong answer: a lone person WITH a transcript has a history to
    /// resume, and a session-less person in a staffed company is not
    /// necessarily a company that was created seconds ago.
    #[test]
    fn founding_is_one_person_who_has_never_run_and_nothing_else() {
        let transcript = PathBuf::from("/data/cobalt/.chief/agent/vera/sessions/abc.jsonl");
        assert_eq!(BootStanding::from_company(1, None, false), BootStanding::Founding);
        // A company chiefd has minted but whose roster this pass has not read
        // is not a reason to invent work either.
        assert_eq!(BootStanding::from_company(0, None, false), BootStanding::Founding);
        assert_eq!(
            BootStanding::from_company(1, Some(transcript.as_path()), false),
            BootStanding::Established,
            "a CEO with a transcript has a history to resume"
        );
        assert_eq!(
            BootStanding::from_company(12, Some(transcript.as_path()), false),
            BootStanding::Established
        );
    }

    /// NO ASSIGNED WORK IS ITS OWN STANDING, and the mailbox is the whole of
    /// the question.
    ///
    /// This is the case the founding arm cannot see: a staffed company where
    /// nothing has been asked of anybody. Five sleeping people, two `Wake Up`
    /// clicks, and `from_company(5, None)` answered `Established` — "continue
    /// the next real piece of work" — so the woken people went and found some.
    #[test]
    fn a_session_less_person_with_no_mail_is_idle_and_one_with_mail_is_not() {
        assert_eq!(
            BootStanding::from_company(5, None, false),
            BootStanding::Idle,
            "a woken person in a staffed company with an empty mailbox has nothing to do"
        );
        assert_eq!(
            BootStanding::from_company(5, None, true),
            BootStanding::Established,
            "mail waiting IS assigned work; a new hire still gets to work on spawn"
        );
        // The transcript still wins over the mailbox: a person who has already
        // run resumes their own history, and this client never sends the
        // fresh-session message for them at all.
        let transcript = PathBuf::from("/data/cobalt/.chief/agent/vera/sessions/abc.jsonl");
        assert_eq!(
            BootStanding::from_company(5, Some(transcript.as_path()), false),
            BootStanding::Established
        );
        // The founding boot outranks the idle arm. It has no mail either, but
        // it gets its own sentence, which names why there is nothing to do.
        assert_eq!(BootStanding::from_company(1, None, false), BootStanding::Founding);
    }

    /// The idle message says the three things the operator's ruling needs it to
    /// say, and does NOT say the thing that caused the incident.
    #[test]
    fn the_idle_message_lifts_the_acknowledgement_ban_and_disowns_what_is_on_disk() {
        let message = fresh_session_message("Vera", "vera", BootStanding::Idle);
        assert!(message.contains("Vera (vera)"), "{message}");
        assert!(message.contains("nothing is assigned to you"), "{message}");
        assert!(message.contains("up and available"), "{message}");
        // The ban is what made hunting for work the cheapest compliant turn.
        assert!(!message.contains("acknowledgement-only"), "{message}");
        // A source tree is not work anybody gave them; this sentence is the one that
        // answers the reasoning that adopted the launcher's own checkout.
        assert!(
            message.contains("source tree you can see is not work anybody gave you"),
            "{message}"
        );
        assert!(message.contains("Create no department, hire nobody"), "{message}");
        for push in ["next real piece of work", "company goals", "focused recovery check"] {
            assert!(!message.contains(push), "idle message must not push: {push}\n{message}");
        }
    }

    /// A PERSON WITH WORK IS UNCHANGED. The exact sentence, word for word,
    /// including the ban — this is the arm the ruling deliberately does not
    /// touch, and the one an over-broad idle rule would swallow.
    #[test]
    fn the_established_message_is_untouched_and_keeps_its_ban() {
        assert_eq!(
            fresh_session_message("Vera", "vera", BootStanding::Established),
            "You are Vera (vera) at work in your company. Read your AGENTS.md. Review your \
             company goals and schedule in the focused recovery check that follows, then continue \
             the next real piece of work. Do not send a startup or acknowledgement-only message."
        );
    }

    /// DROPPING `--approve` COSTS EVERY PERSON THEIR SKILL AND THEIR THEME.
    ///
    /// The name carries the consequence rather than the mechanism, because the
    /// mechanism reads as removable and the consequence does not. A reader who
    /// finds `--approve`, notes that these agents are headless and could never
    /// answer a trust prompt anyway, and deletes it as dead weight, is making
    /// exactly the inference this name exists to interrupt.
    ///
    /// Since chief stopped redirecting `PI_CODING_AGENT_DIR`, the role skill
    /// lives at `<home>/.pi/skills` and the identity theme at
    /// `<home>/.pi/themes` — both PROJECT scope, and Pi admits project-scope
    /// resources only when the project is trusted (`package-manager.js`, two
    /// `if (projectTrusted)` blocks). This flag is what makes that true.
    ///
    /// The failure it guards is silent: no prompt, no error, no log line —
    /// every person simply comes up with no role and no identity.
    #[test]
    fn dropping_approve_would_cost_every_person_their_role_skill_and_their_identity_theme() {
        for session in
            [None, Some(PathBuf::from("/data/cobalt/.chief/agent/vera/sessions/a.jsonl"))]
        {
            let mut launch = launch();
            launch.session = session;
            let cmd =
                launch_command(&spec(), &launch, &placement(), BootStanding::Established, None);
            assert_eq!(
                cmd.argv.iter().filter(|arg| arg.as_str() == "--approve").count(),
                1,
                "fresh and resumed subordinate agents must carry --approve exactly once: it is \
                 what admits their project-scope role skill and identity theme, not merely what \
                 spares them a prompt"
            );
        }
    }

    /// Nobody gets an appearance argument any more, so there is no longer a
    /// standard identity to exempt from one -- the exemption test this
    /// replaces asserted the CEO alone launched without `--theme`.
    #[test]
    fn no_pane_is_told_what_it_should_look_like() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        for retired in ["--theme", "--no-themes"] {
            assert!(!cmd.argv.iter().any(|arg| arg == retired), "{retired} is retired");
        }
        assert!(cmd.argv.iter().all(|arg| !arg.contains("themes/")));
    }

    #[test]
    fn retired_capability_and_system_prompt_arguments_are_never_emitted() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        for retired in [
            "--skill",
            "--prompt-template",
            "--no-extensions",
            "--no-context-files",
            "--no-prompt-templates",
            "--system-prompt",
        ] {
            assert!(!cmd.argv.iter().any(|arg| arg == retired), "{retired} is retired");
        }
        // The retired intermediates themselves appear nowhere.
        let joined = cmd.argv.join("\u{0}");
        assert!(!joined.contains("provider.env"));
        assert_eq!(
            cmd.argv.iter().filter(|arg| arg.as_str() == "--extension").count(),
            3,
            "each shipped organization extension is an explicit Pi argument"
        );
        assert!(!joined.contains("role.md"));
        assert!(!joined.contains("resources.json"));
    }

    /// The route arguments went with provider/model management and the
    /// appearance arguments went with the generated themes; a pane is told
    /// what it may DO and who it is, never who to ask or how to look.
    #[test]
    fn a_pane_is_told_no_route_and_no_appearance() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        for retired in ["--provider", "--model", "--thinking"] {
            assert!(!cmd.argv.iter().any(|arg| arg == retired), "{retired} is retired");
        }
        assert!(
            !cmd.argv.iter().any(|arg| arg.starts_with("ORG_CUSTOM_PROVIDERS=")),
            "the custom-provider transport contract died with provider management"
        );
        assert!(
            !cmd.argv.iter().any(|arg| arg.starts_with("PI_OFFLINE=")),
            "plain Pi must refresh its own provider metadata instead of using a Chief-frozen catalog"
        );
    }

    #[test]
    fn a_resumed_spawn_carries_session_instead_of_the_initial_message() {
        let mut launch = launch();
        launch.session = Some(PathBuf::from("/data/cobalt/.chief/agent/vera/sessions/abc.jsonl"));
        let cmd = launch_command(&spec(), &launch, &placement(), BootStanding::Established, None);
        let joined = cmd.argv.join("\u{0}");
        assert!(joined.contains("--session\u{0}/data/cobalt/.chief/agent/vera/sessions/abc.jsonl"));
        assert!(!joined.contains("You are Vera (vera) at work in your company"));
    }

    /// THE ABSENCE PIN: a relaunched pane receives NO synthesized text.
    ///
    /// This replaces two tests whose subject was the resume copy -- one pinning
    /// that the client selected the sentence chiefd wrote for the cause it was
    /// acting on, one pinning that a missed lookup was silence rather than a
    /// substitute. Both were correct about a feature that is now deleted.
    ///
    /// Operator ruling: *"don't insert anything ever to anything. just boot the
    /// agent. do the same for all agents."* The reported symptom was a CEO that
    /// had been created moments earlier and had never run receiving the
    /// paragraph beginning "You were interrupted: your process was no longer
    /// running and was started again" -- a sentence about a turn that had never
    /// happened, in a context that had nothing in it.
    ///
    /// WHY AN ABSENCE NEEDS A TEST AT ALL. The code that violates this rule
    /// does not exist yet, so there is nothing to assert about, and the rule
    /// would otherwise live in a comment -- which is prose a future change
    /// reads past. "The agent doesn't know what happened to it" is a reasonable
    /// thing to notice and injecting an explanation is the reflex fix, exactly
    /// as it was the first time. This test is what makes that regrowth loud.
    #[test]
    fn a_resumed_pane_is_handed_its_session_and_no_synthesized_text() {
        let mut launch = launch();
        launch.session = Some(PathBuf::from("/data/cobalt/.chief/agent/vera/sessions/abc.jsonl"));
        let cmd = launch_command(&spec(), &launch, &placement(), BootStanding::Established, None);

        // The session IS handed over: this rule bans invented copy, not resume.
        let session = cmd
            .argv
            .iter()
            .position(|arg| arg == "--session")
            .expect("a resumed pane still carries --session");
        assert_eq!(
            cmd.argv.get(session + 1).map(String::as_str),
            Some("/data/cobalt/.chief/agent/vera/sessions/abc.jsonl"),
        );

        // And it is the LAST thing on the line. Anything after the session path
        // would be a positional prompt argument, which is the shape the deleted
        // feature had and the shape any regrowth would take.
        assert_eq!(
            session + 2,
            cmd.argv.len(),
            "a resumed pane's argv must END at its session path; anything after it is a \
             synthesized prompt: {:?}",
            cmd.argv
        );
    }

    /// A first boot has no transcript, so it is told nothing about an
    /// interruption that never happened. Kept after the resume copy was
    /// deleted because its SUBJECT is the fresh-session branch, not the
    /// deleted one: it still proves a pane with no session takes the other
    /// path.
    #[test]
    fn a_first_boot_is_told_nothing_about_an_interruption_that_never_happened() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        let joined = cmd.argv.join(" ");
        assert!(!joined.contains("interrupted"), "{joined}");
        assert!(
            joined.contains("You are Vera (vera) at work in your company"),
            "a first boot gets the fresh message"
        );
    }

    // TOMBSTONE: `a_delayed_spawn_sleeps_the_admission_stagger_around_the_env_invocation`
    // and `fractional_second_delays_render_with_three_decimals`. Both pinned the
    // `sh -c 'sleep <secs>; exec "$@"'` wrapper and its three-decimal rendering.
    // The ramp is deleted by operator ruling, so there is no delay for a spawn
    // to sleep and no `format_delay_seconds` to render one. Deleted rather than
    // weakened: the behaviour they asserted is gone, not merely unasserted, and
    // `boots_every_missing_pane_in_one_pass_with_no_ramp_at_all` in
    // `plan/tests.rs` fails if any pacing reappears.

    #[test]
    fn every_host_respawn_captures_and_validates_the_tmux_pane_before_pi_runs() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        let wrapper = cmd.argv.iter().position(|arg| arg == PANE_STARTUP_SCRIPT).unwrap();
        assert_eq!(cmd.argv[wrapper - 2], "/bin/sh");
        assert_eq!(cmd.argv[wrapper - 1], "-c");
        assert_eq!(cmd.argv[wrapper + 1], PANE_STARTUP_WRAPPER_ARG0);
        assert_eq!(cmd.argv[wrapper + 2], "Vera");
        assert_eq!(cmd.argv[wrapper + 3], "/opt/pi/bin/pi");
        assert!(PANE_STARTUP_SCRIPT.contains("${TMUX_PANE:?missing tmux pane}"));
        assert!(PANE_STARTUP_SCRIPT.contains("%[0-9]*"));
        assert!(PANE_STARTUP_SCRIPT.contains("printf"));
        assert!(PANE_STARTUP_SCRIPT.contains("is starting"));
        assert!(PANE_STARTUP_SCRIPT.contains("ORG_LAUNCHER_PANE_ID=\"$pane\""));
        assert!(PANE_STARTUP_SCRIPT.contains("exec /usr/bin/env"));
        assert!(PANE_STARTUP_SCRIPT.contains("\"$@\""));
    }

    #[test]
    fn no_argument_ever_carries_a_credential_shaped_value() {
        // Invariant 32 holds by construction now: chiefd holds no credential at
        // any point, so there is nothing for argv to leak.
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        let joined = cmd.argv.join(" ");
        assert!(!joined.contains("sk-"));
        for (_, value) in &cmd.env {
            assert!(!value.contains("sk-"));
        }
    }

    /// #80: chiefd is a daemon, so `/usr/bin/env` inherits no COLORTERM and
    /// every pane it launched resolved 256-colour -- quantising each identity
    /// accent to its nearest palette index (#e5c07b -> 38;5;180) while the pane
    /// header kept the exact hex. Measured live: 0 of 72 pi processes had it.
    /// The launcher must SET it rather than rely on inheritance, which is why
    /// this asserts on the argv chiefd builds, not on an ambient variable.
    #[test]
    fn the_pane_is_launched_in_truecolor_so_identity_accents_are_never_quantised() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        assert!(
            cmd.argv.contains(&"COLORTERM=truecolor".to_owned()),
            "a launched pane must resolve TRUECOLOR, never 256-colour: {:?}",
            cmd.argv
        );
        // Set before the catalog's own assignments, so a catalog-supplied
        // COLORTERM still wins (later `env` assignments override earlier ones).
        let colorterm =
            cmd.argv.iter().position(|a| a.starts_with("COLORTERM=")).expect("COLORTERM");
        let first_catalog =
            cmd.argv.iter().position(|a| a.starts_with("ORG_LAUNCHER_")).expect("catalog env");
        assert!(
            colorterm < first_catalog,
            "COLORTERM must precede catalog env so the catalog can override it"
        );
    }

    /// The Chief pane rendering DARK on a LIGHT box, as a test.
    ///
    /// Measured: `/run/tribes-theme` said `light` with a fresh mtime, the rail
    /// drew light (#ede7f6 on #5b21b6), and the pane beside it drew #d4d4d4
    /// body text. The pane had inherited chiefd's own frozen `COLORFGBG=15;0`,
    /// Pi derived dark from it, and — for the Chief, the one person with no
    /// a redirect and therefore the one person Pi ran first-time
    /// setup for — WROTE that answer into the operator's global
    /// `~/.pi/agent/settings.json` as a bare `"theme": "dark"`. An explicit
    /// theme does not follow the live bridge (DECISIONS.md, 2026-08-17), so the
    /// `light` bridge could never win it back: one stale variable at one chief
    /// start, and that operator's Chief pane is dark for good.
    #[test]
    fn a_pane_is_seeded_with_the_ground_the_authority_declares() {
        let light = launch_command(
            &spec(),
            &launch(),
            &placement(),
            BootStanding::Established,
            Some(Appearance::Light),
        );
        assert!(
            light.argv.contains(&"COLORFGBG=0;15".to_owned()),
            "a light bridge must seed the light ground: {:?}",
            light.argv
        );

        let dark = launch_command(
            &spec(),
            &launch(),
            &placement(),
            BootStanding::Established,
            Some(Appearance::Dark),
        );
        assert!(
            dark.argv.contains(&"COLORFGBG=15;0".to_owned()),
            "a dark bridge must seed the dark ground: {:?}",
            dark.argv
        );

        // Exactly one assignment, and it precedes the catalog's own env for the
        // same reason COLORTERM does: a catalog value must still be able to
        // override it.
        for cmd in [&light, &dark] {
            assert_eq!(
                cmd.argv.iter().filter(|word| word.starts_with("COLORFGBG=")).count(),
                1,
                "one ground, stated once"
            );
            let ground =
                cmd.argv.iter().position(|word| word.starts_with("COLORFGBG=")).expect("COLORFGBG");
            let first_catalog = cmd
                .argv
                .iter()
                .position(|word| word.starts_with("ORG_LAUNCHER_"))
                .expect("catalog env");
            assert!(ground < first_catalog);
        }
    }

    /// NO AUTHORITY, NO ASSIGNMENT. A box with no bridge file leaves this
    /// launcher knowing nothing about the operator's screen, and a guess here
    /// is not a colour that is wrong for one draw — Pi persists what it derives
    /// into an explicit theme, so the guess outlives the process that made it.
    /// The pane keeps whatever it would have inherited instead.
    #[test]
    fn an_absent_authority_seeds_nothing_rather_than_guessing_a_ground() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        assert!(
            !cmd.argv.iter().any(|word| word.starts_with("COLORFGBG=")),
            "an unstated ground must not become a stated one: {:?}",
            cmd.argv
        );
        // The colour facts are independent: truecolor is a property of the
        // TERMINAL and is always true of a tmux pane, so it is stated whether
        // or not anybody has declared a ground.
        assert!(cmd.argv.contains(&"COLORTERM=truecolor".to_owned()));
    }

    #[test]
    fn the_person_travels_into_the_launch_identity() {
        let cmd = launch_command(
            &SpawnSpec { person_id: "quant-head".to_owned(), launch_hash: "hash-12".to_owned() },
            &launch(),
            &placement(),
            BootStanding::Established,
            None,
        );
        let joined = cmd.argv.join(" ");
        assert!(joined.contains("--name Cobalt · Quant Head"));
        assert!(joined.contains("You are Vera (quant-head) at work in your company"));
        // The person's static identity env is preserved.
        assert!(cmd.argv.iter().any(|arg| arg == "ORG_LAUNCHER_ORGANIZATION=cobalt"));
    }

    #[test]
    fn the_pane_argv_carries_no_launcher_provider_concurrency_bound() {
        // #748: provider capacity is the provider's own authority. The pane
        // command must never carry a launcher concurrency value — the bound
        // field, its argv assignment, and its ambient forwarding are all gone.
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        assert!(
            !cmd.argv.iter().any(|arg| arg.starts_with("ORGANIZATION_PROVIDER_MAX_CONCURRENCY")),
            "the pane argv must not carry a launcher provider-concurrency bound"
        );
    }

    /// THE regression test for `tmux new-session failed: command too long`.
    ///
    /// `interpret::create_session` sends this argv, the session-creating
    /// command and every identity tag as ONE tmux client message, and tmux
    /// 3.3a rejects that message past ~16-18 KB (measured: 16000 accepted,
    /// 18000 refused). A 337-model registry put 50213 bytes into the single
    /// `ORG_CUSTOM_PROVIDERS` word, so `new-session` failed for every person,
    /// the actuator retried until its breaker tripped to shadow, and no
    /// company could boot.
    ///
    /// `ORG_CUSTOM_PROVIDERS` is deleted with provider management, so the word
    /// that actually blew the ceiling is gone. The bound stays: it exists
    /// because an argv that can grow without anybody CHOOSING to grow it is the
    /// failure, and any future field could be one again. It deliberately
    /// asserts the BUDGET rather than an exact size, so an honest new argument
    /// is free and an operator-sized one is not.
    #[test]
    fn a_pane_argv_stays_far_under_the_tmux_message_ceiling_for_a_large_catalog() {
        let cmd = launch_command(&spec(), &launch(), &placement(), BootStanding::Established, None);
        let bytes = pane_argv_bytes(&cmd.argv);
        assert!(
            bytes <= MAX_PANE_ARGV_BYTES,
            "a pane argv of {bytes} bytes exceeds the {MAX_PANE_ARGV_BYTES}-byte budget; tmux \
             carries this and the whole session-creating command list in ONE message and refuses \
             it past ~16 KB, which is no pane at all rather than a degraded one"
        );
    }

    /// The bound above is only meaningful if it can actually fail. A value of
    /// the size that produced the defect — 50213 bytes, the 337-model registry
    /// — must still blow it, whichever field carries it.
    #[test]
    fn the_argv_budget_rejects_an_operator_sized_projection() {
        let mut launch_spec = launch();
        launch_spec.env.push(("ORG_LAUNCHER_OVERSIZED".to_owned(), "x".repeat(50_213)));
        let cmd =
            launch_command(&spec(), &launch_spec, &placement(), BootStanding::Established, None);
        assert!(
            pane_argv_bytes(&cmd.argv) > MAX_PANE_ARGV_BYTES,
            "the budget must reject the 50213-byte value that produced 'command too long'"
        );
    }
}
