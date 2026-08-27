// TIME TO PANE — the number nobody has measured, measured from a proven cold start.
//
// WHAT THIS IS
// ------------
// An OPERATOR INSTRUMENT, run on demand by a human. It is deliberately not
// wired into CI, `package.json` or `guard-wiring-manifest.mjs`: it creates a
// real company, spawns real processes and paints real tmux panes, and a gate
// that does that is not a gate. Its register row lives in
// `scripts/script-invocation-lib.mjs` under `OPERATOR_ENTRYPOINTS`, which is
// the repo's one way to say "a human types this".
//
//   node scripts/cold-start-latency.mjs [--parent dir] [--name-prefix p]
//                                       [--interval 250] [--bound 120000]
//                                       [--socket name] [--wake-log] [--keep]
//                                       [--json]
//
// A COMPANY IS A DIRECTORY, so this mints a fresh one under `--parent` and
// runs the product's own verbs inside it. Every verb it drives — `create`,
// `actuate`, `rm` — acts on the directory the process is standing in and takes
// no company argument at all.
//
// WHAT IT MEASURES, AND WHY IT IS NOT THE THING ALREADY PROVEN
// ------------------------------------------------------------
// Reconciliation is event-driven: a commit signals through the existing
// `ChangeFeed` and the reconciler converges, with a 60s periodic pass kept as a
// level-triggered backstop (`DEFAULT_REACTIVE_FALLBACK_FLOOR`,
// apps/chiefd/crates/chiefd-daemon/src/run.rs). A test already proves the WAKE
// fires within a second of the commit. Nobody has measured what the operator
// actually experiences: how long from "create a department" until "the head is
// running in a tmux pane". That is this file's whole subject.
//
// Two legs, kept apart because only one of them can be cold:
//
//   LEG A — cold CEO.  `chief create` -> start an actuator -> CEO-only intent
//                      -> the CEO's pane. Genuinely cold: the slug has never
//                      existed anywhere.
//   LEG B — TIME TO PANE.  The headline. Commit `/v1/org/department/create`,
//                      then wait for the head's pane.
//
// Leg B runs with a WARM ACTUATOR by construction — a pane cannot exist without
// one — and this file says so rather than hiding it. That is the honest
// operator scenario: nobody creates a department in a company that is not up.
// Leg A is the cold one, and it is the leg the cold proof below defends.
//
// THE TRAP THIS FILE IS DESIGNED AGAINST
// --------------------------------------
// This exact measurement has already been ruined once. A seat measured attach
// latency, got 1.77s, and concluded the slow path was gone — on the OLD,
// unfixed code. Only the actuator had been killed; the company session was
// still standing, so the wait being timed returned instantly. A harness that
// silently measures a warm stack is worse than no harness at all: it produces a
// confident wrong number and ends the investigation.
//
// WHICH SEQUENCE THIS REPRODUCES, and where that was established. The product
// claims the runtime ownership lease INSIDE a launch or a stop — never when an
// actuator merely starts. Traced, not assumed:
//
//   chiefd-host/src/runtime_lifecycle.rs  claim_ownership(), called from
//     launch_runtime and stop_supervised_runtime — both launch or stop.
//     (`launch_ceo_only_runtime` was a third caller until chief-home-is-cwd
//     §4c deleted the daemon-side CEO boot.)
//   POST /v1/org/runtime/ownership/claim  — the standalone claim route has NO
//     production caller; every real claim happens inside one of the three above
//   chief-cli  — does not reference runtime_lifecycle at all: the actuator
//     converges, the DAEMON launches
//
// So an actuator legitimately runs UNLEASED until something needs running, and
// this file follows that order: spawn the actuator, wait only for facts that
// can be true yet (process alive + tmux answering), and only THEN time the
// lease going active — which on the real path is the instant the runtime
// commits to projecting. The explicit `prepare-ceo-only` POST that used to sit
// between those two steps is gone with the route: `chief create`'s own genesis
// records the root's start decision (`org_manifest_genesis` calls
// `prepare_ceo_only` directly), so the intent already exists by the time the
// actuator is up, and the clock below runs from actuator readiness.
//
// An earlier version required the lease BEFORE stating the intent that causes
// it, and therefore could never return. The caution behind it was right — a
// lease is not evidence of a live process, 188 samples once read `present` with
// zero actuators on the box — but "do not trust the lease ALONE" became
// "REQUIRE the lease", which is a different rule and an unreachable gate. The
// lease is still never trusted alone; it is simply no longer a precondition.
//
// So this file PROVES its cold start rather than assuming it, and when it
// cannot it REFUSES TO PRINT A NUMBER. See `proveCold`. Cold here is scoped to
// the company and the report says so: it records how many other companies are
// registered and how many actuator processes exist on the box as CONTEXT, and
// claims nothing about the whole host.
//
// A LEASE IS NOT A PROCESS
// ------------------------
// 188 consecutive samples once reported `presence: present` while ZERO actuator
// processes existed anywhere on the host. Actuator readiness here therefore
// requires three independent facts: the PID this file spawned is alive, the
// tmux server answers on the socket, and chiefd reports presence. Any one of
// them alone is a story, not evidence.
//
// WHAT COUNTS AS "THE PANE EXISTS"
// --------------------------------
// A live `tmux list-panes` reply, never an HTTP status. This fleet's most
// expensive defect class is a success shape covering a failure — `applied 1
// action(s)` for a company running nobody. So the pane instant is the first
// sample in which a pane in session `org-<slug>-<key6>_` carries
// `@organization_person_id == <the head we hired>` with `#{pane_dead}` clear.
// The tmux user options are what the actuator itself stamps
// (chief-cli/src/actuate/trust.rs: `@organization_person_id`), which is why
// they are the thing read rather than a pane title or an index.
//
// HOW THE COLD PROOF IS KEPT HONEST
// --------------------------------
// It is in TWO parts, and the split is what makes both of them real.
//
// Before anything is created there is no slug and no company key, so no
// session name exists to look for — only the DIRECTORY can be asked about.
// After `chief create` the daemon publishes its rendezvous, which SERVES the
// key, and the session half becomes a question about a name the product has
// just minted. `create` paints no panes (only `attach` and `actuate` do), so a
// session bearing this company's name at that moment means something else on
// the box is already actuating it.
//
// The previous version asked both halves at once, before the company existed,
// against a retired global company tree and a keyless session name. None of those
// can be non-empty under any company, so the proof passed unconditionally
// while printing COLD for all five rows. Every rule now lives in
// `cold-start-latency-lib.mjs`, and every one is driven against a WARM fixture
// by `scripts/test/cold-start-latency.test.mjs` — which is how this file knows
// its own gate is not vacuous.
//
// BEFORE / AFTER — WHAT WOULD MAKE THE COMPARISON INVALID
// -------------------------------------------------------
// "AFTER" is this build. "BEFORE" is a build whose reconciler had the ~1s tick
// instead of the event wake. From cold, an unfixed build can show ~60s where a
// fixed one shows seconds — but ONLY from cold, and only against the same host,
// tmux socket, data root and sampling interval. Change any of those between
// the two runs and the pair is not a comparison.
//
// You cannot fake the baseline from this build. `CHIEFD_REACTIVE_FALLBACK_FLOOR_MS`
// fails closed below 60s (`MIN_REACTIVE_FALLBACK_FLOOR`), so it can LENGTHEN
// the safety net and can never shorten it into the old tick. A true "before"
// therefore needs an old `chief` + `chiefd` pair installed at
// `~/.chief/bin`. This file will not invent one, and prints no baseline of its
// own.
//
// WHAT IT NEVER DOES
// ------------------
// Never a broad `pkill` — teardown kills exactly the PID it spawned, and only
// while that PID's argv still matches what it spawned. Never a write to
// `~/.pi/agent/`: every agent runs on the operator's own Pi and nothing here
// writes a byte into it. Never a hand-built ledger write: the company, the
// department and the hire all go through the product's own surfaces, because
// hand-writing state proves nothing while looking like it worked.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import {
  argvNamesDaemonFor,
  chiefBinary,
  coldDirectoryAssertions,
  coldSessionAssertions,
  daemonLogPath,
  installHome,
  resourceRootBeside,
  sessionNamesFor,
  violations,
} from "./cold-start-latency-lib.mjs";

