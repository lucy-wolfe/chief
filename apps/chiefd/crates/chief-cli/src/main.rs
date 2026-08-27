//! `chief` — the operator client, and the program an operator installs.
//!
//! # The mandate this binary exists to serve
//!
//! chiefd the BACKEND is client-agnostic: it holds the business logic, it
//! exposes an HTTP API, and it must know nothing about tmux and nothing about
//! the web. `apps/web` is one frontend over that API; this crate is the other.
//! It owns every tmux concern on the operator side and reaches a company's
//! daemon only over HTTP.
//!
//! **The dependency list in `Cargo.toml` is the architecture.** This crate
//! links none of `chiefd-core`, `chiefd-host`, `chiefd-api` or `chiefd-daemon`,
//! and `scripts/test/backend-tmux-boundary.test.mjs` enforces that in both
//! directions — rule 5 forbids a backend crate naming a client crate, rule 7
//! forbids this crate naming a backend one.
//!
//! # The library half
//!
//! PLACEMENT lives in `src/lib.rs` (P5): the roster wire (`roster`), the
//! session/window/pane rules computed from it (`placement`), and the layout
//! geometry (`layout`). It sits behind a library boundary because it is pure
//! arithmetic over published facts, which is what lets
//! `apps/chiefd/tests/fixtures/placement-golden.json` hold it and chiefd's own
//! still-live planner to one answer. Nothing is actuated from it yet; chiefd
//! still drives tmux, and the switchover is P8. This binary keeps everything
//! that touches the world.
//!
//! # Two binaries, and why
//!
//! `~/.chief/bin/` holds `chief` (this program) and `chiefd` (the
//! backend), installed together by `scripts/release-chiefd.ts`. Before P6 of
//! the design record they were ONE executable and
//! `chiefd run` was that executable re-invoking itself — which forced the
//! operator half to link `chiefd-core` and `chiefd-host`, precisely the
//! coupling the mandate removes. Keeping one binary and making the CLI a
//! library was rejected for the same reason: the guard would be satisfied
//! while the coupling remained.
//!
//! So the daemon modes ([`DAEMON_VERBS`]) are `exec`'d into `chiefd`
//! rather than answered here. Spawning a program is legitimate — it is the same
//! category as spawning Pi, which this binary also does; linking backend
//! business logic into a presentation client is not.
//!
//! # What each verb decides, and where
//!
//! | verb | decision |
//! |---|---|
//! | `ls` | is a company running — [`listing::derive_status`] |
//! | `attach` | attach, or start-then-attach; refuse an unhealthy daemon up front — [`attach`] |
//! | `stop` | tear the runtime down durable-first, then the daemon — [`stop`] |
//! | `reset` | shed to CEO-only without deleting a byte — [`reset`] |
//! | `rm` | stop it, delete its durable state, drop its registry row last — [`remove`] |
//! | `new` | host the Founder and own company genesis — [`founder`], [`genesis`] |
//! | `host` | the same verbs, made resident for `apps/api` — [`host`] |
//!
//! # Reactive (Mandate 1)
//!
//! Nothing here polls a company's state. `attach`/`reset` reach CEO-only
//! without asking anybody for it: an omitted launch intent is an empty
//! allow-list, so the fence admits the root head alone and the root's
//! unconditional lease keeps it desired. `reset` clears the intent on its way
//! down and `attach` puts an actuator on the socket to converge what is already
//! published. The client never states a boot, never drives a reconcile and
//! never waits for one.
//!
//! The two bounded waits that remain are OS-liveness waits — "has the child I
//! just forked bound its port", "has the process I asked to exit actually
//! exited" — for which no push channel exists on either side. Each is bounded
//! by an explicit deadline and each sleep carries a sited allow naming that
//! reason, exactly as the TypeScript reactive allowlist did before it.

#![forbid(unsafe_code)]

mod attach;
mod build_identity;
mod company;
mod confirm;
mod daemon;
mod discovery;
mod founder;
mod founder_pi;
mod genesis;
mod host;
mod http;
mod listing;
mod paths;
mod preflight;
mod remove;
mod reset;
mod stand_down;
mod stop;
mod terminal;
mod tmux;
mod upgrade;

use chief_cli::actuate::client::ActuationClient;
use chief_cli::actuate::supervise;
use chief_cli::bearer::Bearer;
use chief_cli::placement;

use std::process::ExitCode;

/// THE FIRST-RUN CHECK, as a refusal every company verb shares.
///
/// A verb that acts on the cwd has exactly one way to be asked for something
/// that is not there: the operator is standing somewhere else. The answer names
/// the directory it looked in and the one verb that makes a company — never
/// "unknown company 'x'", which was the old shape and is now unwriteable,
/// because nobody typed an `x`.
///
/// It asks [`paths::company_present`], which is the store file's existence and
/// nothing else: a `.chief/` a crashed genesis left half-built is not a company
/// a caller can act on.
///
/// # Errors
/// [`LifecycleError::Refused`] when this directory holds no company.
fn require_a_company_here(dir: &std::path::Path, verb: &str) -> Result<()> {
    if paths::company_present(dir) {
        return Ok(());
    }
    Err(LifecycleError::refused(format!(
        "{verb}: there is no company in {}. A company is the directory you run `chief` in — \
         `cd` to one that has a .chief/db/chief.db, or run bare `chief` here to open Founder mode.",
        dir.display()
    )))
}

/// The daemon program this client `exec`s, and the file
/// `scripts/release-chiefd.ts` installs beside this one.
///
/// Written down once, here, and read by [`paths::chiefd_daemon_binary`] and by
/// the forwarder. A second copy of a program name is how an install ends up
/// with two binaries that disagree about which one serves a company.
pub(crate) const DAEMON_PROGRAM: &str = "chiefd";

/// A routed operator invocation.
///
/// Parsed, not executed: keeping the parse pure is what lets the whole argv
/// contract be unit-tested without a tmux server, a beacond, or a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// `chief help | --help | -h`, and `chief <verb> --help`.
    Help,
    /// `chief --version | -V`.
    Version,
    /// Bare `chief` — FOUND here, or GO IN here.
    ///
    /// Which one is not decided by the parse. It is decided by whether
    /// `<dir>/.chief/db/chief.db` exists, and a parse that stats the
    /// filesystem could not be unit-tested without one — the property this
    /// whole enum exists to keep. The runner asks; see the dispatch arm.
    ///
    /// TOMBSTONE: this used to be `Overview`, the box-wide running-companies
    /// projection. That is what `chief ls` is for, and having the bare word
    /// answer a question about the BOX made the one command an operator types
    /// most often the only one that was not about the directory they were
    /// standing in.
    Bare,
    /// `chief ls`.
    List,
    /// `chief attach` — this directory's company.
    Attach,
    /// `chief stop` — this directory's company.
    Stop,
    /// `chief stand-down [reason]` — stop every person and KEEP them stopped.
    ///
    /// Not `stop`. `stop` takes the runtime down: the panes, the actuator and
    /// the daemon all go, and there is nobody left to talk to. A stand-down
    /// leaves the company attached and the CEO running, and stops everybody
    /// else until an explicit [`Command::Resume`].
    ///
    /// It exists because obeying was not enough. An operator told a live
    /// company to stop all work; the CEO obeyed exactly, parked six people and
    /// said so, and forty-five seconds later all six were back — their queued
    /// mail re-granted them. See `chiefd_core::store::stand_down`.
    StandDown {
        /// What the operator said about it, joined from the remaining argv.
        /// Empty when they said nothing; it is shown back in every refusal.
        reason: String,
    },
    /// `chief resume` — let this company work again.
    Resume,
    /// `chief rm [--yes]` — make this directory's company stop existing.
    ///
    /// The only verb that removes a beacond row. `stop` is not it: a stopped
    /// company keeps its row, keeps every byte of its durable state, and comes
    /// back with `attach`.
    Remove {
        /// Whether the one confirmation was answered up front.
        yes: bool,
    },
    /// `chief topology` — where this client would place everybody here.
    ///
    /// Computes only; chiefd still actuates. Holding the two answers up
    /// against each other is exactly what this verb is for during the
    /// transition (P5), and it goes away with the switchover (P8).
    Topology,
    /// `chief actuate` — this directory's resident actuator (#751/P8).
    ///
    /// The one verb that does not return. chiefd publishes person-scoped
    /// runtime actions and this is what carries them out; a company with no
    /// attached client is **un-actuated**, which chiefd reports as a first-class
    /// state rather than as an error. Before P8 the daemon created a person's
    /// pane and the process was its child, so somebody was always able to start
    /// a person; afterwards the client creates the pane, and this is the client.
    Actuate,
    /// `chief upgrade [--check | --rollback] [--skip-pi-check]`.
    ///
    /// Answered BEFORE a tokio runtime exists, beside `help` and `--version`:
    /// it touches no company, needs no daemon, and must work on a box whose
    /// install is exactly what it is about to replace.
    Upgrade(upgrade::Mode),
    /// `chief reset [--yes]`.
    Reset {
        /// Whether the one confirmation was answered up front.
        yes: bool,
    },
    /// `chief sidebar` — the operator's left rail.
    ///
    /// A MODE, not an operator verb: it is spawned into a tmux pane by `chief
    /// attach` and never typed, so it is deliberately absent from
    /// [`OPERATOR_VERBS`] and therefore from the usage text — the same
    /// treatment [`Command::Host`] gets, for the same reason.
    ///
    /// It refuses to draw anywhere but the company's own operator session. That
    /// refusal is what makes its unconditionally-scoped operator credential
    /// sound; see `chief_cli::sidebar`'s module doc.
    Sidebar,
    /// Internal interactive card placed in the permanent focus body.
    /// The department overview card: one JSON payload, built by the brain from
    /// the roster it is already holding.
    ///
    /// ONE argument and not ten. The sleeping card takes eight positional
    /// strings because it describes one person with a fixed set of facts; a
    /// department describes a LIST whose length is the roster's, and a
    /// positional argv cannot carry one without inventing a separator and a
    /// parser for it. The payload is `department_card::Card`'s own serde shape,
    /// so the wire format and the thing it draws cannot drift apart.
    DepartmentCard {
        /// The serialized [`chief_cli::sidebar::department_card::Card`].
        payload: String,
    },
    SleepingPersonCard {
        person: String,
        name: String,
        role: String,
        model: chief_cli::actuate::launch_catalog::PersonModel,
        refusal: Option<String>,
        blocked: Option<String>,
    },
    /// Internal ordered callback for one tmux `client-resized` event.
    ViewportResize {
        socket: String,
        session: String,
        organization: String,
        client: String,
        event: String,
        nonce: String,
    },
    /// Silent synchronous eligibility probe for one exact tmux hook client.
    ViewportClientEligible { socket: String, session: String, client: String, nonce: String },
    /// Internal revocation callback for one tmux client session change.
    ViewportClientChanged { socket: String, client: String, nonce: String },
    /// Internal generation-fenced census for the native viewport fast path.
    ViewportClientCensus { socket: String, generation: String, nonce: String },
    /// Internal rebuild of one company's literal viewport manifest.
    ViewportManifestRefresh { socket: String, session: String, generation: String, nonce: String },
    /// Internal commit of one explicit sidebar-border drag.
    ViewportSidebarWidth {
        socket: String,
        session: String,
        organization: String,
        session_id: String,
        nonce: String,
        columns: String,
    },
    /// `chief bench click [<target>…]` — time a click to pixels.
    ///
    /// Stage 0 of that work. A MEASUREMENT MODE, not an operator
    /// verb: it drives synthetic clicks into a live session and moves the glass
    /// around, so it is deliberately absent from [`OPERATOR_VERBS`] and
    /// therefore from the usage text — the same treatment [`Command::Sidebar`]
    /// and [`Command::Host`] get.
    ///
    /// `bench` also names an employment state in this product ("benched"). The
    /// collision is tolerable precisely because this is not an advertised verb:
    /// nothing an operator can read offers them `chief bench`, and the only
    /// callers are this plan's own measurements.
    BenchClick {
        /// What to click, as `department:<text>`, `person:<text>` or
        /// `sleeper:<text>`. Empty prints what the rail is drawing instead of
        /// guessing.
        targets: Vec<String>,
        /// How many times to run the whole cycle of targets.
        rounds: usize,
    },
    /// `chief host` — the resident company-lifecycle surface `apps/api` calls.
    ///
    /// A mode, not an operator verb: it is spawned by `scripts/start-stack.ts`
    /// and never typed, so it is deliberately absent from [`OPERATOR_VERBS`]
    /// and therefore from the usage text.
    Host,
    /// A daemon mode, `exec`'d into `chiefd` with this exact argv.
    Daemon(Vec<String>),
}

