// EVERY DETACHED SPAWN IN THIS TREE IS TRIAGED, AND THE TRIAGE IS CHECKED.
//
// WHAT THIS RULE IS ABOUT
// -----------------------
// This product spawns long-lived children: `chiefd`, `beacond`, Pi agents, tmux
// panes. When a child is DETACHED from its spawner's lifecycle — `detached:
// true` + `unref()` in Node/Bun, `.process_group(0)` in Rust,
// `nohup`/`setsid`/`disown`/`&` in shell — nothing in the kernel ties the
// child's death to the parent's. The child is then reaped only by whatever the
// spawner does on its way out.
//
// The reference failure is the write-db orphan incident: the spawns reaped
// their child from `process.on("exit"/"SIGINT"/"SIGTERM")`. Those handlers do
// not run when the spawner is SIGKILLed, and did not run under the test runner
// either. Orphaned services accumulated, every one reparented to pid 1, and
// stole CPU from the verification runs that were trying to find out why the
// verification runs were slow.
//
// A ROBUST watchdog survives the spawner being SIGKILLed, and only two shapes
// qualify: a child-side self-kill (the child polls its own parent and exits when
// reparented) or a supervisor-side reap keyed to a durable lease. Everything
// else is a hope.
//
// WHY THIS FILE IS `.mjs` AND NOT THE `.ts` PAIR IT REPLACES
// ---------------------------------------------------------
// The scanner and its allowlist used to be `scripts/orphanable-spawner-scan.ts`
// and `scripts/orphanable-spawner-allowlist.ts`, and NOTHING INVOKED THEM. No
// `package.json` script, no workflow, no gate driver; the only test that ran
// them sat in the parked `tests/` corpus, which runs in no lane. They had been
// dark long enough that the allowlist had accumulated rows naming files this
// repo no longer had — the #963 shape, in a scanner that could not report it.
//
// A `.ts` file under `scripts/` can only be run by `bun`, which is why it was
// never a guard: this repo's wired repo-invariant corpus is
// `scripts/test/*.test.mjs`, derived from a directory listing and run by `node
// --test` with no package.json indirection. Porting the rule into that corpus is
// what makes it RUN. It is the same lib+assertion split
// `spawn-program-absolute-lib.mjs` uses, deliberately copied rather than given a
// second shape.
//
// WHAT THIS IS NOT
// ----------------
// It is not a duplicate of `spawn-program-absolute-lib.mjs`, which shares the
// word "spawn" and nothing else. That one asks whether a program LITERAL is
// absolute, so the process that resolves is the process that runs. This one asks
// whether a DETACHED child has anything that reaps it. Different detectors,
// different scan roots, different verdicts; a tree can pass either and fail the
// other.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

import { skipSet } from "./tree-walk-lib.mjs";

/**
 * Directories holding code this repo ships or runs.
 *
 * The `.ts` original listed `src`, `extensions`, `.pi` and `conformance` as
 * well. The first three had not existed for months and `conformance/**` lost
 * its last scannable file to #1047 -- which is the quiet half of an unrun
 * scanner: a
 * scan root that resolves to nothing costs nothing and says nothing, so it
 * survives every deletion. `scanVacuity` below is what keeps this list honest
 * now — a root that stops matching files shows up as a collapsed file count
 * rather than as silence.
 */
export const SCAN_DIRS = ["apps", "packages", "scripts", "tests"];

const EXCLUDE_SEGMENTS = skipSet(["build"]);
const SCAN_EXTENSIONS = [".ts", ".tsx", ".js", ".mjs", ".cjs", ".rs", ".sh"];

/** This rule's own files define the spawn patterns as literals; they are not
 * spawn sites and must not scan themselves. */
const EXCLUDE_BASENAMES = new Set(["orphanable-spawner-lib.mjs", "orphanable-spawner.test.mjs"]);

/**
 * A detach marker: a line whose presence means a spawned child is severed from
 * its spawner's lifecycle and can therefore be orphaned.
 *
 * Foreground spawns are deliberately absent. `spawnSync`, `execSync`,
 * `.output()`, `.status()` and `.wait()` all block the parent, so the child
 * cannot outlive it — they are not the orphan class, and including them would
 * bury the five real sites under a hundred that cannot fail.
 */