/** Default sampling cadence. A number without its resolution is not a
 *  measurement, so this is reported beside every delta it produced. */
const DEFAULT_INTERVAL_MS = 250;

/** Default bound on any single wait. Reported AS A TIMEOUT when hit — never as
 *  a large latency, which is the one reading that would be a lie. */
const DEFAULT_BOUND_MS = 120_000;

/** The box's own chief directory is not configurable
 *  (`chief-cli::paths::install_home`), so the install is a fact about `$HOME`
 *  while the COMPANY is whatever directory this run mints. */
const INSTALL_HOME = installHome(homedir());
const CHIEF_BIN = chiefBinary(homedir());

/** The root department every new department parents onto.
 *  Rust: `chiefd_core::store::organization::ROOT_DEPARTMENT_ID`. */
const ROOT_DEPARTMENT_ID = "executive";

/** Lines a converge pass writes. Only consulted under `--wake-log`, because
 *  seeing them requires raising the daemon's log level, and raising the log
 *  level changes the system being measured. */
const WAKE_LOG_PATTERN = /reconcil|converge|apply/i;

function usage() {
  return [
    "usage: node scripts/cold-start-latency.mjs [options]",
    "",
    "  --parent <dir>     where the throwaway company directory is minted (default: $HOME)",
    "  --name-prefix <p>  name stem for the throwaway company (default: coldstart)",
    "  --interval <ms>    sampling cadence (default: " + DEFAULT_INTERVAL_MS + ")",
    "  --bound <ms>       bound on each wait (default: " + DEFAULT_BOUND_MS + ")",
    "  --socket <name>    tmux socket to pin; default $TEAM_LAUNCHER_TMUX_SOCKET or 'default'",
    "  --wake-log         raise the daemon's log level so a wake instant is observable",
    "  --keep             leave the company and actuator running (skip teardown)",
    "  --json             print the machine-readable report only",
    "",
    "exit: 0 measured  2 cold/precondition refusal (no number printed)  3 timeout  4 product refusal",
  ].join("\n");
}

function parseArgs(argv) {
  const options = {
    parent: homedir(),
    namePrefix: "coldstart",
    intervalMs: DEFAULT_INTERVAL_MS,
    boundMs: DEFAULT_BOUND_MS,
    socket: process.env.TEAM_LAUNCHER_TMUX_SOCKET || "default",
    wakeLog: false,
    keep: false,
    json: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    switch (flag) {
      case "--parent":
        options.parent = String(value);
        index += 1;
        break;
      case "--name-prefix":
        options.namePrefix = String(value);
        index += 1;
        break;
      case "--interval":
        options.intervalMs = Number(value);
        index += 1;
        break;
      case "--bound":
        options.boundMs = Number(value);
        index += 1;
        break;
      case "--socket":
        options.socket = String(value);
        index += 1;
        break;
      case "--wake-log":
        options.wakeLog = true;
        break;
      case "--keep":
        options.keep = true;
        break;
      case "--json":
        options.json = true;
        break;
      case "--help":
      case "-h":
        process.stdout.write(usage() + "\n");
        process.exit(0);
        break;
      default:
        throw new Refusal("bad-argument", `unknown argument ${flag}\n\n${usage()}`);
    }
  }
  if (!Number.isFinite(options.intervalMs) || options.intervalMs <= 0) {
    throw new Refusal("bad-argument", "--interval must be a positive number of milliseconds");
  }
  if (!Number.isFinite(options.boundMs) || options.boundMs <= 0) {
    throw new Refusal("bad-argument", "--bound must be a positive number of milliseconds");
  }
  return options;
}