/// The destination selected by bare `chief` after it reads the current
/// directory.
///
/// A company database is the complete decision. If it exists, bare `chief`
/// uses the same attach path as explicit `chief attach`; otherwise it opens
/// Founder mode. Keeping this as a production decision lets the business rule
/// be tested without starting tmux or a daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BareDoor {
    Company,
    Founder,
}

const fn bare_door(company_present: bool) -> BareDoor {
    if company_present {
        BareDoor::Company
    } else {
        BareDoor::Founder
    }
}

/// Every verb this binary ADVERTISES: the word, its argument spelling, and what
/// it does.
///
/// ONE table. [`route`] dispatches through the same words and [`usage`] renders
/// this, so a verb cannot appear in the help text without being routed and a
/// routed verb cannot be missing from the help text. It used to be a static
/// `USAGE` string maintained beside the routing by hand, and
/// `scripts/test/model-facing-copy.test.mjs` had to be built to assert the two
/// agreed — after nine invented `chiefd` commands shipped in model-facing copy.
/// Deriving is cheaper than guarding.
///
/// Daemon modes ([`DAEMON_VERBS`]) and `host` are deliberately absent: they are
/// spawned by `chief` and by the test harness, never typed by an operator, and
/// listing them would invite exactly the hand-running this program's placement
/// rules exist to prevent.
pub(crate) const OPERATOR_VERBS: [(&str, &str, &[&str]); 10] = [
    ("ls", "", &["every registered company and its state"]),
    (
        "attach",
        "",
        &["put this terminal in this directory's CEO,", "starting it first if it is stopped"],
    ),
    ("stop", "", &["stop this company's runtime, then its daemon"]),
    (
        "stand-down",
        " [reason]",
        &[
            "stop every person working and keep them stopped;",
            "the CEO stays up so you can still talk to it,",
            "and queued mail is held, not lost",
        ],
    ),
    ("resume", "", &["let this company work again after a stand-down"]),
    (
        "rm",
        " [--yes]",
        &["remove this company for good: stop it,", "delete its .chief/, drop it from discovery"],
    ),
    (
        "actuate",
        "",
        &[
            "run this company's people from this terminal;",
            "stays open, because a company with no",
            "attached client has nobody to start them",
        ],
    ),
    ("reset", " [--yes]", &["shed this company back to CEO-only,", "without deleting anything"]),
    ("topology", "", &["where this client would place every desired person"]),
    (
        "upgrade",
        " [--check|--rollback]",
        &[
            "install the latest release over this one;",
            "--check reports and changes nothing,",
            "--rollback returns to the previous version",
        ],
    ),
];

/// The modes this client hands to `chiefd` — the daemon — instead of answering.
///
/// The daemon owns them; this program only knows their names, so that an
/// operator, a script or a test harness that invokes `chief <mode>` reaches
/// the right program without having to know there are two.
/// `scripts/test/model-facing-copy.test.mjs` asserts this list against
/// `chiefd`'s own dispatch in both directions — a mode the daemon
/// answers but this list omits would be unreachable through the installed
/// name, and a name here the daemon does not answer would `exec` into a
/// refusal.
pub(crate) const DAEMON_VERBS: [&str; 4] =
    ["run", "bootstrap-store", "set-actuation-config", "clear-breaker"];

/// The one verb whose stdout is a USER INTERFACE rather than a transcript.
///
/// Named once, and read by both the router and the log install, so the two
/// cannot drift into a rail that routes but still prints over itself.
pub(crate) const SIDEBAR_VERB: &str = "sidebar";
const SLEEPING_PERSON_CARD_VERB: &str = "sleeping-person-card";
/// The department overview card, spawned into a department window's body by the
/// rail brain. Internal and not advertised, exactly like the sleeping card: it
/// is a surface the product puts up, never a verb an operator types.
const DEPARTMENT_CARD_VERB: &str = "department-card";
const VIEWPORT_RESIZE_VERB: &str = "viewport-resize";
const VIEWPORT_CLIENT_ELIGIBLE_VERB: &str = "viewport-client-eligible";
const VIEWPORT_CLIENT_CHANGED_VERB: &str = "viewport-client-changed";
const VIEWPORT_CLIENT_CENSUS_VERB: &str = "viewport-client-census";
const VIEWPORT_MANIFEST_REFRESH_VERB: &str = "viewport-manifest-refresh";
const VIEWPORT_SIDEBAR_WIDTH_VERB: &str = "viewport-sidebar-width";

/// The measurement mode Stage 0 of that work adds.
///
/// Named once, here, and read by the router and by [`parse_bench`].
pub(crate) const BENCH_VERB: &str = "bench";

/// The column the usage text's descriptions start in.
const DESCRIPTION_COLUMN: usize = 34;

/// Everything `chief` accepts, in the words an operator types.
///
/// # Why this is derived rather than written
///
/// `chief --help`, `-h` and `help` used to be handed to a Bun entry point that
/// serves exactly one internal command, with a literal `"chiefd"` pushed in
/// front of them as argv[0]. That entry point dutifully reported
/// `unknown command 'chiefd'` — the program's own name, refused by the program
/// — after paying a whole Bun start-up to do it. A binary that cannot say what
/// it does without starting a JavaScript runtime does not know what it does.
/// Having won the text back, the remaining way to lose it is for the list to
/// rot away from the routing, so the list is not a list.
#[must_use]
pub(crate) fn usage() -> String {
    let line = |left: &str, right: &str| format!("  {left:<DESCRIPTION_COLUMN$}{right}");
    let mut lines = vec![
        "ChiefD — durable, isolated Pi companies.".to_string(),
        String::new(),
        "Usage: chief [command]".to_string(),
        String::new(),
        line("chief", "open Founder here, or start this directory's company"),
    ];
    for (verb, arguments, descriptions) in OPERATOR_VERBS {
        let invocation = format!("chief {verb}{arguments}");
        for (index, description) in descriptions.iter().enumerate() {
            lines.push(line(if index == 0 { invocation.as_str() } else { "" }, description));
        }
    }
    lines.push(line("chief help", "this text"));
    lines.push(line("chief --version", "the installed version"));
    lines.push(String::new());
    lines.push(
        "--yes answers the confirmation `reset` and `rm` ask. A caller with\n\
         no terminal to answer it is refused rather than assumed to have said yes, so any\n\
         script running either verb must pass --yes."
            .to_string(),
    );
    lines.join("\n")
}

/// Why an argv could not be routed. Usage errors are refusals, never defaults:
/// a missing or extra positional and an unknown flag all fail closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RouteError {
    /// A flag this verb does not accept.
    #[error("chief {verb} does not support flag '{flag}'")]
    UnknownFlag {
        /// The verb that refused.
        verb: &'static str,
        /// The offending flag as typed.
        flag: String,
    },
    /// More than one `<company>` positional.
    #[error("chief {verb} requires exactly one <company> argument; received extra '{extra}'")]
    ExtraCompany {
        /// The verb that refused.
        verb: &'static str,
        /// The second positional as typed.
        extra: String,
    },
    /// A verb `chief` does not have.
    ///
    /// Refused HERE, by the binary the operator invoked. It used to fall
    /// through to a scaffold message about an unimplemented socket server, and
    /// six verbs were handed to a Bun entry point that could only answer
    /// `unknown command '<verb>'` itself — an unknown command reported by a
    /// second program, after a runtime start, is a worse answer than no answer.
    /// The daemon split does not reopen that door: a daemon mode is `exec`'d
    /// only when it is a mode this binary knows by name, and everything else
    /// stops here.
    #[error("chief: unknown command '{command}'\n\n{}", usage())]
    UnknownCommand {
        /// The verb as typed.
        command: String,
    },
}

/// Parse one directory-scoped verb's optional confirmation flag.
///
/// `--yes` is the only accepted flag when `accepts_yes` is true, and it is the only way a
/// non-interactive caller may answer a confirmation at all (see
/// [`confirm::confirm_or_refuse`]).
fn parse_yes(
    verb: &'static str,
    args: &[String],
    accepts_yes: bool,
) -> std::result::Result<bool, RouteError> {
    let mut yes = false;
    for argument in args {
        if accepts_yes && argument == "--yes" {
            yes = true;
        } else if argument.starts_with("--") {
            return Err(RouteError::UnknownFlag { verb, flag: argument.clone() });
        } else {
            // A POSITIONAL IS NOW A MISTAKE, and naming it as one is the point.
            // Every one of these verbs took a company slug until the company
            // became the directory, so the argument an operator is most likely
            // to type is the one that no longer means anything. Refusing it by
            // name — rather than ignoring it, or worse, acting on the cwd while
            // the operator believes they named another company — is what makes
            // the change safe to type into muscle memory.
            return Err(RouteError::ExtraCompany { verb, extra: argument.clone() });
        }
    }
    Ok(yes)
}

/// Parse `bench click <company> [--rounds N] [<target>…]`.
///
/// Fails closed exactly as every other verb does: an unknown sub-verb or flag
/// is a refusal, never a default.
fn parse_bench(args: &[String]) -> std::result::Result<Command, RouteError> {
    let mut rest = args.iter();
    match rest.next().map(String::as_str) {
        Some("click") => {}
        Some(other) => {
            return Err(RouteError::UnknownFlag { verb: BENCH_VERB, flag: other.to_owned() })
        }
        None => {
            return Err(RouteError::UnknownFlag {
                verb: BENCH_VERB,
                flag: "bench takes one sub-verb: click".to_owned(),
            })
        }
    }
    let mut targets = Vec::new();
    let mut rounds = 1_usize;
    let mut remaining = rest.peekable();
    while let Some(argument) = remaining.next() {
        if let Some(value) = argument.strip_prefix("--rounds=") {
            rounds = value.parse().map_err(|_| RouteError::UnknownFlag {
                verb: BENCH_VERB,
                flag: argument.clone(),
            })?;
        } else if argument == "--rounds" {
            let value = remaining.next().ok_or_else(|| RouteError::UnknownFlag {
                verb: BENCH_VERB,
                flag: argument.clone(),
            })?;
            rounds = value
                .parse()
                .map_err(|_| RouteError::UnknownFlag { verb: BENCH_VERB, flag: value.clone() })?;
        } else if argument.starts_with("--") {
            return Err(RouteError::UnknownFlag { verb: BENCH_VERB, flag: argument.clone() });
        } else {
            targets.push(argument.clone());
        }
    }
    if rounds == 0 {
        return Err(RouteError::UnknownFlag {
            verb: BENCH_VERB,
            flag: "--rounds 0 measures nothing".to_owned(),
        });
    }
    Ok(Command::BenchClick { targets, rounds })
}