export const DETACH_MARKERS = [
  {
    id: "js-detached",
    langs: ["js"],
    pattern: /detached\s*:\s*true/,
    what: "Node/Bun spawn with detached:true (new process group, severed from parent)",
  },
  {
    id: "rs-detached",
    langs: ["rs"],
    pattern: /\.process_group\s*\(\s*0\s*\)/,
    what: "Rust Command with .process_group(0) (child in a new process group — the safe-code setsid equivalent)",
  },
  { id: "sh-nohup", langs: ["sh"], pattern: /\bnohup\b/, what: "shell nohup (child ignores SIGHUP, survives parent)" },
  { id: "sh-setsid", langs: ["sh"], pattern: /\bsetsid\b/, what: "shell setsid (child in a new session)" },
  { id: "sh-disown", langs: ["sh"], pattern: /\bdisown\b/, what: "shell disown (child removed from the shell job table)" },
  {
    id: "sh-background",
    langs: ["sh"],
    pattern: /(^|[^&|>])&\s*(#.*)?$/,
    what: "shell backgrounded command (& at end of line)",
  },
  {
    id: "sh-tmux-detached",
    langs: ["sh"],
    pattern: /tmux\s+(new-session|new-window|split-window|respawn-pane)/,
    what: "tmux pane/session spawn (outlives the launcher)",
  },
];

/**
 * A robust watchdog token: its presence in a file proves a real parent-death
 * linkage that survives SIGKILL of the spawner. A `robust-watchdog` row names
 * one, and the scan verifies the file really contains it — so the allowlist
 * cannot claim a safety the code does not have.
 */
export const WATCHDOG_TOKENS = {
  // chiefd's docstore-only mode carries a child-side self-kill: the child polls
  // the pid in CHIEFD_STORE_WATCH_PID and exits when that process is gone,
  // however many forks removed (`docstore_only.rs`). A spawn opts in by setting
  // it, so its presence in the spawning file is real evidence.
  "chiefd-store-exit-with-parent": /CHIEFD_STORE_EXIT_WITH_PARENT/,
  // beacond's equivalent, added with the mechanism itself (#751 takeover). Both
  // of the spawns that needed it used to sit under ORPHANABLE with the note
  // "beacond has no CHIEFD_STORE_WATCH_PID equivalent today; giving it one is
  // the fix". It has one: `beacond::watchdog` polls the pid named here and
  // exits when it is gone, proven end to end in
  // `crates/beacond/tests/owner_death_watchdog.rs` by SIGKILLing a real owner.
  "beacond-exit-with-owner": /BEACOND_WATCH_PID/,
};

/**
 * A vocabulary nobody uses is the same defect as an allowlist row nobody
 * matches, one level up. The `.ts` original carried five tokens and used one:
 * `write-db-exit-with-parent` named a service deleted months ago,
 * `unit-shard-sibling-watchdog` named a CI sharder deleted with the bun:test
 * lane, and `beacond-sibling-watchdog` named an environment variable nothing in
 * this tree has ever set. All three read as available safety mechanisms to
 * anyone triaging a new spawn. `allowlistShapeViolations` now fails on a token
 * no row claims, so the table can only shrink to what is true.
 */
/**
 * The triage table. Every detached spawn site the scan finds must appear here
 * exactly once, classified with a written reason.
 *
 * Classifications:
 *   robust-watchdog          — a parent-death linkage that SURVIVES SIGKILL of
 *                              the spawner. `watchdog` names the required token
 *                              and the scan checks the file really has it.
 *   intentionally-long-lived — the child is MEANT to outlive its spawner. The
 *                              reason must say WHAT reaps it instead.
 *   orphanable               — no robust death-linkage. This is the live bug
 *                              class; a row here is a named exposure, not an
 *                              excuse.
 *
 * Line numbers are deliberately not stored — they drift, and a row that goes
 * stale on a line move is a row that trains people to edit the allowlist without
 * reading it. Matching is by `(file, marker)`.
 */
export const ORPHAN_SPAWNER_ALLOWLIST = [
  // =========================================================================
  // ROBUST WATCHDOG — the child kills itself when its spawner is gone, whether
  // the spawner exited or was SIGKILLed.
  // =========================================================================
  {
    file: "packages/testing/src/CompanyDaemon.ts",
    marker: "js-detached",
    classification: "robust-watchdog",
    watchdog: "chiefd-store-exit-with-parent",
    registeredOn: "2026-08-10",
    reason:
      "`@chief/testing`'s serve-only company daemon spawns `chiefd run --serve-only` detached with CHIEFD_STORE_EXIT_WITH_PARENT=1 and the harness pid in CHIEFD_STORE_WATCH_PID. It actuates nobody, and it self-exits when the suite process is gone.",
  },
  {
    file: "packages/testing/src/TmuxHostedCompanyDaemon.ts",
    marker: "js-detached",
    classification: "robust-watchdog",
    watchdog: "beacond-exit-with-owner",
    registeredOn: "2026-08-10",
    reason:
      "This file has two detached spawns and BOTH now arm a child-side self-kill: the chiefd half sets CHIEFD_STORE_EXIT_WITH_PARENT and the beacond half sets BEACOND_WATCH_PID. The row can claim robust-watchdog only because the weaker of the two stopped being weak — the token this row names is verified present in this exact file by the scan.",
  },

  // =========================================================================
  // INTENTIONALLY LONG-LIVED — the child is MEANT to outlive its spawner, and a
  // death linkage to that spawner would be the bug. Each reason says what reaps
  // it instead.
  // =========================================================================
  {
    file: "apps/chiefd/crates/chief-cli/src/reap/tests.rs",
    marker: "rs-detached",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-19",
    reason:
      "A TEST FIXTURE that builds the very thing `chief-cli`'s reap exists to stop. It spawns a group leader with `process_group(0)` so the leader's pgid is its pid, exactly as a tmux pane leader's is, and lets it fork children that escape — including a `setsid` child in a session of its own, which is how the nine live survivors of a `chief stop` escaped. The detachment IS the condition under test: a fixture whose child died with its parent would prove nothing. Every one of these is reaped inside the test that made it — `reap_process_groups` is the subject, and each test SIGKILLs and waits any leader it did not stop.",
  },
  {
    file: "packages/piing/extensions/company-stop.ts",
    marker: "js-detached",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-22",
    reason:
      "`/stop` hands a company teardown to the installed `chief stop`, and the detachment is the whole mechanism rather than an oversight. `chief stop` obeys an ordering law whose MIDDLE step kills the tmux session this extension is a pane of; a child in this pane's process group would be killed there, leaving the durable teardown committed and the daemon still running. So the child must outlive its spawner by design. It is not long-lived in the daemon sense: `chief stop` is a short sequence that exits on its own within seconds, and nothing needs to reap it. If it were linked to this pane's death the command could never work at all.",
  },

  {
    file: "apps/chiefd/crates/chief-cli/src/daemon.rs",
    marker: "rs-detached",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-10",
    reason:
      "`chief-cli` starts the installed `chiefd` in its own process group so a terminal hangup — which signals only the FOREGROUND process group — does not take the company down with the shell that started it. Stopped explicitly by `chief stop` through the recorded pid, never by this command's exit. `process_group(0)` rather than a `setsid` executable because macOS does not ship one.",
  },
  {
    // ONE row, because there is now one site: `spawn_detached` moved into the
    // `host-primitives` leaf and the two mirrored copies were deleted. Both
    // callers' reasons are kept, because they still differ and this row
    // explains the SITE rather than the crate that used to hold it.
    file: "apps/chiefd/crates/host-primitives/src/spawn.rs",
    marker: "rs-detached",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-10",
    reason:
      "`spawn_detached` starts a background worker whose exit is observed through an EXPIRING DURABLE LEASE and never through the handle (#61 states this as a requirement on the caller). Blocking on it would be the defect; a worker that dies without completing is reaped when its lease expires. Its caller `chief-cli` starts workers whose lease the writer phase committed: a throwing spawn is reaped by rollback_to_queued releasing the lease, a hung worker by the lease expiring. Reaped by the lease, not by parent death.",
  },
  {
    file: "scripts/promote-chiefd.sh",
    marker: "sh-nohup",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-10",
    reason:
      "`setsid nohup \"$START\" &` relaunches the live chiefd daemon detached from the one-shot promote script. The daemon MUST outlive the script; its company PID is reported by beacond and stopped explicitly by the NEXT promote.",
  },
  {
    file: "scripts/sweep.sh",
    marker: "sh-background",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-10",
    reason:
      "`cmd_run`'s load sampler runs in the background only for the duration of the wrapped command: its pid is captured and it is killed and waited unconditionally right after that command exits, whatever the exit code. A SIGKILL of sweep.sh itself mid-run would orphan it — a real, named window rather than a zero-risk site — but the normal path always reaps it by name.",
  },
  {
    file: "scripts/start-stack.ts",
    marker: "js-detached",
    classification: "intentionally-long-lived",
    registeredOn: "2026-08-10",
    reason:
      "`chief host` is the resident per-box company-lifecycle service, and tying it to the one-shot script that noticed it was missing would tear it down the moment that script exits — immediately. Guarded by a health probe so a second is never started, and a duplicate could not bind the port anyway. Stopped by the operator or by the box going away.",
  },

  // =========================================================================
  // ORPHANABLE — no death linkage that survives a SIGKILL of the spawner. A
  // row here is a named exposure, not an excuse. The CI Cargo children below
  // are normally waited by this wrapper, but a killed runner can leave them
  // until the job's process cleanup; they have no product watchdog and must
  // stay visible as an exposure.
  // =========================================================================
  {
    file: "scripts/cargo-test-workspace-shard.sh",
    marker: "sh-background",
    classification: "orphanable",
    registeredOn: "2026-08-12",
    reason:
      "The CI wrapper starts independent Cargo test targets in the background to overlap their test bodies, then waits for every recorded pid before it checks the combined exact floor. A SIGKILL of the wrapper can leave a Cargo child until the GitHub runner's job cleanup; this is a bounded CI-only exposure with no product watchdog, recorded here instead of hidden.",
  },
];

/** The identity of a site and of a row: they must be the same function, or the
 * two sides of the comparison are not comparable. */
export function rowKey(entry) {
  return `${entry.file} [${entry.marker}]`;
}

function langOf(path) {
  if (/\.(ts|tsx|js|mjs|cjs)$/.test(path)) return "js";
  if (path.endsWith(".rs")) return "rs";
  if (path.endsWith(".sh")) return "sh";
  return undefined;
}

function walk(dir, out, root) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return out;
  }
  for (const name of entries) {
    if (EXCLUDE_SEGMENTS.has(name)) continue;
    const full = join(dir, name);
    let stat;
    try {
      stat = statSync(full);
    } catch {
      continue;
    }
    if (stat.isDirectory()) walk(full, out, root);
    else if (SCAN_EXTENSIONS.some((ext) => name.endsWith(ext)) && !EXCLUDE_BASENAMES.has(name)) {
      out.push(relative(root, full).split(sep).join("/"));
    }
  }
  return out;
}