/** A refusal this file authored: something it checked and would not proceed
 *  past. Distinguished from a crash so the exit code can mean something. */
class Refusal extends Error {
  constructor(code, detail) {
    super(detail);
    this.code = code;
  }
}

const nowMs = () => Date.now();
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** `new Date(ms).toISOString()` is the exact `at` shape every chiefd route
 *  accepts (chief-cli/src/company.rs::iso_millis). */
const isoAt = (ms) => new Date(ms).toISOString();

// ---------------------------------------------------------------------------
// Process and tmux observation. Everything here READS; nothing signals except
// `teardown`, and that only ever names a PID this file spawned.
// ---------------------------------------------------------------------------

/** Every process on the box as `{ pid, args }`. One `ps`, parsed here, so no
 *  step below ever shells out a pattern that could match more than it meant. */
function processTable() {
  const listing = spawnSync("ps", ["-eo", "pid=,args="], { encoding: "utf8" });
  if (listing.status !== 0) {
    throw new Refusal(
      "ps-unreadable",
      `cannot enumerate processes: ${listing.stderr || listing.error}`,
    );
  }
  return String(listing.stdout)
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const gap = line.indexOf(" ");
      return { pid: Number(line.slice(0, gap)), args: line.slice(gap + 1) };
    })
    .filter((entry) => Number.isFinite(entry.pid) && entry.pid !== process.pid);
}

/**
 * Every actuator-shaped process on the box, whatever company it serves. Host
 * CONTEXT for the report, never a cold assertion — this file does not own
 * other companies and must not refuse because of them.
 *
 * There is deliberately no per-company version. `chief actuate` takes no
 * company: it acts on the directory it was started in, so its argv names
 * nothing to match on, and the honest per-company probe is its tmux session —
 * which is exactly what `coldSessionAssertions` asks about.
 */
const anyActuatorProcesses = (table) =>
  table.filter((entry) => /\bactuate\b/.test(entry.args) && /chief/.test(entry.args));

/** `tmux -L <socket> …` — the socket is a NAME, matching `tmux::run`. */
function tmux(socket, args) {
  return spawnSync("tmux", ["-L", socket, ...args], { encoding: "utf8" });
}

/** Session names on the socket. `[]` when no server is running, which is a
 *  legitimate answer and not a failure. */
function tmuxSessions(socket) {
  const listed = tmux(socket, ["list-sessions", "-F", "#{session_name}"]);
  if (listed.status !== 0) return [];
  return String(listed.stdout).split("\n").map((line) => line.trim()).filter(Boolean);
}

/**
 * Did the tmux BINARY answer at all on this socket?
 *
 * `status === 0` and "no server running on …" are both answers — the server
 * being empty is a fact about the world, not a broken probe. `status === null`
 * is the one case that is not an answer: tmux could not be executed, and no
 * pane observation from this run would mean anything.
 */
function tmuxAnswers(socket) {
  const listed = tmux(socket, ["list-sessions", "-F", "#{session_name}"]);
  return listed.status !== null && listed.error === undefined;
}

/**
 * Every pane on the socket, with the ownership tags the actuator stamps.
 *
 * `list-panes -a` rather than `-t <session>`: a missing session is then an
 * empty answer instead of an error, which keeps "not there yet" and "tmux
 * broke" apart during a wait.
 */
function tmuxPanes(socket) {
  const format = [
    "#{session_name}",
    "#{window_name}",
    "#{pane_id}",
    "#{pane_pid}",
    "#{pane_dead}",
    "#{@organization_id}",
    "#{@organization_person_id}",
  ].join("\t");
  const listed = tmux(socket, ["list-panes", "-a", "-F", format]);
  if (listed.status !== 0) return [];
  return String(listed.stdout)
    .split("\n")
    .map((line) => line.split("\t"))
    .filter((cells) => cells.length === 7)
    .map((cells) => ({
      session: cells[0],
      window: cells[1],
      paneId: cells[2],
      panePid: Number(cells[3]),
      dead: cells[4] === "1",
      organizationId: cells[5],
      personId: cells[6],
    }));
}

/**
 * The one definition of "this person is running in a pane".
 *
 * Three conditions, all required: the pane is in THIS company's session, it
 * carries the person's own ownership tag, and tmux does not report it dead. A
 * dead pane is a hire that failed at creation, and reporting one as a live pane
 * is exactly the success-shape-over-failure this harness exists to avoid.
 */
function livePaneFor(panes, session, personId) {
  return panes.find(
    (pane) => pane.session === session && pane.personId === personId && !pane.dead,
  );
}

/**
 * Sample `probe` every `intervalMs` until it answers truthy or the bound is
 * spent.
 *
 * Returns `{ found, atMs, samples, timedOut }` — and `timedOut` is a
 * FIRST-CLASS answer, never folded into a large `atMs`. Callers must branch on
 * it; a bound that is reported as a latency is a lie with a plausible shape.
 */
async function sampleUntil(probe, { intervalMs, boundMs, startedAtMs }) {
  const deadline = startedAtMs + boundMs;
  let samples = 0;
  for (;;) {
    samples += 1;
    const observedAtMs = nowMs();
    const found = await probe();
    if (found) return { found, atMs: observedAtMs, samples, timedOut: false };
    if (nowMs() >= deadline) {
      return { found: undefined, atMs: undefined, samples, timedOut: true };
    }
    await sleep(intervalMs);
  }
}

