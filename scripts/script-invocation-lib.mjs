// EVERY FILE UNDER `scripts/` IS INVOKED BY SOMETHING, OR IT IS NOT HERE.
//
// WHAT WENT WRONG
// ---------------
// `scripts/orphanable-spawner-scan.ts` was a correct spawn scanner carrying a
// correct allowlist, and nothing in this repo invoked it: no `package.json`
// script, no workflow step, no gate driver. Its only test lived in the parked
// `tests/` corpus, which runs in no lane. It had been dark long enough that its
// allowlist named files the repo no longer had, and nobody could have known,
// because the instrument that would have said so was the instrument that was
// not running.
//
// That is worse than the #963 shape (a stale allowlist row a file move
// orphaned) and worse than the `test:e2e-park` shape (a wired guard nobody put
// on the checklist). Both of those at least ran somewhere. This one produced
// exactly the same outcome as a deleted file while still costing every reader
// who found it the time to work out whether it mattered.
//
// THE RULE
// --------
// A file under `scripts/` is either reachable from something that runs it, or
// it carries a register row naming the human who runs it and why. There is no
// third state. A file in neither set FAILS BY NAME.
//
// BOTH SIDES ARE DERIVED
// ----------------------
// The inventory is a directory walk. The invoked set is a transitive closure
// from the four things that actually start work in this repo — every
// `package.json`'s `scripts` table, every `.github/workflows/*.yml`, every
// `vitest.config.ts` (its `globalSetup`/`setupFiles` really do run scripts),
// and `turbo.json` — plus the guard corpus `scripts/guard-count.mjs` already
// derives, plus the operator entrypoints registered below. Nothing here types a
// list of scripts. A script added to a workflow becomes invoked with no edit to
// this file, and a script whose last caller is deleted becomes unrun the same
// way.
//
// WHY THE CLOSURE READS COMMENT-STRIPPED TEXT
// -------------------------------------------
// This repo's scripts explain themselves at length, and those explanations name
// sibling scripts constantly — `guard-wiring-manifest.mjs` alone mentions
// `gate-matrix.sh` three times without running it once. A scan that counted a
// mention as an invocation would have reported `orphanable-spawner-scan.ts` as
// invoked, because `reactive-scan.ts` discusses it twice in prose. Comments come
// out before anything is matched, in every language this tree uses, so a
// reference has to survive in code to count.
//
// THE REGISTER IS CHECKED BOTH WAYS
// ---------------------------------
// `OPERATOR_ENTRYPOINTS` is the residue: scripts a human types by name — the
// merger's gate driver, the deploy chain, the backup tool, the scaffolder. Each
// row states who runs it and why. A row naming a file that no longer exists
// FAILS and says to delete it; a row whose file has since become genuinely
// invoked FAILS and says to delete it. The same bidirectional discipline
// `spawn-program-absolute-lib.mjs` and `sql-only-state.test.mjs` use, copied
// rather than reinvented.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";

import { deriveAllGuards } from "./guard-count.mjs";

import { skipSet } from "./tree-walk-lib.mjs";

/** Extensions a file under `scripts/` must have to be something that RUNS.
 * `.md` (this directory's two READMEs) and `.txt` (the cargo-log fixtures) are
 * data, not programs, and are deliberately not subjects. */
export const SCRIPT_EXTENSIONS = [".mjs", ".js", ".ts", ".sh", ".py"];

/** Directory names never walked, in the inventory or in the closure. */
const SKIP_DIRECTORIES = skipSet();

/** A floor on the inventory. A walk that finds fewer files than this did not
 * find a small tree; it found the wrong directory, and "no unrun scripts" from
 * a scan that read nothing is the exact answer this whole file exists to
 * refuse. */
export const MIN_PLAUSIBLE_SCRIPT_FILES = 60;

/** A floor on the closure. This repo wires most of `scripts/` through CI; a
 * closure that reaches almost nothing means the roots stopped resolving, which
 * would report the entire tree as unrun rather than reporting a defect. */