/** Every file under the scan roots this rule can have an opinion about. */
export function scannedFiles(repoRoot) {
  const files = [];
  for (const dir of SCAN_DIRS) walk(join(repoRoot, dir), files, repoRoot);
  return files.sort();
}

/**
 * Every detached spawn site under the scan roots, as
 * `{ file, line, marker, what, text }`.
 *
 * Comment lines are skipped and a trailing `//` comment is cut before matching,
 * because this rule's own prose — and half the headers in this tree — describe
 * `detached: true` in sentences. A doc-comment that mentions a marker is
 * documentation, not a spawn.
 */
export function findSpawnSites(repoRoot, files = scannedFiles(repoRoot)) {
  const sites = [];
  for (const file of files) {
    const lang = langOf(file);
    if (lang === undefined) continue;
    let content;
    try {
      content = readFileSync(join(repoRoot, file), "utf8");
    } catch {
      continue;
    }
    const lines = content.split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      const raw = lines[index];
      const trimmed = raw.trim();
      if (/^(\/\/|\*|\/\*|#)/.test(trimmed)) continue;
      const line = raw.replace(/\/\/.*$/, "");
      for (const marker of DETACH_MARKERS) {
        if (!marker.langs.includes(lang)) continue;
        if (!marker.pattern.test(line)) continue;
        // `detached: true` must be a real object property, not a quoted literal
        // in an assertion pinning the production source.
        if (marker.id === "js-detached") {
          if (/["'`]\s*detached/.test(line)) continue;
          if (/\b(toContain|toMatch|includes|expect|assert)\s*\(/.test(line)) continue;
        }
        sites.push({ file, line: index + 1, marker: marker.id, what: marker.what, text: trimmed });
        // One line is one site: `setsid ... &` is a single detachment, not both
        // a setsid site and a background site.
        break;
      }
    }
  }
  return sites;
}

/**
 * Sites against rows, both directions.
 *
 * `untriaged` — a detached spawn with no row. A new orphanable spawn must be a
 * decision somebody made, not something that arrived.
 * `stale` — a row matching no site today. It says to delete itself, by name,
 * because a row nobody can check is #963 again.
 */
export function compareSitesToAllowlist(sites, allowlist = ORPHAN_SPAWNER_ALLOWLIST) {
  const registered = new Set(allowlist.map(rowKey));
  const present = new Set(sites.map(rowKey));
  const seen = new Set();
  return {
    untriaged: [
      ...new Set(
        sites
          .filter((site) => !registered.has(rowKey(site)))
          .map((site) => `${site.file}:${site.line} [${site.marker}] ${site.text} — ${site.what}`),
      ),
    ].sort(),
    stale: allowlist
      .filter((row) => {
        const key = rowKey(row);
        if (seen.has(key)) return false;
        seen.add(key);
        return !present.has(key);
      })
      .map((row) => `${rowKey(row)} — matches nothing today; delete this row`)
      .sort(),
  };
}

/** A `robust-watchdog` row whose file does not actually contain the token it
 * claims. The allowlist may state a safety property; it may not invent one. */
export function unbackedWatchdogClaims(repoRoot, allowlist = ORPHAN_SPAWNER_ALLOWLIST) {
  const violations = [];
  for (const row of allowlist) {
    if (row.classification !== "robust-watchdog") continue;
    const token = WATCHDOG_TOKENS[row.watchdog];
    if (token === undefined) {
      violations.push(`${rowKey(row)} — claims watchdog "${row.watchdog}", which is not a known token`);
      continue;
    }
    let content = "";
    try {
      content = readFileSync(join(repoRoot, row.file), "utf8");
    } catch {
      continue; // a missing file is reported by the stale-row arm, not twice
    }
    if (!token.test(content)) {
      violations.push(`${rowKey(row)} — claims watchdog "${row.watchdog}", which the file does not contain`);
    }
  }
  return violations;
}

/** Rows that cannot be checked are not facts. */
export function allowlistShapeViolations(allowlist = ORPHAN_SPAWNER_ALLOWLIST) {
  const classifications = new Set(["robust-watchdog", "intentionally-long-lived", "orphanable"]);
  const markers = new Set(DETACH_MARKERS.map((marker) => marker.id));
  const violations = [];
  const seen = new Set();
  for (const row of allowlist) {
    const key = rowKey(row);
    if (seen.has(key)) violations.push(`${key} — registered twice`);
    seen.add(key);
    if (!markers.has(row.marker)) violations.push(`${key} — names a marker that does not exist`);
    if (!classifications.has(row.classification)) violations.push(`${key} — classification is outside the closed set`);
    if (row.classification === "robust-watchdog" && !row.watchdog)
      violations.push(`${key} — claims a robust watchdog but names no token`);
    if (row.classification !== "robust-watchdog" && row.watchdog)
      violations.push(`${key} — names a watchdog token but is not classified robust-watchdog`);
    if (!/^\d{4}-\d{2}-\d{2}$/.test(String(row.registeredOn ?? ""))) violations.push(`${key} — no registration date`);
    if (String(row.reason ?? "").trim().length < 60) violations.push(`${key} — no written reason`);
  }
  const claimed = new Set(allowlist.map((row) => row.watchdog).filter(Boolean));
  for (const token of Object.keys(WATCHDOG_TOKENS)) {
    if (!claimed.has(token)) {
      violations.push(
        `WATCHDOG_TOKENS."${token}" — no row claims this watchdog; delete it, an unclaimed token reads as an available safety mechanism that nothing in this tree provides`,
      );
    }
  }
  return violations;
}

/** Rows naming a path that is not in this tree. Reported separately from
 * `stale` so a MOVED file and a DELETED file read differently in the failure. */
export function allowlistRowsNamingMissingFiles(repoRoot, allowlist = ORPHAN_SPAWNER_ALLOWLIST) {
  return allowlist
    .filter((row) => {
      try {
        statSync(join(repoRoot, row.file));
        return false;
      } catch {
        return true;
      }
    })
    .map((row) => `${rowKey(row)} — registered path does not exist; delete this row`);
}

/**
 * Floors on what the scan actually read, so a clean answer from a scan that saw
 * nothing is impossible to report as evidence.
 *
 * The `.ts` original had none, and it needed them: three of its five scan roots
 * (`src`, `extensions`, `.pi`) had not existed for months, and the scan said
 * nothing about that because a missing root is silence, not an error. Returns
 * violation strings rather than throwing, so the guard reports them alongside
 * every other arm instead of aborting on the first.
 */
export function scanVacuity(repoRoot, files = scannedFiles(repoRoot), sites = findSpawnSites(repoRoot, files)) {
  const violations = [];
  if (files.length < 500) {
    violations.push(`only ${files.length} files were read across ${SCAN_DIRS.join(", ")} — the walk is broken, not the tree`);
  }
  for (const dir of SCAN_DIRS) {
    if (!files.some((file) => file.startsWith(`${dir}/`))) {
      violations.push(`scan root "${dir}" matched no files — it has been deleted or renamed; remove it from SCAN_DIRS`);
    }
  }
  if (sites.length < ORPHAN_SPAWNER_ALLOWLIST.length) {
    violations.push(
      `${sites.length} spawn sites found against ${ORPHAN_SPAWNER_ALLOWLIST.length} allowlist rows — the detectors cannot see less than the register claims`,
    );
  }
  return violations;
}