// ---------------------------------------------------------------------------
// Preconditions and the cold proof.
// ---------------------------------------------------------------------------

/**
 * What the run needs from the BOX, checked before a slug is minted.
 *
 * Encoded rather than assumed, and each one names its own recovery. Every
 * failure here is a refusal, not a measurement of zero.
 */
function checkPreconditions(options) {
  const evidence = [];
  const fail = (what) => {
    throw new Refusal("precondition-failed", what);
  };

  if (!existsSync(CHIEF_BIN)) {
    fail(`no chief installed at ${CHIEF_BIN} — run 'bun run release' first`);
  }
  evidence.push({ check: "chief installed", value: CHIEF_BIN });

  const tmuxVersion = spawnSync("tmux", ["-V"], { encoding: "utf8" });
  if (tmuxVersion.status !== 0) {
    fail("tmux does not answer -V; there is nothing to observe a pane with");
  }
  evidence.push({ check: "tmux answers", value: String(tmuxVersion.stdout).trim() });

  // The resources beside the INSTALLED binary, resolved the same way the
  // binary resolves them. A CEO with no `org_*` tools is what an unresolvable
  // resource root produces, and that is the whole reason this precondition
  // exists rather than being discovered from the CEO's own confusion.
  const resourceRoot = resourceRootBeside(realpathSync(chiefBinary(homedir())));
  if (!existsSync(join(resourceRoot, "packages", "piing", "extensions"))) {
    fail(`no packages/piing/extensions under ${resourceRoot} — a CEO would come up with no org_* tools`);
  }
  evidence.push({ check: "resource root", value: resourceRoot });
  evidence.push({ check: "tmux socket (pinned for both this harness and the actuator)", value: options.socket });

  return evidence;
}

/**
 * PROVE the start is cold, part ONE: the DIRECTORY.
 *
 * Asked before anything is created, because before genesis there is no slug
 * and no company key — so no session name exists to look for and asking about
 * one would be asking about a name nothing can ever be called. The two things
 * that CAN be true here are asked instead, and the first of them is stronger
 * than the three path checks it replaces: `<dir>/.chief` is the complete
 * footprint, so its absence rules out a store, keys, logs, a rendezvous and a
 * half-built genesis at once.
 *
 * Host warmth (other companies, other actuators) is RECORDED, never asserted.
 * Refusing because an unrelated company is up would make the instrument
 * unusable on the only kind of box it runs on.
 */
function proveDirectoryCold(dir, options) {
  const table = processTable();
  const sessions = tmuxSessions(options.socket);
  const assertions = coldDirectoryAssertions({ dir, exists: existsSync, processTable: table });
  const violated = violations(assertions);
  return {
    dir,
    cold: violated.length === 0,
    assertions,
    violated,
    context: {
      note: "host warmth is CONTEXT, never a cold assertion — this run owns one company, not the box",
      actuatorProcessesOnHost: anyActuatorProcesses(table).length,
      tmuxSessionsOnSocket: sessions.length,
      installHome: INSTALL_HOME,
    },
  };
}

/**
 * PROVE the start is cold, part TWO: has anything already PAINTED this
 * company?
 *
 * Asked after `chief create`, which is the first moment the slug and the
 * company key exist to be served. `create` runs genesis and starts the daemon
 * but paints no panes — only `attach` and `actuate` do — so a session bearing
 * this company's name here means something else on the box is already
 * actuating it, and no number from this run would mean anything.
 */
function proveSessionsCold({ slug, key }, options) {
  const sessions = tmuxSessions(options.socket);
  const assertions = coldSessionAssertions({ sessions, slug, key });
  const violated = violations(assertions);
  return { slug, key, cold: violated.length === 0, assertions, violated };
}

// ---------------------------------------------------------------------------
// The product path. Every mutation below is a real product surface.
// ---------------------------------------------------------------------------

/** `@chief/chiefing` is the product's own client — the same one `apps/web` and
 *  `scripts/start-stack.ts` use. Imported dynamically so an unbuilt workspace
 *  refuses with a sentence instead of a module-resolution stack. */
async function loadChiefing() {
  try {
    return await import("@chief/chiefing");
  } catch (error) {
    throw new Refusal(
      "chiefing-unbuilt",
      `cannot load @chief/chiefing (${error}) — run 'bun install --frozen-lockfile' so its dist exists`,
    );
  }
}

/**
 * Run a `chief` verb INSIDE the company directory.
 *
 * Every operator verb this file drives — `create`, `actuate`, `rm` — acts on
 * `paths::current_dir()` and takes no company argument, so the working
 * directory IS the company selector. Passing it explicitly, rather than
 * relying on this process's own cwd, is what keeps one harness able to talk
 * about one company without ever chdir-ing itself.
 */
function runChiefd(dir, args, extraEnv) {
  return spawnSync(CHIEF_BIN, args, {
    cwd: dir,
    encoding: "utf8",
    env: { ...process.env, ...extraEnv },
  });
}

/**
 * The teardown ledger: what this run created, and nothing else.
 *
 * Teardown reads only this. It cannot reach a process or a company it did not
 * make, which is the property that matters more than any convenience.
 */
function createdLedger() {
  /** @type {{ companyDir?: string, sessions?: { company: string, actuator: string },
   *           actuatorPid?: number, actuatorArgv?: string }} */
  const ledger = {};
  return ledger;
}