/// Route an operator argv.
///
/// Fails closed: an argument this binary does not understand is a refusal, not
/// a fallthrough to some default behaviour.
///
/// # Errors
/// [`RouteError`] for any usage the verb refuses.
pub(crate) fn route(args: &[String]) -> std::result::Result<Command, RouteError> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(Command::Bare);
    };
    // Claimed by this binary, before anything else looks at the argv. These
    // three used to be forwarded to a Bun entry point that answered
    // `unknown command 'chiefd'`.
    if matches!(verb, "help" | "--help" | "-h") {
        return Ok(Command::Help);
    }
    if matches!(verb, "--version" | "-V") {
        return Ok(Command::Version);
    }
    // A daemon mode: this program does not answer it and does not pretend to.
    // The whole argv goes across, so `chief run --dir .` and `chiefd run --dir .`
    // are the same invocation.
    if DAEMON_VERBS.contains(&verb) {
        return Ok(Command::Daemon(args.to_vec()));
    }
    let rest = &args[1..];
    // `chief attach --help` used to be refused as an unsupported flag. Help
    // beats a usage error for every verb that has one.
    if rest.iter().any(|argument| argument == "--help" || argument == "-h")
        && OPERATOR_VERBS.iter().any(|(known, _, _)| *known == verb)
    {
        return Ok(Command::Help);
    }
    match verb {
        "ls" => {
            if let Some(extra) = rest.first() {
                return Err(RouteError::UnknownFlag { verb: "ls", flag: extra.clone() });
            }
            Ok(Command::List)
        }
        "attach" => {
            parse_yes("attach", rest, false)?;
            Ok(Command::Attach)
        }
        "stop" => {
            parse_yes("stop", rest, false)?;
            Ok(Command::Stop)
        }
        // The reason is FREE TEXT, so the flag parse is deliberately skipped:
        // an operator writing `chief stand-down --the customer called` is
        // saying something, not passing a flag this program should refuse.
        "stand-down" => Ok(Command::StandDown { reason: rest.join(" ").trim().to_owned() }),
        "resume" => {
            parse_yes("resume", rest, false)?;
            Ok(Command::Resume)
        }
        "rm" => Ok(Command::Remove { yes: parse_yes("rm", rest, true)? }),
        "reset" => Ok(Command::Reset { yes: parse_yes("reset", rest, true)? }),
        "topology" => {
            parse_yes("topology", rest, false)?;
            Ok(Command::Topology)
        }
        // Its own flags, not `--yes`: `--check` and `--rollback` are different
        // verbs rather than a confirmation, and the one prompt this verb has
        // (Pi's updater) is asked through `confirm::decide`, which fails closed
        // on its own.
        "upgrade" => upgrade::parse_mode(rest)
            .map(Command::Upgrade)
            .map_err(|flag| RouteError::UnknownFlag { verb: "upgrade", flag }),
        // No `--yes`: it asks nothing. A resident mode that prompted would hang
        // the first time a supervisor started it with no terminal.
        "actuate" => {
            parse_yes("actuate", rest, false)?;
            Ok(Command::Actuate)
        }
        // Spawned into a pane by `attach`, never typed. No `--yes`: it asks
        // nothing, and a rail that prompted would hang the pane it lives in.
        SIDEBAR_VERB => {
            parse_yes(SIDEBAR_VERB, rest, false)?;
            Ok(Command::Sidebar)
        }
        DEPARTMENT_CARD_VERB => {
            if rest.len() != 1 {
                return Err(RouteError::UnknownFlag {
                    verb: DEPARTMENT_CARD_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::DepartmentCard { payload: rest[0].clone() })
        }
        SLEEPING_PERSON_CARD_VERB => {
            if rest.len() != 8 {
                return Err(RouteError::UnknownFlag {
                    verb: SLEEPING_PERSON_CARD_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            let state = chief_cli::actuate::launch_catalog::PersonModelState::parse(&rest[3])
                .ok_or_else(|| RouteError::UnknownFlag {
                    verb: SLEEPING_PERSON_CARD_VERB,
                    flag: rest[3].clone(),
                })?;
            Ok(Command::SleepingPersonCard {
                person: rest[0].clone(),
                name: rest[1].clone(),
                role: rest[2].clone(),
                model: chief_cli::actuate::launch_catalog::PersonModel {
                    state,
                    provider: (!rest[4].is_empty()).then(|| rest[4].clone()),
                    model: (!rest[5].is_empty()).then(|| rest[5].clone()),
                },
                refusal: (!rest[6].is_empty()).then(|| rest[6].clone()),
                // CHIEFD'S LAUNCH GATE, in its own words. Carried apart from
                // `refusal` because the two differ in the only way the
                // operator can act on: a wake refusal is one attempt's answer
                // and can be asked again, a gate refusal is re-derived against
                // the disk every pass and cannot.
                blocked: (!rest[7].is_empty()).then(|| rest[7].clone()),
            })
        }
        VIEWPORT_RESIZE_VERB => {
            if rest.len() != 6 {
                return Err(RouteError::UnknownFlag {
                    verb: VIEWPORT_RESIZE_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::ViewportResize {
                socket: rest[0].clone(),
                session: rest[1].clone(),
                organization: rest[2].clone(),
                client: rest[3].clone(),
                event: rest[4].clone(),
                nonce: rest[5].clone(),
            })
        }
        VIEWPORT_CLIENT_ELIGIBLE_VERB => {
            if rest.len() != 4 {
                return Err(RouteError::UnknownFlag {
                    verb: VIEWPORT_CLIENT_ELIGIBLE_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::ViewportClientEligible {
                socket: rest[0].clone(),
                session: rest[1].clone(),
                client: rest[2].clone(),
                nonce: rest[3].clone(),
            })
        }
        VIEWPORT_CLIENT_CHANGED_VERB => {
            if rest.len() != 3 {
                return Err(RouteError::UnknownFlag {
                    verb: VIEWPORT_CLIENT_CHANGED_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::ViewportClientChanged {
                socket: rest[0].clone(),
                client: rest[1].clone(),
                nonce: rest[2].clone(),
            })
        }
        VIEWPORT_CLIENT_CENSUS_VERB => {
            if rest.len() != 3 {
                return Err(RouteError::UnknownFlag {
                    verb: VIEWPORT_CLIENT_CENSUS_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::ViewportClientCensus {
                socket: rest[0].clone(),
                generation: rest[1].clone(),
                nonce: rest[2].clone(),
            })
        }
        VIEWPORT_MANIFEST_REFRESH_VERB => {
            if rest.len() != 4 {
                return Err(RouteError::UnknownFlag {
                    verb: VIEWPORT_MANIFEST_REFRESH_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::ViewportManifestRefresh {
                socket: rest[0].clone(),
                session: rest[1].clone(),
                nonce: rest[2].clone(),
                generation: rest[3].clone(),
            })
        }
        VIEWPORT_SIDEBAR_WIDTH_VERB => {
            if rest.len() != 6 {
                return Err(RouteError::UnknownFlag {
                    verb: VIEWPORT_SIDEBAR_WIDTH_VERB,
                    flag: rest.first().cloned().unwrap_or_default(),
                });
            }
            Ok(Command::ViewportSidebarWidth {
                socket: rest[0].clone(),
                session: rest[1].clone(),
                organization: rest[2].clone(),
                session_id: rest[3].clone(),
                nonce: rest[4].clone(),
                columns: rest[5].clone(),
            })
        }
        // A measurement mode, never typed by an operator and absent from the
        // usage text. `bench` takes ONE sub-verb, `click`, because Stage 0 has
        // exactly one thing to measure and a `bench` that dispatched over an
        // open set would be the speculative configuration AGENTS.md forbids.
        BENCH_VERB => parse_bench(rest),
        "host" => Ok(Command::Host),
        _ => Err(RouteError::UnknownCommand { command: verb.to_string() }),
    }
}

/// Replace this process with `chiefd`, carrying the argv across intact.
///
/// `exec`, never spawn-and-wait. The pid, the process group and every signal
/// disposition survive, so a harness that spawns `chief run` and
/// later kills that pid still kills the daemon, and a `chiefd run` under a
/// supervisor is still the process the supervisor is watching. A wrapper
/// process would break both, silently, in exactly the cases nobody tests.
///
/// The daemon is resolved as a SIBLING of this executable, not through `PATH`
/// and not through `~/.chief/bin` — a `chief` from a cargo target directory
/// must reach the `chiefd` built beside it, and an installed `chief`
/// must reach the installed one. A `PATH` lookup would let either pick up the
/// other's.
fn exec_daemon(args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let Ok(executable) = std::env::current_exe() else {
        eprintln!("chief: cannot locate its own executable, so it cannot find {DAEMON_PROGRAM}");
        return ExitCode::FAILURE;
    };
    let program = paths::chiefd_daemon_binary(&executable);
    // `exec` only ever RETURNS an error; on success this process is gone.
    let error = std::process::Command::new(&program).args(args).exec();
    eprintln!(
        "chief: could not run {} ({error}).\n\
         `chief {}` is served by the {DAEMON_PROGRAM} binary, which is installed beside this one.\n\
         Install both with: bun run release",
        program.display(),
        args.first().map_or("", String::as_str),
    );
    ExitCode::FAILURE
}

/// `chief topology` — this directory's company.
///
/// Reads the roster facts and prints where THIS client would place every
/// desired person. It computes only — chiefd still actuates, and holding the
/// two answers up against each other is exactly what this verb is for during
/// the transition.
///
/// # Errors
/// [`LifecycleError`] when the daemon or the roster cannot be read.
async fn run_topology(dir: &std::path::Path) -> Result<()> {
    require_a_company_here(dir, "chief topology")?;
    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer.
    let client = http::Client::operator(dir);
    let running = daemon::resolve_running(&client, dir).await.ok_or_else(|| {
        LifecycleError::unreachable(format!(
            "chief topology: the company in {} has no running chiefd to ask. `chief attach` \
             starts one.",
            dir.display()
        ))
    })?;
    let key = paths::company_key(dir);
    let roster = company::CompanyClient::new(&client, &running.url, dir, &key).roster().await?;
    // The DESIRED SET decides membership, exactly as it does in the resident
    // actuator. Reading it here rather than filtering the roster's
    // `desiredActive` is the point of this verb: what it prints has to be what
    // the actuator would actually do, and the actuator reads this route.
    //
    // `topology` is a verb an OPERATOR typed, so it reads as the OPERATOR and
    // never as the actuator. The two identities exist precisely so a record can
    // tell a deliberate look from an automatic one.
    let bearer = std::sync::Arc::new(Bearer::operator(&paths::keys_dir(dir)));
    let actuation = ActuationClient::new(&running.url, &key, bearer);
    let desired = actuation.desired().await.map_err(|error| {
        LifecycleError::unreachable(format!(
            "chief topology: the desired set for {} could not be read: {error}",
            dir.display()
        ))
    })?;
    // The SESSION NAME is composed once, here, and handed down — see
    // `placement::desired_topology`'s own note: its two hottest callers already
    // hold the session they are drawing into, and re-deriving it there would
    // hash a path on every click.
    let session = company::conventional_session_name(&roster.company.slug, &key);
    let topology =
        placement::desired_topology(&roster, &desired.hashes(), &session).map_err(|error| {
            LifecycleError::refused(format!(
                "chief topology: the roster for {} is unusable: {error}",
                dir.display()
            ))
        })?;
    for line in placement::render(&topology) {
        println!("{line}");
    }
    Ok(())
}

/// `chief actuate <company>` — become this company's actuator and stay.
///
/// # Why this verb does not return
///
/// chiefd decides WHO runs; after #751/P8 the client is what makes it so. A
/// person's Pi process is a child of a pane THIS process creates, so a company
/// with no attached client has nobody to start its people — and chiefd CANNOT
/// SEE THAT. It used to report `presence: "never-attached" | "lapsed"` with
/// `withheld: "no-actuator"`, derived from a lease the actuator renewed by
/// reporting; there is no report, so there is no lease and no presence. That is
/// a named, accepted loss. Starting a company
/// is still two facts, not one: chiefd wants the people up, *and* somebody is
/// running this verb.
///
/// # What is decided here, and what is decided in the library
///
/// Everything in this function is discovery: which daemon serves this company,
/// which composite key addresses it, which tmux socket and session it owns, and
/// what this actuator calls itself. The loop, the report, the plan, the wait and
/// the renewal are [`chief_cli::actuate::resident`]'s, and they are exercised
/// against a scripted wire rather than against a live daemon.
///
/// # Errors
/// [`LifecycleError`] when discovery fails, when the company has no running
/// daemon to actuate against, or when chiefd permanently refuses this actuator
/// — a 404 for a company this daemon does not serve, or a 422 for a body this
/// client should never have built. A transport failure is none of those: the
/// loop survives a daemon restart rather than leaving a company un-actuated.
/// Which half of `chief actuate` this process is.
///
/// The pane's command is unchanged — `<abs chief> actuate`, exactly what
/// `attach` has always spawned — and the SUPERVISOR is what that command now
/// runs. It re-spawns the same binary with `CHIEF_ACTUATOR_ATTEMPT` set, and
/// the process that sees the marker runs the actuator body this function has
/// always run.
///
/// A marker rather than a verb: a child verb would be typeable by an operator
/// and would show up in `chief help`. Presence is the whole signal; the value
/// only decorates the banner, so an unreadable one still means "child".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActuateRole {
    /// Spawn and watch a child, for ever.
    Supervisor,
    /// Be the actuator. `attempt` is 1 for the first child of a supervisor.
    Child {
        /// Which restart this is.
        attempt: u32,
    },
}

/// The role this process plays, read from the environment.
fn actuate_role() -> ActuateRole {
    actuate_role_from(std::env::var(supervise::ATTEMPT_ENV).ok().as_deref())
}

/// Pure, so the dispatch can be pinned without an environment.
fn actuate_role_from(marker: Option<&str>) -> ActuateRole {
    match marker {
        None => ActuateRole::Supervisor,
        Some(raw) => ActuateRole::Child { attempt: raw.trim().parse().unwrap_or(1) },
    }
}

/// The program a supervisor re-spawns.
///
/// `argv[0]`, which `attach` already makes absolute — deliberately NOT
/// `current_exe()`, which on Linux reads `<path> (deleted)` after an in-place
/// binary replacement and would then fail every spawn. A relative argv[0] is
/// left alone: the child inherits this process's working directory, so it
/// resolves identically, and a bare name resolves on PATH as it did when the
/// operator typed it.
fn supervisor_program() -> std::path::PathBuf {
    std::env::args_os()
        .next()
        .map_or_else(|| std::path::PathBuf::from("chief"), std::path::PathBuf::from)
}

/// The line the actuator prints when it starts.
///
/// Pure so the restart suffix can be pinned. The first child of a supervisor
/// says nothing extra — an operator watching a healthy company should not have
/// to read the word "restart" — and every one after it names its number.
fn actuating_banner(dir: &std::path::Path, identity: &str, attempt: u32) -> String {
    let restart = if attempt > 1 { format!(" (restart #{attempt})") } else { String::new() };
    format!(
        "{}: actuating as {identity}{restart}. This process must stay running — chiefd decides \
         who runs, and this is what runs them.",
        dir.display()
    )
}

async fn run_actuate(dir: &std::path::Path) -> Result<()> {
    use chief_cli::actuate::host::Socket;
    use chief_cli::actuate::resident;
    use chief_cli::real::RealHostExecutor;

    // THE SUPERVISOR FORK. A bare `chief actuate` supervises; the marked child
    // is the actuator. Everything below this block is unchanged and runs only
    // in the child.
    let attempt = match actuate_role() {
        ActuateRole::Supervisor => {
            let program = supervisor_program();
            let company = dir.display().to_string();
            // Async because the SIGNALS are: this crate is
            // `#![forbid(unsafe_code)]`, so a raw `sigaction` handler is not
            // available and `tokio::signal` is what can watch for SIGHUP.
            // The child itself is waited for on a blocking thread inside.
            let code = supervise::run(&program, &company, supervise::Schedule::production()).await;
            // The supervisor exits with the CHILD's status, which tmux shows in
            // the corpse line (`Pane is dead (status N)`). The runtime's own
            // `Result` mapping has only success and failure, so the status is
            // handed to the OS here rather than flattened to 1. Safe because
            // this process holds nothing but stdio, which is flushed first.
            use std::io::Write as _;
            std::io::stdout().flush().ok();
            std::io::stderr().flush().ok();
            std::process::exit(i32::from(code));
        }
        ActuateRole::Child { attempt } => attempt,
    };

    // BEFORE anything is occupied: this process spawns every person in the
    // company, so a host that cannot run Pi cannot run anybody. Left unasked,
    // that host does not refuse — it mints a pane whose command dies instantly,
    // tmux reaps the empty window, and the next step fails against a window
    // that no longer exists with a message about window dimensions. Then it
    // does it again, once per second, for as long as the process lives. See
    // `preflight::Surface::ResidentActuator`.
    preflight::require_ready_to_actuate()?;
    require_a_company_here(dir, "chief actuate")?;
    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer.
    let client = http::Client::operator(dir);
    let running = daemon::resolve_running(&client, dir).await.ok_or_else(|| {
        LifecycleError::unreachable(format!(
            "chief actuate: the company in {} has no running chiefd to actuate against. \
             `chief attach` starts one.",
            dir.display()
        ))
    })?;
    let key = paths::company_key(dir);

    // The SOCKET is read from the SAME daemon this actuator will report to —
    // never guessed, because a guess that disagrees with the recorded
    // ownership actuates a second, parallel copy of the company onto a
    // different tmux server. That record is the one durable rendezvous for a
    // handle this client chose (`boot_socket` tier 2: a company that ran on a
    // non-default socket comes back onto it).
    //
    // The SESSION is `org-<slug>-<key6>_`: the key from this directory and the
    // slug from the company's own manifest, which is the one place it is
    // written down.
    let company = company::CompanyClient::new(&client, &running.url, dir, &key);
    let socket = company::boot_socket_from_env(
        company.active_runtime_owner_socket().await?.as_deref(),
        &key,
    );
    let facts = company.facts().await?.ok_or_else(|| {
        LifecycleError::refused(format!(
            "chief actuate: the company in {} has a daemon but no manifest, so it has no name \
             and no session to actuate into.",
            dir.display()
        ))
    })?;
    let session = company::conventional_session_name(&facts.slug, &key);

    // WHICH BUILD IS ACTUATING, recorded on this actuator's own session.
    //
    // TAGS ARE THE LIVE RECORD (`.chief/` is not runtime placement, and nothing
    // on disk is). Every other fact about a running tmux object already lives
    // on the object — a pane's person, its launch hash, a window's department —
    // and this is the same kind of fact about the same kind of object: which
    // binary the process in that pane is running. So it is stamped here rather
    // than written to a file some later reader would have to trust.
    //
    // Measured at start, while this process's own executable certainly still
    // exists. Best effort: a tmux that will not take the option costs a build
    // check, never an actuator, and the reader treats an absent tag as
    // unknowable and leaves the actuator alone.
    if let Some(build) = host_primitives::rendezvous::ReportedBuild::of_running_process() {
        if let Ok(rendered) = serde_json::to_string(&build) {
            tmux::record_actuator_build(
                &socket,
                &attach::actuator_session_name(&session),
                &rendered,
            );
        }
    }

    // The per-person launch catalog — pi binary, pi-home, workspace, theme
    // files, granted tools, the session to resume, the pane environment — is
    // NOT supplied here, and deliberately not built here either. It is derived
    // by chiefd and served on `POST /v1/org/runtime/launch-catalog`. The
    // actuator fetches it from the same client, once per pass, so a person
    // materialized while this process is running becomes launchable without
    // restarting it.
    //
    // The resident actuator authenticates as `service`, NOT as the operator.
    // Its actions are automatic and the operator's are deliberate, and the
    // staffing routes record who acted — a distinction the record cannot make
    // if both arrive as one principal.
    let bearer = std::sync::Arc::new(Bearer::service(&paths::keys_dir(dir)));
    let actuation = ActuationClient::new(&running.url, &key, bearer);
    let identity = resident::actuator_id(&discovery::hostname());

    // THE SESSION BRAIN, STARTED BEFORE THE LOOP THAT FEEDS IT.
    //
    // This process is the company's one authority for what the operator is
    // looking at: it holds the `View`, hit-tests every click, performs the
    // gesture and pushes frames to the thin rail clients in every window. It
    // is here because it is the process that has to read the company anyway,
    // which is the whole of the design record Stage 3.
    //
    // Its own tmux is a second control client. That is one more than the
    // actuator had and N fewer than the session had, because every rail used to
    // own one.
    let brain = chief_cli::sidebar::brain::start(
        std::sync::Arc::new(chief_cli::control::ControlTransport::new(
            Socket(socket.clone()),
            session.clone(),
        )),
        std::sync::Arc::new(actuation.clone()),
        session.clone(),
        dir,
        &paths::rail_socket_path(dir),
    )
    .await
    .map_err(|error| {
        LifecycleError::refused(format!(
            "chief actuate: the company in {} could not open its sidebar socket ({error}); with \
             no brain there is no rail, so this refuses rather than actuating a company the \
             operator cannot see",
            dir.display()
        ))
    })?;
    let gestured = brain.nudge();

    let mut actuator = resident::TmuxActuator::new(
        actuation.clone(),
        // Control mode, not a process per command: a pass is many tmux
        // invocations and each spawn cost ~25 ms of fork/exec that tmux's own
        // work does not need. The session is the company session this
        // actuator already targets.
        Box::new(RealHostExecutor::control_mode(Socket(socket.clone()), session.clone())),
        Socket(socket),
        session,
        dir.to_path_buf(),
        brain,
    );

    println!("{}", actuating_banner(dir, &identity, attempt));
    let label = dir.display().to_string();
    resident::run(
        &actuation,
        &mut actuator,
        &label,
        &identity,
        resident::Schedule::default(),
        &gestured,
    )
    .await
    .map_err(|error| {
        LifecycleError::refused(format!(
            "chief actuate: the company in {} can no longer be actuated by this client: {error}",
            dir.display()
        ))
    })
}

/// `chief sidebar <company>` — blit this session's rail into this pane.
///
/// Spawned by [`attach`] into a pane it has already split and tagged; it is not
/// a verb an operator types, and it is absent from the usage text for the same
/// reason `host` is.
///
/// # It reads nothing, and that is the whole of Stage 3 from this side
///
/// This verb used to build an authenticated chiefd client, read the roster, the
/// desired set, the lifecycle board and the launch catalog on every changefeed
/// wake, and hold its own `View`, its own selection, its own placement copy and
/// its own accent memo — one such process PER WINDOW. All of it moved into the
/// one process that already had to read the company
/// ([`chief_cli::sidebar::brain`]). What is left connects to a unix socket,
/// forwards raw stdin bytes and writes the frames that come back.
///
/// So there is no credential here any more. The fence that made the operator
/// bearer sound — [`chief_cli::sidebar::for_session`], which refuses every
/// session but this company's own — is still the fence, and it is still checked
/// before anything is drawn.
///
/// # Errors
/// [`LifecycleError`] when the terminal cannot be taken, when this process is
/// not in a tmux pane, or when the pane is not in this company's own session.
async fn run_sidebar(dir: &std::path::Path) -> Result<()> {
    use chief_cli::sidebar::client::Glass;

    // THE GLASS IS TAKEN BEFORE ANYTHING ELSE, and this ordering is the whole
    // of the fix for "when I click on Department, it flashes white".
    //
    // Until this line the pane is blank and its pty is in canonical mode with
    // echo on. Blank is the white the operator photographed. The work that used
    // to follow it — discovery, a beacond health wait, an authenticated company
    // round trip, a key read off disk — is DELETED rather than reordered, so the
    // pane is painted within a millisecond of the process starting and its
    // first real frame arrives one socket round trip later.
    let glass = Glass::take().map_err(|error| {
        LifecycleError::refused(format!("chief: the sidebar could not take its terminal: {error}"))
    })?;

    let outcome = run_sidebar_in_pane(dir).await;
    // The glass goes back before anything is printed: it owns the alternate
    // screen, and a refusal written onto it would vanish with it.
    drop(glass);
    outcome
}

fn run_viewport_resize(
    socket: &str,
    session: &str,
    organization: &str,
    client: &str,
    event: &str,
    nonce: &str,
) -> Result<()> {
    if !session.starts_with("org-") || !session.ends_with('_') {
        return Err(LifecycleError::refused(
            "viewport resize refused outside a tagged company session".to_owned(),
        ));
    }
    #[cfg(debug_assertions)]
    let mut test_barrier = if let Ok(path) = std::env::var("CHIEF_TEST_VIEWPORT_BARRIER") {
        use std::io::{Read as _, Write as _};

        let mut barrier = std::os::unix::net::UnixStream::connect(&path).map_err(|error| {
            LifecycleError::host(format!("viewport test barrier {path} could not connect: {error}"))
        })?;
        barrier.write_all(b"entered").map_err(|error| {
            LifecycleError::host(format!("viewport test barrier could not announce entry: {error}"))
        })?;
        let mut release = [0_u8; 1];
        barrier.read_exact(&mut release).map_err(|error| {
            LifecycleError::host(format!("viewport test barrier was not released: {error}"))
        })?;
        Some(barrier)
    } else {
        None
    };
    let executor = chief_cli::real::RealHostExecutor::production();
    let result = chief_cli::actuate::resize_session_viewport_for_client(
        &executor,
        &chief_cli::actuate::Socket(socket.to_owned()),
        session,
        organization,
        client,
        event,
        nonce,
    );
    #[cfg(debug_assertions)]
    if let Some(barrier) = test_barrier.as_mut() {
        use std::io::Write as _;

        barrier.write_all(b"complete").map_err(|error| {
            LifecycleError::host(format!("viewport test completion could not be sent: {error}"))
        })?;
    }
    result.map(|_| ()).map_err(LifecycleError::host)
}

fn run_viewport_client_eligible(socket: &str, session: &str, client: &str, nonce: &str) -> bool {
    if !session.starts_with("org-") || !session.ends_with('_') {
        return false;
    }
    let executor = chief_cli::real::RealHostExecutor::production();
    let eligible = chief_cli::actuate::viewport_client_is_eligible(
        &executor,
        &chief_cli::actuate::Socket(socket.to_owned()),
        session,
        client,
        nonce,
    )
    .unwrap_or(false);
    #[cfg(debug_assertions)]
    if eligible {
        if let Ok(path) = std::env::var("CHIEF_TEST_VIEWPORT_ELIGIBILITY_BARRIER") {
            use std::io::{Read as _, Write as _};

            let Ok(mut barrier) = std::os::unix::net::UnixStream::connect(path) else {
                return false;
            };
            if barrier.write_all(b"entered").is_err() {
                return false;
            }
            let mut release = [0_u8; 1];
            if barrier.read_exact(&mut release).is_err() {
                return false;
            }
        }
    }
    eligible
}

fn run_viewport_client_changed(socket: &str, client: &str, nonce: &str) -> Result<()> {
    let executor = chief_cli::real::RealHostExecutor::production();
    chief_cli::actuate::revoke_client_viewport_tokens_for_client(
        &executor,
        &chief_cli::actuate::Socket(socket.to_owned()),
        client,
        nonce,
    )
    .map(|_| ())
    .map_err(LifecycleError::host)
}

fn run_viewport_client_census(socket: &str, generation: &str, nonce: &str) -> Result<()> {
    let executor = chief_cli::real::RealHostExecutor::production();
    chief_cli::actuate::refresh_single_ordinary_viewport_session(
        &executor,
        &chief_cli::actuate::Socket(socket.to_owned()),
        generation,
        nonce,
    )
    .map_err(LifecycleError::host)
}

/// The rail, once its pane's terminal is held.
///
/// Split out so [`run_sidebar`] has exactly one place to give the glass back,
/// however this refuses.
async fn run_sidebar_in_pane(dir: &std::path::Path) -> Result<()> {
    let pane = std::env::var("TMUX_PANE").map_err(|_| {
        LifecycleError::refused(
            "chief: the sidebar draws inside a tmux pane and this process is not in one. \
             `chief attach` opens the company, and the rail comes with it."
                .to_owned(),
        )
    })?;
    // THE FENCE, UNCHANGED. The rail exists only in the operator's own company
    // session; see `chief_cli::sidebar`'s module doc for the disclosure rule
    // that rests on it. The socket this connects to is per company, so a client
    // in the wrong session could not reach another company's brain anyway —
    // this refuses out loud rather than resting on that.
    //
    // It asks the KEY and not the slug, which is what keeps this verb reading
    // NOTHING: the key is `sha256(this directory)`, and a slug would have meant
    // a company round trip before the first frame — the boot Stage 3 exists to
    // win back. See `sidebar::for_session`.
    let socket = company::pane_socket_from_env(&paths::company_key(dir));
    let session = tmux::session_of_pane(&socket, &pane)?;
    chief_cli::sidebar::for_session(&session, dir)
        .map_err(|refusal| LifecycleError::refused(format!("chief: {refusal}")))?;

    chief_cli::sidebar::client::run(&paths::rail_socket_path(dir))
        .await
        .map_err(|error| LifecycleError::refused(format!("chief: {error}")))
}

/// `chief bench click <company> [<target>…]` — time a click to the pixels it
/// produces.
///
/// # What this measures, stated plainly, because three claims went wrong here
///
/// The clock starts the instant before the SGR mouse bytes are handed to tmux,
/// and stops on a READ OF TMUX'S GRID for the session's ACTIVE window — the
/// only thing an attached terminal is ever sent. Two stops are reported per
/// click: `first-change`, when the glass differs at all, and `visible`, when it
/// is CORRECT for the gesture and has painted cells in it. Nothing here is
/// stopped by a command returning, which is the error that produced "department
/// click to layout: 1-37ms" for a gesture measured honestly at 5,636ms.
///
/// # It needs no daemon, on purpose
///
/// Nothing here reads a company daemon — not for the store's weight, which is
/// a file in this directory, and not for the session name, which is FOUND on
/// the tmux server by this company's key rather than composed from a slug the
/// store holds. So the harness runs against a company whose chiefd is slow,
/// wedged or dead, which is exactly the condition Stage 1's proof requires ("a
/// click completes in <50ms against a daemon that never answers"). tmux is the
/// only live dependency.
///
/// # Errors
/// [`LifecycleError`] when this directory holds no company, when it has no
/// session on this socket, when its session has no rail to click, or when tmux
/// will not answer.
async fn run_bench_click(dir: &std::path::Path, targets: &[String], rounds: usize) -> Result<()> {
    use chief_cli::actuate::host::Socket;
    use chief_cli::bench::click::{measure, read_glass, row_of, Target};
    use chief_cli::bench::{Samples, Weight};

    require_a_company_here(dir, "chief bench click")?;
    let weight = Weight::of(&paths::store_db_path(dir));

    let key = paths::company_key(dir);
    let socket = company::pane_socket_from_env(&key);
    let session = tmux::session_for_key(&socket, &key).ok_or_else(|| {
        LifecycleError::refused(format!(
            "chief bench click: no tmux session on socket '{socket}' belongs to the company in \
             {}. `chief attach` opens it.",
            dir.display()
        ))
    })?;
    let transport =
        chief_cli::control::ControlTransport::new(Socket(socket.clone()), session.clone());
    let socket = Socket(socket);

    let glass = read_glass(&transport, &socket, &session)
        .map_err(|error| LifecycleError::refused(error.to_string()))?;
    let Some(rail) = glass.rail() else {
        return Err(LifecycleError::refused(format!(
            "chief bench click: the active window of '{session}' has no sidebar pane, so there \
             is no rail to click. `chief attach` opens the company with one."
        )));
    };
    // THE COMPANY AS THE BRAIN IS DRAWING IT, asked over the session socket.
    // It costs no chiefd read and it is what turns a row the operator can SEE
    // ("Executive (1)") into the id the glass is checked against (`executive`)
    // — two different questions that must not be answered by the same string.
    let company = ask_the_brain(&paths::rail_socket_path(dir)).await;

    // NO TARGETS IS NOT AN EMPTY RUN. The rail is drawn from a company this
    // process cannot see, so the honest answer to "measure something" with
    // nothing named is to show what there IS to click, and what each row is
    // called underneath.
    if targets.is_empty() {
        println!("{}", weight.line());
        println!(
            "\nchief bench click <target>…  where each target is one of\n  \
             department:<row text>   person:<row text>   sleeper:<row text>\n\
             and <row text> is any part of a row the rail is drawing. Append =<id> when the \
             row's text is not the id.\n\nThe rail, as the operator sees it:\n"
        );
        for (row, line) in rail.text.lines().enumerate() {
            println!("  {row:>3}  {line}");
        }
        match &company {
            Some(company) => {
                println!("\nDepartments (row text -> id):");
                for (id, name) in &company.departments {
                    println!("  {name:<28} {id}");
                }
                println!("\nPeople (row text -> id):");
                for (id, name) in &company.people {
                    println!("  {name:<28} {id}");
                }
            }
            None => println!(
                "\nThis session's brain did not answer, so ids cannot be resolved from a \
                 row's text. Name each target's id explicitly: department:Quant=quant"
            ),
        }
        return Ok(());
    }

    let parsed = targets
        .iter()
        .map(|target| {
            let (kind, rest) = target.split_once(':').ok_or_else(|| {
                LifecycleError::refused(format!(
                    "chief bench click: '{target}' is not a target. Use department:<text>, \
                     person:<text> or sleeper:<text>."
                ))
            })?;
            let (text, stated) = rest.split_once('=').map_or((rest, None), |(text, id)| {
                (text, (!id.trim().is_empty()).then(|| id.trim().to_owned()))
            });
            let row =
                row_of(rail, text).map_err(|error| LifecycleError::refused(error.to_string()))?;
            let id = match stated {
                Some(id) => id,
                // Resolved from the published company, by the SAME text that
                // found the row — so the thing that is clicked and the thing
                // that is checked for are provably the same entity.
                None => resolve_id(company.as_ref(), kind, text).ok_or_else(|| {
                    LifecycleError::refused(format!(
                        "chief bench click: '{text}' matches a row of the rail but no \
                         {kind} of the published company, so there is nothing to check the \
                         glass against. State the id: {kind}:{text}=<id>"
                    ))
                })?,
            };
            Ok((target.clone(), row, text.to_owned(), id, kind.to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;

    println!("{}", weight.line());
    println!("session: {session}   socket: {}   rounds: {rounds}", socket.0);
    let mut gathered: Vec<(String, Samples)> = Vec::new();
    for round in 0..rounds {
        for (label, first_row, text, id, kind) in &parsed {
            // RE-READ THE ROW EVERY TIME. A click changes the glass, and the
            // rail it changes to may be a DIFFERENT PROCESS drawing a
            // different scroll position; a row cached from the first pass
            // would click something else entirely by the third.
            let glass = read_glass(&transport, &socket, &session)
                .map_err(|error| LifecycleError::refused(error.to_string()))?;
            let row =
                glass.rail().map_or(Ok(*first_row), |rail| row_of(rail, text).map_err(|_| ()));
            let Ok(row) = row else {
                // The row scrolled out of view between rounds. Reported, not
                // silently skipped: a bench that quietly measured fewer
                // clicks than it says it did is the fixture defect again.
                println!("  {label}: the row is no longer drawn; round {round} is not counted");
                continue;
            };
            let target = match kind.as_str() {
                "department" => Target::Department(id.clone()),
                "person" => Target::Person(id.clone()),
                "sleeper" => Target::Person(id.clone()),
                other => {
                    return Err(LifecycleError::refused(format!(
                        "chief bench click: '{other}' is not a target kind. Use department, \
                         person or sleeper."
                    )))
                }
            };
            let outcome = measure(&transport, &socket, &session, row, &target, BENCH_BUDGET)
                .map_err(|error| LifecycleError::refused(error.to_string()))?;
            match gathered.iter_mut().find(|(known, _)| known == label) {
                Some((_, samples)) => samples.record(&outcome),
                None => {
                    let mut samples = Samples::default();
                    samples.record(&outcome);
                    gathered.push((label.clone(), samples));
                }
            }
        }
    }

    for (label, samples) in &gathered {
        println!("\n{label}");
        for line in samples.lines() {
            println!("{line}");
        }
    }
    println!(
        "\nvisible = tmux's grid for the ACTIVE window is correct for the gesture AND has \
         painted cells in it. Nothing here stops the clock on a command returning."
    );
    Ok(())
}

/// The id a rail row's text names, resolved from the company the actuator
/// published into this session's own tmux options.
///
/// The row text is what the OPERATOR can point at; the id is what the glass is
/// checked against. Keeping them separate is what stops a department click
/// being graded against a window it did not open.
fn resolve_id(
    company: Option<&chief_cli::sidebar::wire::Named>,
    kind: &str,
    text: &str,
) -> Option<String> {
    let company = company?;
    let rows = match kind {
        "department" => &company.departments,
        "person" | "sleeper" => &company.people,
        _ => return None,
    };
    rows.iter()
        .find(|(_, name)| text.contains(name.as_str()) || name.contains(text))
        .map(|(id, _)| id.clone())
}

/// Ask this session's brain what it is drawing.
///
/// One connection, one question, one answer — the brain is the authority on the
/// rail, and it is reachable with no daemon at all, which is the condition the
/// harness exists to measure under. `None` when there is no brain: the harness
/// then requires every target to state its own id.
async fn ask_the_brain(socket: &std::path::Path) -> Option<chief_cli::sidebar::wire::Named> {
    use chief_cli::sidebar::wire::{Frames, ToBrain, ToClient};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut stream = tokio::net::UnixStream::connect(socket).await.ok()?;
    stream.write_all(&ToBrain::Describe.encode()).await.ok()?;
    let mut frames = Frames::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        frames.feed(buffer.get(..count).unwrap_or_default());
        while let Ok(Some(message)) = frames.next_to_client() {
            if let ToClient::Company(named) = message {
                return Some(named);
            }
        }
    }
}

/// How long one click is given to become visible before it is recorded as
/// never having done so.
///
/// Sixty seconds because the honest measured maximum for a cold person click on
/// the operator's box was 64.6 seconds. A budget under that would silently
/// convert the worst gestures — the ones the plan exists to fix — into missing
/// samples.
const BENCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(70);

/// The one error type every operator verb returns.
///
/// Deliberately a single string-carrying variant per class rather than a deep
/// taxonomy: these messages are read by a human at a terminal, and the value
/// they carry is the recovery instruction, not a machine code.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LifecycleError {
    /// The host cannot run a company (tmux/Pi/release preflight).
    #[error("{0}")]
    Preflight(String),
    /// beacond, a daemon, or a company route could not be reached or answered.
    #[error("{0}")]
    Unreachable(String),
    /// The operator asked for something this state does not permit.
    #[error("{0}")]
    Refused(String),
    /// A host effect (spawn, tmux, directory creation) failed.
    #[error("{0}")]
    Host(String),
}

impl LifecycleError {
    /// A refusal carrying the operator's next move.
    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self::Refused(message.into())
    }

    /// A transport/answer failure against beacond or a chiefd.
    pub(crate) fn unreachable(message: impl Into<String>) -> Self {
        Self::Unreachable(message.into())
    }

    /// A host-effect failure.
    pub(crate) fn host(message: impl Into<String>) -> Self {
        Self::Host(message.into())
    }
}

/// The result every operator verb returns.
pub(crate) type Result<T> = std::result::Result<T, LifecycleError>;

/// Record a hidden viewport callback failure without writing it to tmux.
///
/// These commands are background maintenance, not operator commands. Their
/// callers cannot recover from a diagnostic on the company glass, and tmux
/// turns a nonzero `run-shell` result into a visible line of its own. The next
/// event retries from current authority, while the file log keeps the failure
/// available for diagnosis.
fn hide_viewport_callback_failure(verb: &'static str, result: Result<()>) -> Result<()> {
    if let Err(error) = result {
        tracing::warn!(
            event = "viewport.callback.failed",
            verb,
            detail = %error,
            "a hidden viewport callback failed; a later current event can retry it"
        );
    }
    Ok(())
}

/// Execute a routed command.
///
/// A multi-threaded runtime, deliberately, even though these are one-shot
/// operator invocations: two verbs park a whole thread on a blocking child —
/// Bare `chief` runs the Founder Pi in the foreground while its genesis
/// endpoint must keep answering, and `attach` hands the terminal to tmux. On a
/// current-thread runtime the genesis endpoint would never get a poll, and the
/// Founder's first company launch would hang against a listener that is bound
/// but not serving.
fn run(command: Command) -> ExitCode {
    // Answered before a runtime exists. They must work on a machine with no
    // beacond, no tmux and no installed checkout — the states an operator is
    // most likely to be in when they type them.
    match command {
        Command::Help => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("chief {}", env!("CHIEF_VERSION"));
            return ExitCode::SUCCESS;
        }
        // Before a runtime, for the same reason `--version` is: it replaces
        // this install and must work on a box with no beacond, no tmux and no
        // company. It is also the one verb whose exit code carries meaning
        // beyond success — `--check` exits 10 when an upgrade exists.
        Command::Upgrade(ref mode) => return upgrade::run(mode.clone()),
        // Also before a runtime: `exec` replaces this process, so building a
        // tokio runtime first would only be work thrown away.
        Command::Daemon(ref args) => return exec_daemon(args),
        // Its own multi-threaded runtime, its own graceful shutdown.
        Command::Host => return host::run(),
        Command::ViewportClientEligible { ref socket, ref session, ref client, ref nonce } => {
            return if run_viewport_client_eligible(socket, session, client, nonce) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        _ => {}
    }
    let runtime =
        match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("chief: could not start its async runtime: {error}");
                return ExitCode::FAILURE;
            }
        };
    let outcome = runtime.block_on(async move {
        match command {
            // Answered above, before the runtime was built.
            Command::Help
            | Command::Version
            | Command::Upgrade(_)
            | Command::Daemon(_)
            | Command::Host
            | Command::ViewportClientEligible { .. } => Ok(()),
            // THE FIRST-RUN CHECK, and the only place it decides anything.
            // The database file's existence IS "this directory has a
            // company": no marker, no registry, no second source of truth. A
            // `.chief/` a crashed genesis left half-built is not a company, so
            // that operator lands in Founder and can finish founding.
            Command::Bare => {
                let dir = paths::current_dir()?;
                match bare_door(paths::company_present(&dir)) {
                    BareDoor::Company => attach::run(&dir).await,
                    BareDoor::Founder => founder::run(&dir).await,
                }
            }
            Command::List => listing::run_list().await,
            // THE DIRECTORY IS THE COMPANY, so it is resolved ONCE, here,
            // and handed down. Every verb below used to take a slug and go
            // looking for it in a global registry; none of them can now
            // disagree about which company they are acting on, because none of
            // them chooses.
            Command::Attach => attach::run(&paths::current_dir()?).await,
            Command::Stop => stop::run(&paths::current_dir()?).await,
            Command::StandDown { ref reason } => {
                stand_down::run_stand_down(&paths::current_dir()?, reason).await
            }
            Command::Resume => stand_down::run_resume(&paths::current_dir()?).await,
            Command::Remove { yes } => remove::run(&paths::current_dir()?, yes).await,
            Command::Reset { yes } => reset::run(&paths::current_dir()?, yes).await,
            Command::Topology => run_topology(&paths::current_dir()?).await,
            Command::Actuate => run_actuate(&paths::current_dir()?).await,
            Command::Sidebar => run_sidebar(&paths::current_dir()?).await,
            Command::DepartmentCard { payload } => {
                // A payload this build cannot read is drawn as an EMPTY card
                // rather than refused. The card is a report living in a pane the
                // operator just clicked into: a process that exits leaves them
                // looking at a dead pane with no explanation, which is strictly
                // worse than a card that says the department is empty. The parse
                // failure is logged where an operator can grep for it.
                let card =
                    serde_json::from_str::<chief_cli::sidebar::department_card::Card>(&payload)
                        .unwrap_or_else(|error| {
                            tracing::error!(
                                event = "sidebar.department-card.unreadable",
                                %error,
                                "the department card was launched with a payload this build cannot \
                                 read; it draws an empty department rather than leaving a dead pane"
                            );
                            chief_cli::sidebar::department_card::Card::default()
                        });
                chief_cli::sidebar::department_card::run(&paths::current_dir()?, card).map_err(
                    |error| {
                        LifecycleError::refused(format!(
                            "chief: the department card could not take its terminal: {error}"
                        ))
                    },
                )
            }
            Command::SleepingPersonCard { person, name, role, model, refusal, blocked } => {
                chief_cli::sidebar::sleeping_card::run(
                    &paths::current_dir()?,
                    chief_cli::sidebar::sleeping_card::Card {
                        person_id: person,
                        name,
                        role,
                        model,
                        refusal,
                        blocked,
                    },
                )
                .map_err(|error| {
                    LifecycleError::refused(format!(
                        "chief: the sleeping-person card could not take its terminal: {error}"
                    ))
                })
            }
            Command::ViewportResize { socket, session, organization, client, event, nonce } => {
                hide_viewport_callback_failure(
                    VIEWPORT_RESIZE_VERB,
                    run_viewport_resize(&socket, &session, &organization, &client, &event, &nonce),
                )
            }
            Command::ViewportClientChanged { socket, client, nonce } => {
                hide_viewport_callback_failure(
                    VIEWPORT_CLIENT_CHANGED_VERB,
                    run_viewport_client_changed(&socket, &client, &nonce),
                )
            }
            Command::ViewportClientCensus { socket, generation, nonce } => {
                hide_viewport_callback_failure(
                    VIEWPORT_CLIENT_CENSUS_VERB,
                    run_viewport_client_census(&socket, &generation, &nonce),
                )
            }
            Command::ViewportManifestRefresh { socket, session, generation, nonce } => {
                hide_viewport_callback_failure(
                    VIEWPORT_MANIFEST_REFRESH_VERB,
                    attach::refresh_viewport_manifest(&socket, &session, &generation, &nonce),
                )
            }
            Command::ViewportSidebarWidth {
                socket,
                session,
                organization,
                session_id,
                nonce,
                columns,
            } => hide_viewport_callback_failure(
                VIEWPORT_SIDEBAR_WIDTH_VERB,
                attach::release_sidebar_width(
                    &socket,
                    &session,
                    &organization,
                    &session_id,
                    &nonce,
                    &columns,
                ),
            ),
            Command::BenchClick { targets, rounds } => {
                run_bench_click(&paths::current_dir()?, &targets, rounds).await
            }
        }
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    // The console formatter this program has always had, PLUS the daemon-level
    // file sink at `<dir>/.chief/log/chief.jsonl`. Both, and the gh#502 ANSI
    // rule that used to be pasted here, are `chiefd_log::install`'s.
    //
    // THIS is the program whose silence cost the 4½-minute launch: everything
    // before a company exists happens here, in a process that wrote to no file
    // at all. It logs from its first line now.
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    chiefd_log::install("chief");
    // NO LOG EVER REACHES A SCREEN. Not the sidebar's pane, not the actuator's,
    // not the terminal `chief attach` was typed into. The operator's ruling,
    // after watching forty `INFO chief::http` lines paint over what they were
    // reading: "always log to only the file."
    //
    // It used to be per-PHASE — the sidebar handed the glass over at its first
    // line and every other verb kept printing — and the reasoning was that a
    // CLI's operator is reading the terminal. They are; they are just not
    // reading the LOG. What they asked for is this program's own `println!`
    // progress, which is untouched, and `<dir>/.chief/log/chief.jsonl` keeps
    // every line either way.
    chiefd_log::console_off();

    tracing::info!(
        event = "process.start",
        version = env!("CHIEF_VERSION"),
        // The VERB only. Arguments carry paths and company operations, so the
        // log records which command ran and never its operands.
        verb = args.first().map_or("", String::as_str),
        "chief started"
    );
    match route(&args) {
        Ok(command) => run(command),
        Err(error) => {
            // `eprintln!` and not `tracing::error!`: this is a usage answer to
            // a person at a terminal, not a daemon event, and the log
            // formatter's key=value colouring has no business in it.
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        actuate_role_from, actuating_banner, bare_door, hide_viewport_callback_failure, route,
        supervisor_program, upgrade, usage, ActuateRole, BareDoor, Command, LifecycleError,
        RouteError, DAEMON_VERBS, OPERATOR_VERBS,
    };

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|value| (*value).to_string()).collect()
    }

    /// #1207: a bare `chief actuate` supervises; the marked process actuates.
    ///
    /// PRESENCE is the signal, not the value. A marker this build cannot parse
    /// still means "you are the child" — reading it as "supervisor" would put a
    /// second supervisor under the first and, through it, a second actuator,
    /// which is the one thing this product cannot survive (there is no
    /// single-actuator lease, and the brain unlinks the rail socket
    /// unconditionally).
    #[test]
    fn bare_actuate_supervises_and_the_marker_runs_the_body() {
        assert_eq!(actuate_role_from(None), ActuateRole::Supervisor);
        assert_eq!(actuate_role_from(Some("3")), ActuateRole::Child { attempt: 3 });
        assert_eq!(actuate_role_from(Some(" 12 ")), ActuateRole::Child { attempt: 12 });
        assert_eq!(
            actuate_role_from(Some("")),
            ActuateRole::Child { attempt: 1 },
            "an unreadable marker is still a marker: presence is the signal"
        );
        assert_eq!(actuate_role_from(Some("not-a-number")), ActuateRole::Child { attempt: 1 });
    }

    #[test]
    fn the_child_banner_carries_the_attempt() {
        let dir = std::path::Path::new("/companies/northstar");
        let first = actuating_banner(dir, "chiefd-cli@host#7", 1);
        assert!(first.contains("actuating as chiefd-cli@host#7"), "{first}");
        assert!(
            !first.contains("restart"),
            "a healthy first start must not make an operator read the word: {first}"
        );
        assert!(first.contains("This process must stay running"), "the old sentence stands");

        let later = actuating_banner(dir, "chiefd-cli@host#7", 3);
        assert!(later.contains("(restart #3)"), "{later}");
        assert!(later.contains("actuating as chiefd-cli@host#7"), "{later}");
    }

    /// The supervisor re-runs the path it was invoked as. Pinned because the
    /// alternative — `current_exe()` — reads `<path> (deleted)` on Linux after
    /// an in-place install and would fail every spawn thereafter.
    #[test]
    fn the_supervisor_respawns_the_path_it_was_invoked_as() {
        let program = supervisor_program();
        assert!(!program.as_os_str().is_empty(), "argv[0] is always something");
        let command = chief_cli::actuate::supervise::child_command(&program, 2);
        assert_eq!(command.get_program(), program.as_os_str());
    }

    #[test]
    fn hidden_viewport_failure_is_logged_but_exits_silently() {
        let result = hide_viewport_callback_failure(
            "viewport-manifest-refresh",
            Err(LifecycleError::host("forced hidden callback failure")),
        );
        assert!(result.is_ok(), "tmux must never publish a background callback failure");
    }

    /// The deleted TypeScript entry point's file name, ASSEMBLED rather than
    /// spelled.
    ///
    /// Every assertion below that searches for it searches THIS FILE too, and
    /// P6 merged the router and its tests into one file — so a literal here
    /// would match itself and turn a real guard into a permanently red one,
    /// which is how guards get deleted. Building it from parts is what keeps
    /// the search honest.
    fn javascript_entry_point() -> String {
        format!("{}.ts", "Main")
    }

    /// NO VERB PRINTS A LOG LINE. The operator's ruling: "always log to only
    /// the file, don't log anywhere else."
    ///
    /// Read from the SOURCE, because the thing being pinned is a choice made in
    /// `main()` before any of this is reachable as a value — and because the
    /// symptom is invisible to every other kind of test: the program keeps
    /// working perfectly while painting its own log over what the operator is
    /// reading. It was the sidebar's own pane once; it was `chief attach`'s
    /// forty HTTP lines the day this rule was written.
    #[test]
    fn every_verb_sends_its_log_to_the_file_and_never_to_a_screen() {
        let source = include_str!("main.rs");
        let after_install = source
            .split_once("chiefd_log::install(\"chief\");")
            .expect("every program installs the sinks")
            .1;
        let console_off = after_install
            .find("chiefd_log::console_off();")
            .expect("and this one silences the console immediately");
        assert!(
            !after_install[..console_off].contains("if args"),
            "the silence is UNCONDITIONAL — not per verb, not per phase, and nothing may \
             be branched on before it"
        );
        // ASSEMBLED, never spelled: a literal needle would appear in this file
        // as the argument to `contains` and match the assertion itself, which
        // is how a guard ends up unconditionally red and then deleted.
        let per_verb_install = format!("install_{}_only", "file");
        assert!(
            !source.contains(&per_verb_install),
            "there is no per-VERB install: one program, one answer"
        );
    }

    /// THE OPERATOR'S DOOR to "stop working, and stay stopped".
    ///
    /// It exists because obeying was not enough: a live company was told to
    /// stop all work, the CEO obeyed and parked six people, and forty-five
    /// seconds later all six were back. There was no supported way to make a
    /// company stop from inside the product, and `chief stop` is not it —
    /// that takes the daemon down and leaves nobody to talk to.
    #[test]
    fn stand_down_and_resume_are_typed_verbs_with_no_flags_to_get_wrong() {
        assert_eq!(route(&argv(&["stand-down"])), Ok(Command::StandDown { reason: String::new() }));
        assert_eq!(route(&argv(&["resume"])), Ok(Command::Resume));
    }

    /// The reason is FREE TEXT, and the parse does not treat any of it as
    /// flags. An operator writing why they stopped their company is saying
    /// something to whoever reads the refusal later, and a parser that refused
    /// `--the customer called` would be refusing the sentence.
    #[test]
    fn a_stand_down_reason_is_prose_and_never_parsed_as_flags() {
        assert_eq!(
            route(&argv(&["stand-down", "stop", "all", "work", "now"])),
            Ok(Command::StandDown { reason: "stop all work now".into() })
        );
        assert_eq!(
            route(&argv(&["stand-down", "--the", "customer", "called"])),
            Ok(Command::StandDown { reason: "--the customer called".into() })
        );
    }

    /// Both are advertised, because a door an operator cannot find is the
    /// defect this closes.
    #[test]
    fn help_advertises_the_way_to_stop_a_company_and_the_way_back() {
        let usage = usage();
        for verb in ["chief stand-down", "chief resume"] {
            assert!(usage.lines().any(|line| line.trim_start().starts_with(verb)), "{verb}");
        }
    }

    #[test]
    fn bare_chief_is_the_only_local_company_creation_door() {
        for retired in ["new", "create"] {
            assert!(
                matches!(route(&argv(&[retired])), Err(RouteError::UnknownCommand { .. })),
                "chief {retired} must not bypass the directory-scoped Founder door"
            );
            assert!(
                !usage()
                    .lines()
                    .any(|line| line.trim_start().starts_with(&format!("chief {retired}"))),
                "help must not advertise the retired chief {retired} door"
            );
        }
    }

    /// Bare `chief` parses to ONE command whichever directory it is run in.
    ///
    /// The choice between founding and going in is the runner's, off the
    /// database file — deliberately not the parser's, because a parse that
    /// stats the filesystem cannot be tested without one, and this enum's
    /// whole job is to make the argv contract testable with no tmux server, no
    /// beacond and no daemon.
    #[test]
    fn bare_chief_is_a_command_not_an_error() {
        assert_eq!(route(&argv(&[])), Ok(Command::Bare));
    }

    /// The public front-door contract, and the one guard the whole surface
    /// rests on.
    ///
    /// These four spent a long time advertised in help and fully implemented
    /// while NOTHING routed them: `chief ls` answered "Unknown command 'ls'",
    /// and only a private argv-prefix bridge could reach the implementation.
    /// A verb that exists, is documented, and is unreachable looks identical
    /// to a working one from every angle except actually typing it — so this
    /// asserts reachability by name, for each of the four, individually.
    #[test]
    fn the_four_front_door_lifecycle_verbs_are_routed_at_the_top_level() {
        assert_eq!(route(&argv(&["ls"])), Ok(Command::List));
        assert_eq!(route(&argv(&["attach"])), Ok(Command::Attach));
        assert_eq!(route(&argv(&["stop"])), Ok(Command::Stop));
        assert_eq!(route(&argv(&["reset"])), Ok(Command::Reset { yes: false }));
    }

    /// `chief upgrade` is a TOP-LEVEL verb, and its flags are its own.
    ///
    /// It is routed here rather than left to a wrapper because it is the one
    /// verb the product must answer on a box whose install is exactly what it
    /// is about to replace: no daemon, no beacond, possibly no working
    /// company. An unknown flag is a refusal that names it, like every other
    /// verb — `chief upgrade --force` must not silently upgrade.
    #[test]
    fn routes_upgrade_and_its_own_flags() {
        assert_eq!(
            route(&argv(&["upgrade"])),
            Ok(Command::Upgrade(upgrade::Mode::Install { skip_pi_check: false }))
        );
        assert_eq!(
            route(&argv(&["upgrade", "--check"])),
            Ok(Command::Upgrade(upgrade::Mode::Check))
        );
        assert_eq!(
            route(&argv(&["upgrade", "--rollback"])),
            Ok(Command::Upgrade(upgrade::Mode::Rollback))
        );
        assert_eq!(
            route(&argv(&["upgrade", "--skip-pi-check"])),
            Ok(Command::Upgrade(upgrade::Mode::Install { skip_pi_check: true }))
        );
        assert_eq!(
            route(&argv(&["upgrade", "--force"])),
            Err(RouteError::UnknownFlag { verb: "upgrade", flag: "--force".into() })
        );
    }

    #[test]
    fn routes_the_directory_operator_verbs() {
        assert_eq!(route(&argv(&["ls"])), Ok(Command::List));
        assert_eq!(route(&argv(&["attach"])), Ok(Command::Attach));
        assert_eq!(
            route(&argv(&["attach", "--yes"])),
            Err(RouteError::UnknownFlag { verb: "attach", flag: "--yes".into() })
        );
        assert_eq!(route(&argv(&["stop"])), Ok(Command::Stop));
        assert_eq!(route(&argv(&["reset", "--yes"])), Ok(Command::Reset { yes: true }));
    }

    /// Bare `chief` in a company uses the same company-entry path as explicit
    /// `chief attach`. This is the first half of the no-prompt rule; attach's
    /// stopped-daemon decision test holds the second half.
    #[test]
    fn bare_chief_in_a_company_uses_the_company_door() {
        assert_eq!(bare_door(true), BareDoor::Company);
        assert_eq!(bare_door(false), BareDoor::Founder);
    }

    #[test]
    fn sleeping_person_card_is_internal_exact_argv_and_not_advertised() {
        assert_eq!(
            route(&argv(&[
                "sleeping-person-card",
                "ada",
                "Ada Lovelace",
                "Quant Analyst",
                "selected",
                "openai",
                "gpt-5.6",
                "wake was refused",
                "",
            ])),
            Ok(Command::SleepingPersonCard {
                person: "ada".into(),
                name: "Ada Lovelace".into(),
                role: "Quant Analyst".into(),
                model: chief_cli::actuate::launch_catalog::PersonModel {
                    state: chief_cli::actuate::launch_catalog::PersonModelState::Selected,
                    provider: Some("openai".into()),
                    model: Some("gpt-5.6".into()),
                },
                refusal: Some("wake was refused".into()),
                blocked: None,
            })
        );
        assert!(!usage().contains("sleeping-person-card"));
    }

    /// THE GATE'S SENTENCE TRAVELS IN ITS OWN SLOT, so the card can tell "one
    /// wake was refused, ask again" from "this person cannot start at all".
    /// The second has no button, and a card that could not tell them apart
    /// would offer one.
    #[test]
    fn the_launch_gates_reason_reaches_the_card_apart_from_a_wake_refusal() {
        let Ok(Command::SleepingPersonCard { refusal, blocked, .. }) = route(&argv(&[
            "sleeping-person-card",
            "ada",
            "Ada Lovelace",
            "Quant Analyst",
            "pi-default",
            "",
            "",
            "",
            "required files 'settings.json' and 'agent.md' are missing from home '/c/ada'",
        ])) else {
            panic!("the card verb must route with the gate's own slot");
        };
        assert_eq!(refusal, None, "no wake was attempted, so no wake was refused");
        assert_eq!(
            blocked.as_deref(),
            Some("required files 'settings.json' and 'agent.md' are missing from home '/c/ada'"),
            "the gate's sentence travels verbatim"
        );
    }

    #[test]
    fn viewport_resize_is_internal_routed_and_not_advertised() {
        assert_eq!(
            route(&argv(&[
                "viewport-resize",
                "chiefd-acme",
                "org-acme_",
                "acme",
                "/dev/pts/4",
                "41",
                "0123456789abcdef0123456789abcdef",
            ])),
            Ok(Command::ViewportResize {
                socket: "chiefd-acme".into(),
                session: "org-acme_".into(),
                organization: "acme".into(),
                client: "/dev/pts/4".into(),
                event: "41".into(),
                nonce: "0123456789abcdef0123456789abcdef".into(),
            })
        );
        assert_eq!(
            route(&argv(&[
                "viewport-client-eligible",
                "chiefd-acme",
                "org-acme_",
                "/dev/pts/4",
                "0123456789abcdef0123456789abcdef",
            ])),
            Ok(Command::ViewportClientEligible {
                socket: "chiefd-acme".into(),
                session: "org-acme_".into(),
                client: "/dev/pts/4".into(),
                nonce: "0123456789abcdef0123456789abcdef".into(),
            })
        );
        assert_eq!(
            route(&argv(&[
                "viewport-client-changed",
                "chiefd-acme",
                "/dev/pts/4",
                "0123456789abcdef0123456789abcdef",
            ])),
            Ok(Command::ViewportClientChanged {
                socket: "chiefd-acme".into(),
                client: "/dev/pts/4".into(),
                nonce: "0123456789abcdef0123456789abcdef".into(),
            })
        );
        assert_eq!(
            route(&argv(&[
                "viewport-client-census",
                "chiefd-acme",
                "42",
                "0123456789abcdef0123456789abcdef",
            ])),
            Ok(Command::ViewportClientCensus {
                socket: "chiefd-acme".into(),
                generation: "42".into(),
                nonce: "0123456789abcdef0123456789abcdef".into(),
            })
        );
        assert_eq!(
            route(&argv(&[
                "viewport-manifest-refresh",
                "chiefd-acme",
                "org-acme_",
                "0123456789abcdef0123456789abcdef",
                "43",
            ])),
            Ok(Command::ViewportManifestRefresh {
                socket: "chiefd-acme".into(),
                session: "org-acme_".into(),
                generation: "43".into(),
                nonce: "0123456789abcdef0123456789abcdef".into(),
            })
        );
        assert_eq!(
            route(&argv(&[
                "viewport-sidebar-width",
                "chiefd-acme",
                "org-acme_",
                "acme",
                "$9",
                "0123456789abcdef0123456789abcdef",
                "31",
            ])),
            Ok(Command::ViewportSidebarWidth {
                socket: "chiefd-acme".into(),
                session: "org-acme_".into(),
                organization: "acme".into(),
                session_id: "$9".into(),
                nonce: "0123456789abcdef0123456789abcdef".into(),
                columns: "31".into(),
            })
        );
        assert!(
            route(&argv(&["viewport-sidebar-width", "chiefd-acme", "org-acme_", "31",])).is_err()
        );
        // THE DRAG COMMIT TAKES NO EPOCH. A seventh word is the #1196 argv,
        // and it is a routing error rather than an operand this quietly eats.
        assert!(route(&argv(&[
            "viewport-sidebar-width",
            "chiefd-acme",
            "org-acme_",
            "acme",
            "$9",
            "41",
            "0123456789abcdef0123456789abcdef",
            "31",
        ]))
        .is_err());
        assert!(!usage().contains("viewport-resize"));
        assert!(!usage().contains("viewport-sidebar-width"));
    }

    /// A COMPANY POSITIONAL IS A MISTAKE, and every verb that used to take one
    /// says so BY NAME.
    ///
    /// The one refusal an operator with muscle memory will actually hit. The
    /// dangerous alternative is not a usage error — it is acting on the cwd
    /// while the operator believes they named another company, which would
    /// stop, reset or DELETE something they were not standing in.
    #[test]
    fn every_verb_that_used_to_take_a_company_now_refuses_one_by_name() {
        for verb in ["attach", "stop", "reset", "rm", "topology", "actuate"] {
            assert_eq!(
                route(&argv(&[verb, "acme"])),
                Err(RouteError::ExtraCompany { verb, extra: "acme".into() }),
                "`chief {verb} acme` must be refused, never acted on"
            );
        }
    }

    /// `chief bench click` routes with its targets and repeat count, and it
    /// fails CLOSED on everything else.
    ///
    /// The measurement mode Stage 0 of that work adds. It is a
    /// mode and not an operator verb — absent from [`OPERATOR_VERBS`] and so
    /// from the usage text, like `sidebar` and `host` — because it drives
    /// synthetic clicks into a live session and moves the operator's glass
    /// around.
    #[test]
    fn routes_the_click_bench_and_refuses_everything_it_does_not_understand() {
        assert_eq!(
            route(&argv(&["bench", "click"])),
            Ok(Command::BenchClick { targets: vec![], rounds: 1 }),
            "no targets is a legitimate invocation: it prints what the rail is drawing"
        );
        assert_eq!(
            route(&argv(&["bench", "click", "--rounds", "20", "department:Quant=quant"])),
            Ok(Command::BenchClick { targets: vec!["department:Quant=quant".into()], rounds: 20 })
        );
        assert_eq!(
            route(&argv(&["bench", "click", "--rounds=5", "person:Ada=ada", "x:y"])),
            Ok(Command::BenchClick {
                targets: vec!["person:Ada=ada".into(), "x:y".into()],
                rounds: 5,
            })
        );

        // A sub-verb this mode does not have, a repeat count that measures
        // nothing, and a flag nobody defined — all refusals, never defaults.
        assert!(route(&argv(&["bench", "hover"])).is_err());
        assert!(route(&argv(&["bench"])).is_err());
        assert!(route(&argv(&["bench", "click", "--rounds", "0"])).is_err());
        assert!(route(&argv(&["bench", "click", "--rounds", "many"])).is_err());
        assert!(route(&argv(&["bench", "click", "--forever"])).is_err());
    }

    /// Every daemon mode reaches the daemon, with its argv intact.
    ///
    /// The split's one behavioural risk. `chiefd run --company acme` is typed
    /// by scripts and spawned by test harnesses, and after P6 this binary
    /// cannot answer it — it links none of the daemon's crates. A mode missing
    /// from [`DAEMON_VERBS`] would be refused as an unknown command instead:
    /// a working invocation turning into a usage error.
    #[test]
    fn every_daemon_mode_is_forwarded_whole_rather_than_answered_here() {
        for mode in DAEMON_VERBS {
            assert_eq!(route(&argv(&[mode])), Ok(Command::Daemon(vec![mode.to_string()])));
        }
        assert_eq!(
            route(&argv(&["run", "--dir", "/work/acme"])),
            Ok(Command::Daemon(argv(&["run", "--dir", "/work/acme"]))),
            "the whole argv crosses, not just the verb"
        );
        // `host` is served HERE — it is this crate's own module, not the
        // daemon's — so it must never be forwarded.
        assert!(!DAEMON_VERBS.contains(&"host"));
        assert_eq!(route(&argv(&["host"])), Ok(Command::Host));
    }

    /// A daemon mode is never advertised, and never confused with a verb.
    #[test]
    fn the_daemon_modes_and_the_operator_verbs_are_disjoint_sets() {
        for (verb, _, _) in OPERATOR_VERBS {
            assert!(!DAEMON_VERBS.contains(&verb), "'{verb}' cannot be both");
        }
    }

    #[test]
    fn usage_fails_closed_the_same_way_the_typescript_did() {
        // The ported contract: an unknown flag and an unexpected positional are
        // each a refusal — never a default, never a guess. The MISSING
        // positional that stood here is gone with the argument itself: `chief
        // attach` with nothing after it is now the whole invocation.
        assert_eq!(
            route(&argv(&["attach", "--force"])),
            Err(RouteError::UnknownFlag { verb: "attach", flag: "--force".into() })
        );
        assert_eq!(route(&argv(&["attach"])), Ok(Command::Attach));
        assert_eq!(
            route(&argv(&["attach", "acme"])),
            Err(RouteError::ExtraCompany { verb: "attach", extra: "acme".into() })
        );
        assert!(matches!(route(&argv(&["new"])), Err(RouteError::UnknownCommand { .. })));
    }

    /// Help is THIS binary's, in all three spellings.
    ///
    /// All three used to be handed to the deleted TypeScript entry point with a
    /// literal `"chiefd"` inserted as argv[0], so that entry point read the
    /// program's own name as the command and answered
    /// `chiefd: unknown command 'chiefd'`. A binary whose `--help` reports its
    /// own name as unknown is not merely unhelpful; it is evidence the argv
    /// belongs to something else.
    #[test]
    fn help_is_claimed_by_this_binary_in_every_spelling() {
        for spelling in ["help", "--help", "-h"] {
            assert_eq!(route(&argv(&[spelling])), Ok(Command::Help), "{spelling}");
        }
        for spelling in ["--version", "-V"] {
            assert_eq!(route(&argv(&[spelling])), Ok(Command::Version), "{spelling}");
        }
    }

    /// `chief attach --help` used to be `chief attach does not support flag
    /// '--help'` — a usage error in the one place an operator looks to learn
    /// the usage.
    #[test]
    fn every_verb_answers_its_own_help_instead_of_refusing_the_flag() {
        for (verb, _, _) in OPERATOR_VERBS {
            assert_eq!(route(&argv(&[verb, "--help"])), Ok(Command::Help), "{verb} --help");
            assert_eq!(route(&argv(&[verb, "-h"])), Ok(Command::Help), "{verb} -h");
        }
        // Even after a positional, which is how a human actually types it.
        assert_eq!(route(&argv(&["attach", "acme", "--help"])), Ok(Command::Help));
    }

    /// The help text names every verb it claims to serve, and must not name a
    /// retired product or a mode an operator should never type.
    ///
    /// DERIVED, not transcribed: the loop is over the routing table itself, so
    /// a verb added without a help line — or a help line for a verb nobody
    /// routes — cannot exist in the first place.
    #[test]
    fn the_usage_text_names_exactly_the_operator_surface() {
        let text = usage();
        for (verb, arguments, descriptions) in OPERATOR_VERBS {
            assert!(
                text.contains(&format!("chief {verb}{arguments}")),
                "help must document '{verb}' exactly as it is spelled"
            );
            for description in descriptions {
                assert!(text.contains(description), "help must print '{verb}'s description");
            }
        }
        assert!(text.contains("chief help"));
        // The flag whose absence from every surface is what let a scripted
        // caller hang on a prompt it was never told about.
        assert!(text.contains("--yes"));
        // Modes are spawned, never typed — including the one this binary
        // itself serves, and every one it forwards. Matched as an INVOCATION
        // (`chief <mode>`) rather than as a bare word: `run` is a substring of
        // "the running companies", which is the first line of the help, so a
        // bare-word check here would fail on correct copy.
        for internal in DAEMON_VERBS.iter().copied().chain(["host", "founder-pi"]) {
            assert!(
                !text.contains(&format!("chief {internal}")),
                "help must not advertise 'chief {internal}'"
            );
        }
        // Retired names have no place in surfaced copy.
        for retired in ["launcher", "Launcher", "triber"] {
            assert!(!text.contains(retired), "'{retired}' is retired");
        }
    }

    /// An unknown command is refused by `chief`, with `chief`'s own usage.
    ///
    /// Six organization verbs used to be `exec`'d into a Bun entry point that
    /// serves exactly one internal command, so `chief catalog` paid a whole
    /// JavaScript runtime start to be told `unknown command 'catalog'` by a
    /// different program. Nothing is forwarded now except the daemon modes
    /// this binary knows by name.
    #[test]
    fn an_unknown_command_is_refused_by_this_binary_with_its_own_usage() {
        let refusal =
            route(&argv(&["catalog"])).expect_err("an unrouted verb must refuse").to_string();
        assert!(refusal.starts_with("chief: unknown command 'catalog'"), "{refusal}");
        assert!(refusal.contains("chief attach"), "{refusal}");
        assert!(!refusal.contains("chief attach [--yes]"), "{refusal}");
        // The whole reason the old message was wrong: it named a second
        // program's idea of the command surface.
        assert!(!refusal.contains(&javascript_entry_point()), "{refusal}");
        // And a typo is not quietly handed to the daemon either, which would
        // reintroduce "an unknown command reported by a second program".
        assert!(!refusal.contains("chiefd"), "{refusal}");
    }

    /// The forwarding router is gone, and so is the last reach into Bun.
    ///
    /// INVERTED, deliberately, rather than deleted. This test used to assert
    /// `founder.contains("founder-pi")` — *"the one legitimate Bun reach must
    /// survive"* — because opening a Founder session went through a TypeScript
    /// entry point that took that one argument. `chief` now builds the Founder
    /// Pi argv itself ([`super::founder_pi`]) and spawns Pi directly, so the
    /// surviving reach is to **Pi**, the agent runtime, and not to a dispatcher
    /// standing in front of it. Keeping the assertion and flipping its sense is
    /// what makes the shim unresurrectable: a revert re-adding the spawn fails
    /// here by name, not by review.
    #[test]
    fn nothing_in_this_binary_forwards_an_operator_verb_to_a_javascript_entry_point() {
        // The router's own needle is assembled for the same reason
        // `javascript_entry_point` is: this assertion used to read a DIFFERENT
        // file (the daemon binary's `main.rs`) and now reads its own.
        let main = include_str!("main.rs");
        let delegation_router = format!("{}::route", "operator");
        assert!(!main.contains(&delegation_router), "the delegation router is deleted");
        assert!(
            !main.contains(&javascript_entry_point()),
            "main.rs must not name a JavaScript entry point"
        );
        let founder = include_str!("founder.rs");
        for banned in [javascript_entry_point(), "founder-pi".to_string()] {
            assert!(!founder.contains(&banned), "'{banned}' must not come back");
        }
        assert!(
            founder.contains("founder_pi::founder_pi_argv"),
            "the Founder Pi argv must be built in Rust"
        );
    }

    /// The verb that makes a company actuated at all.
    ///
    /// Reachability by name, individually, for the same reason the four
    /// lifecycle verbs get it: `ls` spent a long time documented, implemented
    /// and unroutable, and a verb that cannot be typed looks identical to a
    /// working one from every angle except typing it. This one carries more
    /// weight than most — after #751/P8 a company with no attached client is
    /// un-actuated, so an unroutable `actuate` is a product that starts nobody.
    #[test]
    fn actuate_is_routed_at_the_top_level_and_takes_no_company() {
        assert_eq!(route(&argv(&["actuate"])), Ok(Command::Actuate));
        // The company is the pane's working directory, so a word here is a
        // mistake — and `attach` starts the actuator with exactly two words
        // (`attach::actuator_command`), which this refusal is what makes
        // load-bearing.
        assert_eq!(
            route(&argv(&["actuate", "acme"])),
            Err(RouteError::ExtraCompany { verb: "actuate", extra: "acme".into() })
        );
    }

    /// `actuate` asks nothing, so `--yes` is a flag it does not know.
    ///
    /// A resident mode that prompted would hang the first time a supervisor
    /// started it with no terminal to answer on.
    #[test]
    fn actuate_does_not_accept_yes_because_it_never_prompts() {
        assert_eq!(
            route(&argv(&["actuate", "--yes"])),
            Err(RouteError::UnknownFlag { verb: "actuate", flag: "--yes".into() })
        );
    }

    /// The help text tells an operator the one thing about this verb that will
    /// otherwise be learned by closing the terminal.
    #[test]
    fn the_usage_text_says_that_actuate_stays_open() {
        let text = usage();
        assert!(text.contains("chief actuate"), "{text}");
        assert!(!text.contains("chief actuate <company>"), "no verb takes one: {text}");
        assert!(
            text.contains("stays open"),
            "an operator who does not know this verb is resident will close it: {text}"
        );
    }

    #[test]
    fn stop_does_not_accept_yes_because_it_never_prompts() {
        // `stop` is non-destructive and has no confirmation, so `--yes` is a
        // flag it does not know rather than a silently-ignored one.
        assert_eq!(
            route(&argv(&["stop", "--yes"])),
            Err(RouteError::UnknownFlag { verb: "stop", flag: "--yes".into() })
        );
    }

    /// THE GAP. The product could create a company and stop one, and had no
    /// verb at all that removed one, so every company anybody ever made stayed
    /// in `chief ls` for ever. Typing the obvious word got
    /// `chief: unknown command 'rm'`.
    #[test]
    fn removal_is_a_verb_this_binary_answers() {
        assert!(route(&argv(&["rm"])).is_ok(), "there must be a way to remove a company");
        assert_eq!(route(&argv(&["rm"])), Ok(Command::Remove { yes: false }));
        assert_eq!(route(&argv(&["rm", "--yes"])), Ok(Command::Remove { yes: true }));
    }

    /// It deletes durable state, so every usage error fails closed exactly the
    /// way `attach` and `reset` do — a removal must never be assembled out of a
    /// guess about which company was meant.
    #[test]
    fn removal_refuses_an_extra_or_an_unknown_argument() {
        assert_eq!(
            route(&argv(&["rm", "acme"])),
            Err(RouteError::ExtraCompany { verb: "rm", extra: "acme".into() })
        );
        assert_eq!(
            route(&argv(&["rm", "--force"])),
            Err(RouteError::UnknownFlag { verb: "rm", flag: "--force".into() })
        );
    }

    /// The help must say that this one deletes data. `stop` and `reset` both
    /// promise the opposite, and an operator who confuses the three loses a
    /// company.
    #[test]
    fn the_usage_text_says_removal_deletes_the_data_and_stop_does_not() {
        let text = usage();
        assert!(text.contains("chief rm [--yes]"), "{text}");
        assert!(text.contains("delete"), "{text}");
        assert!(
            text.contains("stop this company's runtime, then its daemon"),
            "stop keeps its own non-destructive description: {text}"
        );
        assert!(
            text.contains("delete its .chief/"),
            "and rm says which folder it deletes, because it deletes inside a directory the \
             operator owns: {text}"
        );
    }
}