export const MIN_PLAUSIBLE_INVOKED = 40;

function walk(dir, out, root) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return out;
  }
  for (const name of entries) {
    if (SKIP_DIRECTORIES.has(name)) continue;
    const full = join(dir, name);
    let stat;
    try {
      stat = statSync(full);
    } catch {
      continue;
    }
    if (stat.isDirectory()) walk(full, out, root);
    else if (SCRIPT_EXTENSIONS.some((ext) => name.endsWith(ext))) out.push(relative(root, full).split(sep).join("/"));
  }
  return out;
}

/** Every runnable file under `scripts/`, repo-relative and sorted. The
 * inventory side, straight off the disk. */
export function scriptInventory(repoRoot) {
  return walk(join(repoRoot, "scripts"), [], repoRoot).sort();
}

/**
 * Strip comments so a script that DISCUSSES a sibling is not read as one that
 * RUNS it.
 *
 * The JavaScript half is a character scanner, not two `replace` calls, because
 * two `replace` calls are WRONG in a way that fails silently and enormously:
 * stripping `/* ... *\/` first lets any `/*` inside a `//` line comment open a
 * phantom block that runs to the next `*\/` anywhere in the file. Measured on
 * `scripts/test/refusal-taxonomy.test.mjs`, that ate 14,000 of 17,478
 * characters — including the `import` line naming the library the file exists
 * to test, which the audit then reported as unrun. A scanner that knows about
 * string literals has no such state to get wrong.
 *
 * The shell/YAML half stays deliberately conservative: only a whole-line `#`
 * comment and a ` # `-delimited trailing comment come out, because
 * `${var#prefix}`, `$#` and `#!/usr/bin/env` are all live shell a naive strip
 * would destroy.
 */