/**
 * Stop exactly what this run started.
 *
 * `chief rm` is the product's own counterpart to `create`: it stops the
 * company, deletes its durable state and drops the beacond row last. The
 * actuator is signalled by the PID this file spawned, and ONLY while that PID's
 * argv still matches — a recycled PID belongs to somebody else.
 */
function teardown(created, options, report) {
  const outcomes = [];
  if (created.actuatorPid !== undefined) {
    const stillOurs = processTable().find(
      (entry) => entry.pid === created.actuatorPid && entry.args === created.actuatorArgv,
    );
    if (!stillOurs) {
      outcomes.push(`actuator pid ${created.actuatorPid}: already gone, or no longer ours — not signalled`);
    } else {
      process.kill(created.actuatorPid, "SIGTERM");
      outcomes.push(`actuator pid ${created.actuatorPid}: SIGTERM sent`);
    }
  }
  if (created.companyDir !== undefined) {
    // `rm` takes no company: it removes THIS DIRECTORY's.
    const removed = runChiefd(created.companyDir, ["rm", "--yes"], {
      TEAM_LAUNCHER_TMUX_SOCKET: options.socket,
    });
    outcomes.push(
      `chief rm (in ${created.companyDir}): exit ${removed.status}${removed.status === 0 ? "" : ` — ${String(removed.stderr).trim()}`}`,
    );
    // Only checkable once the company had a name to be called by; a run that
    // refused before `create` has no session to look for and says so.
    if (created.sessions === undefined) {
      outcomes.push("tmux: no session was ever named for this company");
    } else {
      const named = new Set([created.sessions.company, created.sessions.actuator]);
      const leftovers = tmuxSessions(options.socket).filter((name) => named.has(name));
      outcomes.push(
        leftovers.length === 0
          ? "tmux: no session left for this company"
          : `tmux: STILL PRESENT ${leftovers.join(", ")} — left standing rather than killed blind`,
      );
    }
  }
  report.teardown = outcomes;
}

// ---------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------