export function stripComments(text, file) {
  if (/\.(mjs|js|ts|tsx)$/.test(file)) return stripJsComments(text);
  if (/\.(sh|py|ya?ml)$/.test(file)) {
    return text
      .split("\n")
      .map((line) => (/^\s*#/.test(line) ? "" : line.replace(/\s#\s.*$/, "")))
      .join("\n");
  }
  return text;
}

/** Comment-strip JavaScript/TypeScript, tracking string literals so a `//` or
 * `/*` inside one is left alone. Newlines are preserved so a caller can still
 * report a line number against the stripped text. */
function stripJsComments(text) {
  let out = "";
  let index = 0;
  let quote;
  while (index < text.length) {
    const char = text[index];
    if (quote !== undefined) {
      out += char;
      if (char === "\\") {
        out += text[index + 1] ?? "";
        index += 2;
        continue;
      }
      if (char === quote) quote = undefined;
      index += 1;
      continue;
    }
    if (char === '"' || char === "'" || char === "`") {
      quote = char;
      out += char;
      index += 1;
      continue;
    }
    if (char === "/" && text[index + 1] === "/") {
      while (index < text.length && text[index] !== "\n") index += 1;
      continue;
    }
    if (char === "/" && text[index + 1] === "*") {
      index += 2;
      while (index < text.length && !(text[index] === "*" && text[index + 1] === "/")) {
        if (text[index] === "\n") out += "\n";
        index += 1;
      }
      index += 2;
      continue;
    }
    out += char;
    index += 1;
  }
  return out;
}

/** A runner word, then the file it runs: `bash scripts/x.sh`,
 * `node --test scripts/test/x.test.mjs`, `bun run scripts/x.ts`,
 * `. "$HERE/lib/company-process-count.sh"`. */
const RUNNER_INVOCATION =
  /(?:^|[\s"'`(&|;])(?:bash|sh|node|bun|python3?|source|\.)\s+(?:(?:--?[\w:-]+|run|test)\s+)*["'`]?((?:\$\{?\w+\}?\/|\.{1,2}\/)*[\w./-]*[\w.-]+\.(?:mjs|js|ts|sh|py))/g;

/** A script executed directly, with no runner word, at a command position —
 * `"$here/sweep-result-guard.sh" "$log"`, `./scripts/x.sh`,
 * `if GATE=$("$ROOT/scripts/canonical-writer-lease.sh" --verify)` — or bound to
 * a shell variable that is executed later, which is the same act written in two
 * lines: a script that sets `PROG="$HERE/thing.py"` and runs `"$PROG"` several
 * lines further down, which a scan matching only the run word would condemn as
 * dead code. `=` is therefore in the preceding
 * character class alongside the command-position separators. */
const DIRECT_EXECUTION =
  /(?:^|[\s(&|;=[]|\$\()["'`]?((?:\$\{?\w+\}?\/|\.{1,2}\/)[\w./-]*[\w.-]+\.(?:mjs|js|ts|sh|py))["'`]?(?=[\s"'`)\]]|$)/gm;

/** An ES module or CommonJS specifier. */
const MODULE_SPECIFIER = /(?:\bfrom\s*|\bimport\s*\(\s*|\brequire\s*\(\s*)["']([^"'\n]+)["']/g;

/** vitest's two script-running config keys, and everything in their array. */
const VITEST_SETUP = /\b(?:globalSetup|setupFiles)\s*:\s*\[([^\]]*)\]/g;

/** A BARE BASENAME in a quoted string — no directory separator at all.
 * `join(import.meta.dirname, "..", "guard-repo-path.sh")` and
 * `['browser-org-tools-check.mjs']` are how two live guards name the script
 * they spawn, and neither survives any runner-word test. It is safe to treat a
 * bare basename as a reference precisely BECAUSE it is bare: every basename
 * under `scripts/` is unique today, and a path-shaped mention in a DATA list —
 * `coverage-scope-gap.test.mjs`'s known-gap rows name
 * `"scripts/orphanable-spawner-scan.ts"` — always carries its directory, so it
 * never reaches this shape. That distinction is the whole reason the two are
 * separated: it is exactly what keeps a scanner listed as a known coverage gap
 * from being counted as a scanner somebody runs. */
const BARE_BASENAME = /["'`]([\w.-]+\.(?:mjs|js|ts|sh|py))["'`]/g;

/** Resolve one textual reference to a repo-relative `scripts/` path, or
 * `undefined`. Tries, in order: the literal path from the repo root, the path
 * from the referring file's own directory, and the same again after dropping a
 * leading shell variable (`$ROOT/`, `${HERE}/`). */
function resolveReference(raw, { baseDir, repoRoot, known }) {
  const stripped = raw.replace(/^["'`]|["'`]$/g, "");
  const withoutVariable = stripped.replace(/^\$\{?\w+\}?\//, "");
  for (const candidate of [
    resolve(repoRoot, stripped),
    resolve(baseDir, stripped),
    resolve(repoRoot, withoutVariable),
    resolve(baseDir, withoutVariable),
  ]) {
    const rel = relative(repoRoot, candidate).split(sep).join("/");
    if (known.has(rel)) return rel;
  }
  return undefined;
}

/**
 * Every `scripts/` file some text refers to IN A POSITION THAT RUNS IT.
 *
 * The distinction between a reference and an invocation is the whole point.
 * An earlier draft of this scan accepted any `scripts/...` path token, and it
 * reported `scripts/orphanable-spawner-scan.ts` — the very file that started
 * this packet — as invoked, because `scripts/test/coverage-scope-gap.test.mjs`
 * lists it as a known coverage gap. A register of things a checker CANNOT see
 * is not a caller. So a path has to appear after a runner word, at a command
 * position, in a module specifier, or in a vitest setup array; only a BARE
 * BASENAME is accepted on its own, for the reason given at `BARE_BASENAME`.
 *
 * COVERAGE BOUNDARY, STATED HONESTLY: this cannot tell a guard that SPAWNS a
 * script from one that only READS it as a subject —
 * `scripts/test/gate-matrix-sequence.test.mjs` reads `scripts/gate-matrix.sh`
 * line by line and executes none of it, and both look like a bare basename or a
 * runner-shaped path from here. The register's own staleness check is built
 * around that limit rather than pretending it away: only a ROOT invoker (a
 * `package.json` script, a workflow step, `turbo.json`, a vitest config, the
 * derived guard corpus) can retire a row.
 */
export function invocationsIn(text, { file, repoRoot, inventory }) {
  const stripped = stripComments(text, file ?? "");
  const known = new Set(inventory);
  const byBasename = new Map();
  for (const entry of inventory) {
    const base = basename(entry);
    byBasename.set(base, byBasename.has(base) ? null : entry);
  }
  const baseDir = file === undefined ? repoRoot : dirname(join(repoRoot, file));
  const found = new Set();
  const add = (raw) => {
    const resolved = resolveReference(raw, { baseDir, repoRoot, known });
    if (resolved !== undefined) found.add(resolved);
  };

  for (const match of stripped.matchAll(RUNNER_INVOCATION)) add(match[1]);
  for (const match of stripped.matchAll(DIRECT_EXECUTION)) add(match[1]);
  for (const match of stripped.matchAll(MODULE_SPECIFIER)) add(match[1]);
  for (const match of stripped.matchAll(VITEST_SETUP)) {
    for (const entry of match[1].matchAll(/["'`]([^"'`\n]+)["'`]/g)) add(entry[1]);
  }
  for (const match of stripped.matchAll(BARE_BASENAME)) {
    const resolved = byBasename.get(match[1]);
    if (resolved) found.add(resolved);
  }

  return [...found].sort();
}

/** The roots of the closure: everything in this repo that can start work
 * without another script having started it. Returned as `{ source, file, text }`
 * so a violation can name WHERE an invocation was found, not just that it was.
 *
 * `vitest.config.ts` is a root because `globalSetup` really does execute a
 * script — `scripts/test/assert-workspace-built.mjs` runs before every package
 * suite in this repo and is reachable from nowhere else. Leaving vitest out
 * would have condemned a live, load-bearing preflight as dead code. */
export function invocationRoots(repoRoot) {
  const roots = [];

  for (const manifest of workspaceManifests(repoRoot)) {
    let json;
    try {
      json = JSON.parse(readFileSync(join(repoRoot, manifest), "utf8"));
    } catch {
      continue;
    }
    for (const [name, command] of Object.entries(json.scripts ?? {})) {
      roots.push({ source: `${manifest} script "${name}"`, file: manifest, text: String(command) });
    }
  }

  const workflowsDir = join(repoRoot, ".github", "workflows");
  let workflows = [];
  try {
    workflows = readdirSync(workflowsDir).filter((name) => /\.ya?ml$/.test(name));
  } catch {
    workflows = [];
  }
  for (const name of workflows.sort()) {
    roots.push({
      source: `.github/workflows/${name}`,
      file: `.github/workflows/${name}`,
      text: readFileSync(join(workflowsDir, name), "utf8"),
    });
  }

  for (const config of vitestConfigs(repoRoot)) {
    roots.push({ source: config, file: config, text: readFileSync(join(repoRoot, config), "utf8") });
  }

  try {
    roots.push({ source: "turbo.json", file: "turbo.json", text: readFileSync(join(repoRoot, "turbo.json"), "utf8") });
  } catch {
    /* a tree with no turbo.json is caught by the vacuity floors, not here */
  }

  return roots;
}

/** Every `package.json` in the workspace — the root manifest plus each member
 * under `apps/` and `packages/`. Derived by walking those two directories one
 * level deep rather than parsing the workspaces glob, which is the same
 * git-grep-tier convention `guard-count.mjs` already uses for the same reason:
 * this repo's members are all exactly one level down. */
export function workspaceManifests(repoRoot) {
  const manifests = ["package.json"];
  for (const group of ["apps", "packages"]) {
    let members = [];
    try {
      members = readdirSync(join(repoRoot, group));
    } catch {
      continue;
    }
    for (const member of members.sort()) {
      const candidate = `${group}/${member}/package.json`;
      try {
        statSync(join(repoRoot, candidate));
        manifests.push(candidate);
      } catch {
        /* not a package */
      }
    }
  }
  return manifests;
}

/** Every `vitest.config.ts` in the workspace. */
export function vitestConfigs(repoRoot) {
  const configs = [];
  for (const group of ["apps", "packages"]) {
    let members = [];
    try {
      members = readdirSync(join(repoRoot, group));
    } catch {
      continue;
    }
    for (const member of members.sort()) {
      const candidate = `${group}/${member}/vitest.config.ts`;
      try {
        statSync(join(repoRoot, candidate));
        configs.push(candidate);
      } catch {
        /* no vitest in this member */
      }
    }
  }
  return configs;
}

/**
 * The transitive closure: every `scripts/` file something invokes, mapped to
 * the sources that invoke it.
 *
 * Seeded from `invocationRoots`, from the guard corpus `deriveAllGuards()`
 * already derives (a `scripts/test/*.test.mjs` file is run by every gate driver
 * and by CI without any workflow naming it individually — the derivation IS its
 * caller), and from `register`, because an operator entrypoint's own callees are
 * invoked exactly as truly as CI's are.
 */
export function deriveInvokedScripts(repoRoot, { inventory = scriptInventory(repoRoot), register = OPERATOR_ENTRYPOINTS } = {}) {
  const invokedBy = new Map();
  const frontier = [];
  const seed = (target, source, kind) => {
    if (!invokedBy.has(target)) invokedBy.set(target, []);
    const sources = invokedBy.get(target);
    if (!sources.some((entry) => entry.source === source)) sources.push({ source, kind });
    if (sources.length === 1) frontier.push(target);
  };

  for (const root of invocationRoots(repoRoot)) {
    for (const target of invocationsIn(root.text, { file: root.file, repoRoot, inventory })) {
      seed(target, root.source, "root");
    }
  }

  for (const guard of deriveAllGuards({
    guardTestDir: join(repoRoot, "scripts", "test"),
    workflowsDir: join(repoRoot, ".github", "workflows"),
    packageJsonPath: join(repoRoot, "package.json"),
  })) {
    if (guard.category !== "test.mjs") continue;
    seed(`scripts/test/${guard.name}`, "the derived guard corpus (scripts/guard-count.mjs)", "root");
  }

  for (const row of register) {
    if (inventory.includes(row.file)) seed(row.file, `OPERATOR_ENTRYPOINTS row: ${row.reason}`, "register");
  }

  while (frontier.length > 0) {
    const current = frontier.shift();
    let text;
    try {
      text = readFileSync(join(repoRoot, current), "utf8");
    } catch {
      continue;
    }
    for (const target of invocationsIn(text, { file: current, repoRoot, inventory })) {
      if (target === current) continue;
      seed(target, current, "script");
    }
  }

  return invokedBy;
}

/** A register row's identity, and what a violation names. */
export function rowKey(row) {
  return String(row.file);
}

/**
 * Scripts a HUMAN invokes by name, with the reason. Every row is a claim that
 * can go wrong in two directions, and both are checked: the file can disappear,
 * and the script can become genuinely wired — either way the row is stating
 * something false and must go.
 *
 * The bar for a row is not "somebody might run this one day". It is that the
 * script has a live operator and a stated occasion. Everything that failed that
 * bar was deleted rather than registered, because a register that accepts
 * "might be useful" rebuilds the class this file exists to close.
 */
export const OPERATOR_ENTRYPOINTS = [
  {
    file: "scripts/link-worktree-node-modules.sh",
    registeredOn: "2026-08-19",
    reason:
      "An engineer runs this once in a fresh worktree, before anything else, so `@chief/*` resolves to that worktree instead of to the shared checkout it borrowed node_modules from. Nothing can wire it: CI does a real `bun install` and has no second checkout to be confused with, and a gate that ran it would be a gate that rewrites node_modules.",
  },
  {
    file: "scripts/gate-matrix.sh",
    registeredOn: "2026-08-10",
    reason:
      "The merger's pre-push gate driver, invoked by hand on an authorized build host before every landing. It is the thing that runs the other gates; nothing runs it, on purpose, because a gate matrix CI started would be CI.",
  },
  {
    file: "scripts/promote-chiefd.sh",
    registeredOn: "2026-08-10",
    reason:
      "Promotes a built chiefd to the live per-box install and relaunches the daemon. An operator runs it on a real box after a release; CI has no live box to promote onto.",
  },
  {
    file: "scripts/chiefd-backup.sh",
    registeredOn: "2026-08-10",
    reason:
      "Snapshots beacond's registry and every per-company database on a live box. An operator runs it before a risky migration; there is nothing on a CI runner to back up.",
  },
  {
    file: "scripts/create-package.ts",
    registeredOn: "2026-08-10",
    reason:
      "The workspace-package scaffolder an engineer runs once when adding a package. It WRITES files, so a gate that ran it would be a gate that mutates the tree.",
  },
  {
    file: "scripts/start-stack.ts",
    registeredOn: "2026-08-10",
    reason:
      "`bun scripts/start-stack.ts` — the documented way to boot the local stack around a company (CLAUDE.md's own instruction). It starts long-lived services and never terminates on its own.",
  },
  {
    file: "scripts/release-packet-checkout.sh",
    registeredOn: "2026-08-10",
    reason:
      "#1004's release-time step: purge a released checkout's artifacts from the shared CARGO_TARGET_DIR before the checkout goes away. It runs when a build host reclaims a packet directory, which is an operator act.",
  },
  {
    file: "scripts/reap-orphaned-build-processes.mjs",
    registeredOn: "2026-08-10",
    reason:
      "#987's reap half. `pregate-orphan-check.mjs` refuses inside a gate and never kills; this one an operator runs on a shared host between gates. A gate that killed processes it did not start is the hazard #987 named.",
  },
  {
    file: "scripts/reap-test-tmp.sh",
    registeredOn: "2026-08-10",
    reason:
      "#84's /tmp sweep for a shared build host whose tmpfs is finite in inodes. An operator runs it when a box starts failing unrelated suites; a gate cannot delete scratch directories a concurrent gate is using.",
  },
  {
    file: "scripts/sweep.sh",
    registeredOn: "2026-08-10",
    reason:
      "The operator's run-wrapper for long remote commands: it logs, samples load, and refuses to report a count from a run that did not finish. It wraps gates; a gate cannot wrap it.",
  },
  {
    file: "scripts/browser-org-tools-check.mjs",
    registeredOn: "2026-08-10",
    reason:
      "THE browser acceptance check — a real Chromium against a real hosted agent and a real durable write. It needs a booted stack and a browser, which CI has neither of; `scripts/test/browser-check-runnable.test.mjs` is the wired half that keeps it loadable.",
  },
  {
    file: "scripts/e2e-web-company.sh",
    registeredOn: "2026-08-10",
    reason:
      "Boots one API-hosted company on a real box and proves the web surface can talk to its agents. It exists as a script precisely because the sequence failed repeatedly as one-shot remote commands; an operator runs it on a box with tmux and a stack, which CI has neither of.",
  },
  {
    file: "scripts/install-root-status-line.sh",
    registeredOn: "2026-08-10",
    reason:
      "Installs the live activity status line into the ROOT Pi layer so every plain Pi agent inherits it. It WRITES into an operator's Pi home outside this repo, which is the one thing no gate may ever do; it is run once per box, by hand.",
  },
  {
    file: "scripts/cold-start-latency.mjs",
    registeredOn: "2026-08-11",
    reason:
      "The TIME TO PANE instrument, typed by an operator on a real box when somebody needs to know how long a create-department takes to reach a running pane. It creates a real company, spawns a real actuator and paints real tmux panes, so wiring it into CI would make CI create companies; and it is measuring, not gating, so a red from it is a number nobody likes rather than a change that may not land.",
  },
];

/**
 * The whole verdict, as `{ inventory, invokedBy, unrun, staleRows, wiredRows }`.
 *
 * `unrun` is what fails the guard: a file under `scripts/` that nothing invokes
 * and no row claims. `staleRows` and `wiredRows` are the register's own two
 * failure directions.
 */
export function auditScriptInvocation(repoRoot, { register = OPERATOR_ENTRYPOINTS } = {}) {
  const inventory = scriptInventory(repoRoot);
  const invokedBy = deriveInvokedScripts(repoRoot, { inventory, register });
  const registered = new Set(register.map(rowKey));

  const unrun = inventory.filter((file) => !invokedBy.has(file) && !registered.has(file));

  const staleRows = register
    .filter((row) => !inventory.includes(row.file))
    .map((row) => `${rowKey(row)} — no such file under scripts/ today; delete this row`);

  // The register's second failure direction: a row claiming only a human runs
  // this file, when CI now does.
  //
  // Only a `root` invoker counts here — a `package.json` script, a workflow
  // step, `turbo.json`, a vitest config, or the derived guard corpus. A
  // reference from ANOTHER SCRIPT does not retire a row, because a sibling
  // script naming a file is exactly as likely to be a guard READING it as a
  // caller RUNNING it: `gate-matrix-sequence.test.mjs` reads
  // `scripts/gate-matrix.sh` line by line to lock its stage order and never
  // executes a byte of it. Retiring the row on that reference would have
  // deleted the only record of who actually runs the merger's gate driver.
  const wiredRows = register
    .filter((row) => (invokedBy.get(row.file) ?? []).some((entry) => entry.kind === "root"))
    .map(
      (row) =>
        `${rowKey(row)} — is now invoked by ${(invokedBy.get(row.file) ?? [])
          .filter((entry) => entry.kind === "root")
          .map((entry) => entry.source)
          .join(", ")}; delete this row, the script is wired`,
    );

  return { inventory, invokedBy, unrun, staleRows, wiredRows };
}

/** Rows that cannot be checked are not facts. Same shape check
 * `spawn-program-absolute-lib.mjs` applies to its own register. */
export function registerShapeViolations(register = OPERATOR_ENTRYPOINTS) {
  const violations = [];
  const seen = new Set();
  for (const row of register) {
    const key = rowKey(row);
    if (seen.has(key)) violations.push(`${key} — registered twice`);
    seen.add(key);
    if (!row.file || !row.file.startsWith("scripts/")) violations.push(`${key} — a row must name a file under scripts/`);
    if (!/^\d{4}-\d{2}-\d{2}$/.test(String(row.registeredOn ?? "")))
      violations.push(`${key} — no registration date`);
    if (String(row.reason ?? "").trim().length < 40)
      violations.push(`${key} — no written reason saying who runs it and when`);
  }
  return violations;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const repoRoot = process.argv[2] ?? resolve(dirname(new URL(import.meta.url).pathname), "..");
  const { inventory, invokedBy, unrun, staleRows, wiredRows } = auditScriptInvocation(repoRoot);
  console.log(`SCRIPT_FILES:${inventory.length}`);
  console.log(`INVOKED:${invokedBy.size}`);
  console.log(`UNRUN:${unrun.length}`);
  for (const file of unrun) console.log(`  x ${file}`);
  for (const row of [...staleRows, ...wiredRows]) console.log(`  ! ${row}`);
  process.exit(unrun.length + staleRows.length + wiredRows.length === 0 ? 0 : 1);
}