async function measure(options, report) {
  // Preconditions first, and before the teardown ledger exists: a box that
  // cannot host the run refuses having created nothing to clean up.
  report.preconditions = checkPreconditions(options);

  const chiefing = await loadChiefing();
  const created = createdLedger();
  report.created = created;

  // A COMPANY IS A DIRECTORY, so this run mints one. `mkdtemp` gives a name
  // nothing has ever occupied, which is what makes the directory half of the
  // cold proof below a fact rather than a hope.
  const dir = mkdtempSync(join(options.parent, `${options.namePrefix}-`));
  report.companyDir = dir;

  // ---- cold proof I: the directory, before anything is created or timed ---
  report.cold = proveDirectoryCold(dir, options);
  if (!report.cold.cold) {
    throw new Refusal(
      "not-cold",
      [
        "REFUSING TO REPORT A NUMBER — the start is not cold.",
        "A warm stack answers instantly and the number would be a confident lie.",
        ...report.cold.violated.map((entry) => `  ${entry.claim} — observed: ${entry.observed.join(", ")}`),
      ].join("\n"),
    );
  }

  // ---- LEG A: cold company, cold actuator, cold CEO -----------------------
  const createStartedMs = nowMs();
  // `create` takes a NAME and a vision, and makes the company in the directory
  // it is run in. The slug is genesis's decision, never this file's.
  const createdCompany = runChiefd(dir, ["create", `${options.namePrefix} probe`, "Measure time to pane."], {
    TEAM_LAUNCHER_TMUX_SOCKET: options.socket,
    ...(options.wakeLog ? { RUST_LOG: "info,chiefd_daemon=debug,chiefd_host=debug" } : {}),
  });
  const createDoneMs = nowMs();
  if (createdCompany.status !== 0) {
    throw new Refusal(
      "create-refused",
      `chief create refused: ${String(createdCompany.stderr).trim() || String(createdCompany.stdout).trim()}`,
    );
  }
  created.companyDir = dir;

  // THE RENDEZVOUS, not the registry. A process standing inside a company
  // directory reads `<dir>/.chief/run/daemon.json` — it carries the daemon's
  // url AND the company key, and `readDaemonRendezvous` proves the record
  // describes THIS directory before returning it, which is what makes a copied
  // project unable to point one company's client at another's daemon. beacond
  // answers the box-wide question and is not on this path.
  const rendezvous = chiefing.readDaemonRendezvous(dir);
  if (!rendezvous || !rendezvous.url) {
    throw new Refusal(
      "no-rendezvous",
      `chief create returned 0 but ${join(dir, ".chief", "run", "daemon.json")} names no running daemon`,
    );
  }
  const client = new chiefing.ChiefdClient({ url: rendezvous.url });

  // The CEO's id is chiefd's decision, read off the root department's head —
  // never reconstructed. Assuming it was `executive-ceo` (it is `ceo`) once
  // made every company refuse `unknown-person` and reach no running CEO at all.
  // The company key is SERVED on the rendezvous. It used to be rebuilt here as
  // a composite of the slug and a data root, by handing `ChiefdClient` a `root`; that
  // identity has one producer now and this is a reader.
  const manifest = await client.manifest.readManifest(rendezvous.key);
  if (!manifest) {
    throw new Refusal("no-manifest", `the company in ${dir} was created but its manifest could not be read back`);
  }
  const slug = manifest.slug;
  if (!slug) {
    throw new Refusal("no-slug", `the company in ${dir} was created but its manifest names no slug`);
  }
  const ceoPersonId = manifest.departments[manifest.rootDepartmentId]?.headPersonId;
  if (!ceoPersonId) {
    throw new Refusal(
      "no-ceo",
      `the company in ${dir} was created but its manifest names no head on '${ROOT_DEPARTMENT_ID}' — there is nobody to bring up`,
    );
  }

  // ---- cold proof II: nothing has PAINTED this company --------------------
  // The first moment the slug and the key both exist, and the last moment
  // before this run starts an actuator. `create` paints no panes, so a session
  // by either of these names belongs to somebody else's actuator.
  const sessions = sessionNamesFor({ slug, key: rendezvous.key });
  created.sessions = sessions;
  report.coldSessions = proveSessionsCold({ slug, key: rendezvous.key }, options);
  if (!report.coldSessions.cold) {
    throw new Refusal(
      "not-cold",
      [
        "REFUSING TO REPORT A NUMBER — this company is already being actuated.",
        "`chief create` paints no panes, so a session by this name is another actuator's.",
        ...report.coldSessions.violated.map(
          (entry) => `  ${entry.claim} — observed: ${entry.observed.join(", ")}`,
        ),
      ].join("\n"),
    );
  }

  // Genesis states CEO-only intent with NOBODY actuating (genesis.rs), and
  // intent stated while nobody holds the lease is accepted and then silently
  // lost for ever. So the order below is the product: actuator started ->
  // lease held -> CEO-only intent -> observe. Re-stating the intent after the
  // actuator is up is not belt-and-braces, it is the only statement that can
  // take effect.
  // `actuate` takes no company either: it runs THIS DIRECTORY's people, so the
  // cwd is the selector and the argv names nothing to match on. Its log is the
  // company's own disposable runtime state, beside the daemon's.
  const actuatorArgv = `${CHIEF_BIN} actuate`;
  const runDir = join(dir, ".chief", "run");
  mkdirSync(runDir, { recursive: true });
  const actuatorLogPath = join(runDir, "actuator.log");
  const actuatorLog = openSync(actuatorLogPath, "a");
  const actuatorSpawnedMs = nowMs();
  const actuator = spawn(CHIEF_BIN, ["actuate"], {
    cwd: dir,
    stdio: ["ignore", actuatorLog, actuatorLog],
    env: {
      ...process.env,
      TEAM_LAUNCHER_TMUX_SOCKET: options.socket,
      ...(options.wakeLog ? { RUST_LOG: "info,chief_cli=debug" } : {}),
    },
  });
  created.actuatorPid = actuator.pid;
  created.actuatorArgv = actuatorArgv;
  report.actuator = { pid: actuator.pid, log: actuatorLogPath, socket: options.socket };

  // A LEASE IS NOT A PROCESS. Three independent facts, all required:
  //   1. the PID this file spawned is still alive;
  //   2. tmux itself answers on the socket the actuator was pinned to;
  //   3. chiefd's ownership row is `active`.
  // A timeout that does not say WHICH fact was missing costs a whole run to
  // diagnose, and they have completely different causes: a dead actuator or a
  // broken tmux. The last observation is kept so the refusal can name it — and
  // it is what produced the trace above, because the first version of this
  // wait ALSO required the lease and could therefore never return.
  /** @type {{ note: string } | { alive: boolean, serverAnswers: boolean, ownershipStatus: string, socketName: string | null }} */
  let lastReadiness = { note: "no sample completed" };
  const ready = await sampleUntil(
    async () => {
      const alive = processTable().some((entry) => entry.pid === actuator.pid);
      const serverAnswers = tmuxAnswers(options.socket);
      // Read, reported, and NOT required — see the header. At this point in the
      // sequence `released` is the correct answer, every time.
      const ownership = await client.runtime.readOwnership(rendezvous.key);
      lastReadiness = {
        alive,
        serverAnswers,
        ownershipStatus: ownership.status,
        socketName: ownership.socketName ?? null,
      };
      return alive && serverAnswers
        ? { alive, serverAnswers, ownershipStatus: ownership.status }
        : undefined;
    },
    { intervalMs: options.intervalMs, boundMs: options.boundMs, startedAtMs: actuatorSpawnedMs },
  );
  if (ready.timedOut) {
    report.outcome = {
      status: "TIMEOUT",
      where: "actuator readiness (process alive + tmux answering)",
      lastObserved: lastReadiness,
      boundMs: options.boundMs,
      samples: ready.samples,
      note: `the actuator this run spawned (pid ${actuator.pid}) never satisfied both; see ${actuatorLogPath}`,
    };
    return 3;
  }
  report.actuator.readiness = ready.found;

  // The clock for leg A. It used to start at an explicit
  // `client.rows.prepareCeoOnly(...)` POST, whose committed answer was checked
  // here; chief-home-is-cwd §4c deleted that route with the daemon-side CEO
  // boot, and genesis records the same start decision itself, so the intent is
  // already durable and the measurable interval is "actuator ready -> pane".
  const ceoIntentAtMs = nowMs();

  // THE LEASE, now that it can mean something. `claim_ownership` runs inside
  // `launch_runtime`, so the row going `active` is the instant the runtime
  // COMMITS TO PROJECTING — a real milestone between stating intent and seeing
  // a pane, and the one this file used to wait for before anything could
  // possibly have set it. Not a gate: a timeout here is
  // recorded and the pane wait still runs, because a lease that never lands
  // while a pane does is itself worth seeing.
  const leaseActive = await sampleUntil(
    async () => ((await client.runtime.readOwnership(rendezvous.key)).status === "active" ? true : undefined),
    { intervalMs: options.intervalMs, boundMs: options.boundMs, startedAtMs: ceoIntentAtMs },
  );
  report.lease = leaseActive.timedOut
    ? { observed: false, afterIntentMs: null, note: "the ownership row never went active within the bound" }
    : { observed: true, afterIntentMs: (leaseActive.atMs ?? ceoIntentAtMs) - ceoIntentAtMs };

  const ceoPane = await sampleUntil(
    () => livePaneFor(tmuxPanes(options.socket), sessions.company, ceoPersonId),
    { intervalMs: options.intervalMs, boundMs: options.boundMs, startedAtMs: ceoIntentAtMs },
  );
  if (ceoPane.timedOut) {
    report.outcome = {
      status: "TIMEOUT",
      where: "leg A — CEO pane",
      boundMs: options.boundMs,
      samples: ceoPane.samples,
      note: "no live pane carrying the CEO's @organization_person_id inside the bound",
    };
    return 3;
  }

  report.legA = {
    what: "cold company -> CEO running in a pane",
    coldness:
      "COLD — this DIRECTORY held no company and no daemon served it when the run began, " +
      "and nothing had painted a session for it after create (see cold.assertions and coldSessions.assertions)",
    ceoPersonId,
    createCommandMs: createDoneMs - createStartedMs,
    actuatorReadyMs: ready.atMs - actuatorSpawnedMs,
    ceoIntentToPaneMs: ceoPane.atMs - ceoIntentAtMs,
    createToPaneMs: ceoPane.atMs - createStartedMs,
    pane: ceoPane.found,
    samples: ceoPane.samples,
  };

  // ---- LEG B: TIME TO PANE, the headline ----------------------------------
  const departmentId = `${slug}-latency`;
  const headPersonId = `${departmentId}-head`;
  const logPath = daemonLogPath(dir);
  const logSizeBeforeCommit = existsSync(logPath) ? readFileSync(logPath, "utf8").length : 0;

  const requestSentMs = nowMs();
  const outcome = await client.staffing.createDepartment(
    rendezvous.key,
    departmentId,
    ROOT_DEPARTMENT_ID,
    "Latency",
    {
      kind: "hire-new",
      personId: headPersonId,
      name: "Latency Head",
      title: "Head of Latency",
      mandate: "Exist, so that the time until you exist can be measured.",
      personKind: "head",
      employmentState: "active",
      activation: "resident",
      tools: [],
      prompts: [],
    },
    { kind: "person", personId: ceoPersonId },
  );
  const commitAckMs = nowMs();
  if (!("applied" in outcome)) {
    throw new Refusal(
      "department-refused",
      `create-department refused: ${outcome.refused} — ${outcome.detail}`,
    );
  }

  const headPane = await sampleUntil(
    () => livePaneFor(tmuxPanes(options.socket), sessions.company, headPersonId),
    { intervalMs: options.intervalMs, boundMs: options.boundMs, startedAtMs: commitAckMs },
  );

  // The wake instant, when it is observable at all. This is the harness's own
  // observation time of the first NEW daemon log line matching the converge
  // pattern — NOT the daemon's own timestamp, and therefore no finer than the
  // sampling interval. Absent `--wake-log` the daemon does not emit at a level
  // that makes this visible, and the field says so instead of guessing.
  /** @type {{ observed: false, reason: string } | { observed: true, line: string, source: string, caveat: string }} */
  let wake = { observed: false, reason: "not requested — pass --wake-log to raise the daemon's log level" };
  if (options.wakeLog) {
    const grown = existsSync(logPath) ? readFileSync(logPath, "utf8").slice(logSizeBeforeCommit) : "";
    const line = grown.split("\n").find((entry) => WAKE_LOG_PATTERN.test(entry));
    wake = line
      ? { observed: true, line: line.trim(), source: logPath, caveat: "harness observation time, not the daemon's timestamp" }
      : { observed: false, reason: `no line matching ${WAKE_LOG_PATTERN} appeared in ${logPath} after the commit` };
  }
  report.wake = wake;

  if (headPane.timedOut) {
    report.outcome = {
      status: "TIMEOUT",
      where: "leg B — head pane (TIME TO PANE)",
      boundMs: options.boundMs,
      samples: headPane.samples,
      note: [
        `no live pane carrying @organization_person_id == '${headPersonId}' within the bound.`,
        "This is a TIMEOUT, not a latency. The number you want is not in this run.",
      ].join(" "),
    };
    return 3;
  }

  report.legB = {
    what: "department created -> its head running in a tmux pane (TIME TO PANE)",
    coldness: [
      "WARM ACTUATOR BY CONSTRUCTION — a pane cannot exist without an actuator,",
      "so this leg starts from the company leg A just brought up. That is the real",
      "operator scenario; the cold claim in this report belongs to leg A.",
    ].join(" "),
    departmentId: outcome.departmentId,
    headPersonId,
    requestSentAt: isoAt(requestSentMs),
    commitAckAt: isoAt(commitAckMs),
    paneAt: isoAt(headPane.atMs),
    requestRoundTripMs: commitAckMs - requestSentMs,
    // DERIVED: the write is committed by the time the route answers, so the ack
    // is the latest instant the commit can have happened at. The request-sent
    // instant is reported beside it so the reader can see the whole envelope.
    commitAckToPaneMs: headPane.atMs - commitAckMs,
    requestSentToPaneMs: headPane.atMs - requestSentMs,
    pane: headPane.found,
    samples: headPane.samples,
  };
  report.outcome = { status: "MEASURED" };
  return 0;
}

function renderHuman(report) {
  const lines = [];
  lines.push("");
  lines.push("TIME TO PANE — cold-start latency");
  lines.push("=================================");
  lines.push(`sampling interval : ${report.sampling.intervalMs} ms  (no delta below is finer than this)`);
  lines.push(`bound per wait    : ${report.sampling.boundMs} ms  (a bound reached is reported as TIMEOUT, never as a latency)`);
  lines.push(`tmux socket       : ${report.sampling.socket}`);
  lines.push("");

  const renderAssertions = (assertions) => {
    for (const entry of assertions) {
      lines.push(
        `  ${entry.observed.length === 0 ? "ok  " : "FAIL"} ${entry.claim}` +
          `${entry.observed.length ? ` — ${entry.observed.join(", ")}` : ""}`,
      );
    }
  };

  if (report.cold) {
    lines.push(`COLD PROOF for '${report.cold.dir}' — ${report.cold.cold ? "COLD" : "NOT COLD"}`);
    renderAssertions(report.cold.assertions);
    // The session half is only reachable once the company has a name, so a run
    // that refused earlier says so rather than printing a silent nothing.
    if (report.coldSessions) {
      lines.push(`  (after create, as '${report.coldSessions.slug}' / key ${report.coldSessions.key})`);
      renderAssertions(report.coldSessions.assertions);
    } else {
      lines.push("  --   the session half was never reached (no company was created)");
    }
    lines.push(`  context: ${report.cold.context.actuatorProcessesOnHost} actuator process(es) and ${report.cold.context.tmuxSessionsOnSocket} tmux session(s) on this host, none of them this company's`);
    lines.push("");
  }

  if (report.legA) {
    lines.push("LEG A — cold company to a running CEO");
    lines.push(`  ${report.legA.coldness}`);
    lines.push(`  chief create            ${report.legA.createCommandMs} ms`);
    lines.push(`  actuator became ready    ${report.legA.actuatorReadyMs} ms`);
    lines.push(`  CEO intent -> lease      ${report.lease?.observed ? `${report.lease.afterIntentMs} ms   (the runtime commits to projecting)` : `not observed — ${report.lease?.note ?? "not sampled"}`}`);
    lines.push(`  CEO intent -> CEO pane   ${report.legA.ceoIntentToPaneMs} ms   (${report.legA.samples} samples)`);
    lines.push(`  create -> CEO pane       ${report.legA.createToPaneMs} ms`);
    lines.push(`  observed pane            ${report.legA.pane.paneId} in ${report.legA.pane.session} (pid ${report.legA.pane.panePid})`);
    lines.push("");
  }

  if (report.legB) {
    lines.push("LEG B — department created to its head in a pane  <-- THE NUMBER");
    lines.push(`  ${report.legB.coldness}`);
    lines.push(`  create-department round trip   ${report.legB.requestRoundTripMs} ms`);
    lines.push(`  commit ack -> head pane        ${report.legB.commitAckToPaneMs} ms   (${report.legB.samples} samples)`);
    lines.push(`  request sent -> head pane      ${report.legB.requestSentToPaneMs} ms`);
    lines.push(`  observed pane                  ${report.legB.pane.paneId} in ${report.legB.pane.session}, @organization_person_id=${report.legB.pane.personId}`);
    lines.push(`  wake instant                   ${report.wake.observed ? report.wake.line : `not observed — ${report.wake.reason}`}`);
    lines.push("");
  }

  if (report.outcome) {
    lines.push(`OUTCOME: ${report.outcome.status}`);
    if (report.outcome.status === "TIMEOUT") {
      lines.push(`  at: ${report.outcome.where}`);
      if (report.outcome.lastObserved) {
        lines.push(`  last observed: ${JSON.stringify(report.outcome.lastObserved)}`);
      }
      lines.push(`  ${report.outcome.note}`);
    }
    lines.push("");
  }

  if (report.teardown) {
    lines.push("TEARDOWN — only what this run created");
    for (const entry of report.teardown) lines.push(`  ${entry}`);
    lines.push("");
  }

  lines.push("COMPARING BEFORE AND AFTER");
  lines.push("  'after' is this build. 'before' needs an OLD chief + chiefd pair at");
  lines.push("  ~/.chief/bin: CHIEFD_REACTIVE_FALLBACK_FLOOR_MS fails closed below 60s and");
  lines.push("  cannot reproduce the old ~1s tick. The pair is only a comparison if the host,");
  lines.push("  socket, data root and sampling interval are all unchanged.");
  return lines.join("\n");
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error instanceof Refusal ? error.message : error}\n`);
    return 2;
  }
  // The report GROWS as the run learns things — `cold`, then `legA`, then
  // `legB`, then `teardown` — and each section is only meaningful once the step
  // that produced it has run. An open record is the honest shape for that; a
  // fixed one would need every field pre-declared as absent, which reads as
  // "measured nothing" rather than "has not happened yet".
  /** @type {Record<string, any>} */
  const report = {
    sampling: { intervalMs: options.intervalMs, boundMs: options.boundMs, socket: options.socket },
    startedAt: isoAt(nowMs()),
  };
  let code = 0;
  try {
    code = await measure(options, report);
  } catch (error) {
    if (error instanceof Refusal) {
      report.outcome = { status: "REFUSED", code: error.code, detail: error.message };
      code = error.code === "not-cold" || error.code === "precondition-failed" || error.code === "bad-argument" ? 2 : 4;
    } else {
      report.outcome = { status: "ERROR", detail: String(error && error.stack ? error.stack : error) };
      code = 1;
    }
  } finally {
    if (!options.keep && report.created) {
      try {
        teardown(report.created, options, report);
      } catch (error) {
        report.teardown = [`teardown itself failed: ${error}`];
      }
    } else if (options.keep) {
      report.teardown = ["--keep: nothing torn down; the company and its actuator are still running"];
    }
  }

  if (options.json) {
    process.stdout.write(JSON.stringify(report, undefined, 2) + "\n");
  } else {
    process.stdout.write(renderHuman(report) + "\n");
    if (report.outcome && report.outcome.status !== "MEASURED") {
      process.stderr.write(`\n${report.outcome.detail ?? report.outcome.note ?? ""}\n`);
    }
  }
  return code;
}

process.exitCode = await main();
