// The retired pane env stamp stays retired: no production file may name it.
//
// # What was deleted, and why a name is worth a guard
//
// `ORG_CHIEFD_URL` carried ONE chiefd address per PROCESS. That is the right
// shape for exactly one deployment — one Pi process per tmux pane, one company
// per process — and it has no correct value at all in `apps/web`, which serves
// many companies from one server process. The failure it produced is the worst
// available shape: SILENT. A wrong daemon ANSWERS. It does not refuse, it does
// not 500, it does not time out — it commits the mutation into another
// company's database and returns 200.
//
// Every reader was ported to beacond, which answers per COMPANY
// (`organization-intercom.ts`, `team-ui.ts`),
// and #983 then deleted the two writers: the pane
// `EnvAssignment` in `converge_apply/cycle.rs` and the API-child entry in
// `converge_apply/api_host_profile.rs`.
//
// What remains at risk is a READER. A variable with no reader and no writer
// costs nothing until somebody reintroduces one, and the reintroduction is a
// one-line change that looks entirely reasonable in review — the shape is
// familiar and the defect it causes cannot be seen from the diff. So the guard
// bans the name in production CODE.
//
// # Comments are excluded, deliberately
//
// `beacond-port-single-definition.test.mjs` made this call first and it holds
// here for the same reason: a guard that forbade naming the mechanism in prose
// would delete the only record of why the rule exists. The ported readers
// (`organization-intercom.ts`, `team-ui.ts`) each
// carry a past-tense account of what the stamp was and how it failed, and that
// account is the most valuable documentation in those files. It is also the
// landing site for the engineer who finds the name in an old runbook or a
// stale shell environment and greps for it: a tree where the name returns
// nothing teaches that engineer nothing.
//
// Comments are STRIPPED before the scan rather than excepted afterwards, so
// there is no per-file judgement and nothing to keep in sync.
//
// # Derived, never hand-listed
//
// The file set comes from the tree at run time, not from a list in this file.
// A hand-written list goes stale the first time somebody adds a module, and a
// stale allowlist row that a file move orphaned has already cost this repo a
// misattributed defect (#963). For the same reason this guard carries NO
// exceptions at all: there is no row here that can rot, because there are no
// rows. If a production file ever legitimately needs the name, deleting the
// guard is the honest move, not adding the first exception to it.
//
// Test files are out of scope on purpose and by PATH CONVENTION, again derived
// rather than listed. The name is the SUBJECT of several assertions — that the
// pane launch catalog never publishes it, that the API-host wire never carries
// it, that an ambient value cannot steer an extension — and a test proving a
// name is absent has to be able to write the name down.
//
// # Non-vacuity
//
// A guard that scans nothing passes. Every root below is asserted to exist and
// to yield production files, and the roots themselves are asserted to include
// the two places a reader would do the most damage: the Rust actuator that
// used to write the stamp, and the multi-company web host.
//
// Run with `node --test scripts/test/no-chiefd-url-stamp.test.mjs`.

import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** The retired stamp. Assembled so this guard's own source does not carry a
 * greppable occurrence that its sibling scanners would have to except. */
const STAMP = ["ORG", "CHIEFD", "URL"].join("_");

/**
 * Production roots, as globs over the tree rather than file lists.
 *
 * `apps/chiefd/crates` is where both writers lived. `apps/web/src` is the
 * many-companies-one-process host — the single most damaging place for a
 * reader to come back, and therefore the one root a future engineer must never
 * be allowed to quietly drop from this set. `packages` holds every ported
 * reader, in both the package sources and the copied pi-home extensions.
 */
const PRODUCTION_ROOTS = [
  "apps/chiefd/crates",
  "apps/web/src",
  "packages",
  "scripts",
];

/** Extensions whose contents are code this repo ships or executes. */
const SCANNED = [".rs", ".ts", ".tsx", ".mjs", ".js", ".json", ".jsonc"];

/** Directories that never hold production source, by convention not by name. */
const SKIPPED_DIRS = new Set([
  "node_modules",
  "target",
  "dist",
  ".next",
  ".turbo",
  "fixtures",
]);

/**
 * A path is TEST code when any path segment is a test directory, or the base
 * name follows a test-file convention. Derived from the convention, so a new
 * test file is covered the day it is written and never needs a row here.
 */
function isTestPath(relativePath) {
  const segments = relativePath.split(sep);
  const base = segments[segments.length - 1];
  if (segments.slice(0, -1).some((segment) => segment === "test" || segment === "tests")) {
    return true;
  }
  return (
    base === "tests.rs" ||
    base.startsWith("test_") ||
    base.includes(".test.") ||
    base.includes(".spec.") ||
    base.startsWith("test-") ||
    base.endsWith("-test.mjs")
  );
}

function walk(dir, collected) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (SKIPPED_DIRS.has(entry.name)) continue;
      walk(join(dir, entry.name), collected);
      continue;
    }
    if (!entry.isFile()) continue;
    if (!SCANNED.some((extension) => entry.name.endsWith(extension))) continue;
    const path = join(dir, entry.name);
    const relativePath = relative(repoRoot, path);
    if (isTestPath(relativePath)) continue;
    collected.push(relativePath);
  }
  return collected;
}

function productionFiles() {
  const collected = [];
  for (const root of PRODUCTION_ROOTS) {
    const absolute = join(repoRoot, root);
    assert.ok(
      existsSync(absolute) && statSync(absolute).isDirectory(),
      `production root '${root}' does not exist — this guard has stopped measuring its subject`
    );
    walk(absolute, collected);
  }
  return collected.sort();
}

/**
 * Blank out `//` line comments and block comments, leaving line structure (and
 * therefore line numbers) intact.
 *
 * String literals are tracked, so a `//` inside a URL is not read as a comment
 * start. Two hazards are handled explicitly because both occur in this tree and
 * both would DESYNC the scan — and a desynced scan reads code as comment, which
 * is a false NEGATIVE and the only kind of bug that matters in a guard:
 *
 *   * a REGEX literal containing a quote. `organization-intercom.ts` has
 *     `/'([^'\n]{1,80})'/g`; a naive scanner reads that apostrophe as a string
 *     start and then treats the next few hundred lines of code as string.
 *   * an unterminated `'`/`"` for any other reason. Neither language allows a
 *     bare newline inside those, so the state resets at end of line — a bounded
 *     blast radius instead of a runaway.
 */
function stripComments(source) {
  // A `/` begins a regex (not a division) when the previous significant
  // character opens an expression. The standard heuristic, and sufficient here.
  const REGEX_ALLOWED_AFTER = new Set(["(", ",", "=", ":", "[", "!", "&", "|", "?", "{", "}", ";", "+", "-", "*", "%", "~", "^", "<", ">", "\n"]);
  let out = "";
  let index = 0;
  let quote = null;
  let lastSignificant = "\n";
  while (index < source.length) {
    const character = source[index];
    const next = source[index + 1];
    if (quote !== null) {
      if (character === "\\") {
        out += source.slice(index, index + 2);
        index += 2;
        continue;
      }
      if (character === quote) quote = null;
      // `'` and `"` cannot span a line in either language: reset rather than run away.
      if (character === "\n" && quote !== "`") quote = null;
      out += character;
      index += 1;
      continue;
    }
    if (character === '"' || character === "'" || character === "`") {
      quote = character;
      lastSignificant = character;
      out += character;
      index += 1;
      continue;
    }
    if (character === "/" && next === "/") {
      while (index < source.length && source[index] !== "\n") index += 1;
      continue;
    }
    if (character === "/" && next === "*") {
      index += 2;
      while (index < source.length && !(source[index] === "*" && source[index + 1] === "/")) {
        if (source[index] === "\n") out += "\n";
        index += 1;
      }
      index += 2;
      continue;
    }
    if (character === "/" && REGEX_ALLOWED_AFTER.has(lastSignificant)) {
      // Consume the regex body, honoring escapes and character classes.
      out += character;
      index += 1;
      let inClass = false;
      while (index < source.length && source[index] !== "\n") {
        const inner = source[index];
        if (inner === "\\") {
          out += source.slice(index, index + 2);
          index += 2;
          continue;
        }
        if (inner === "[") inClass = true;
        else if (inner === "]") inClass = false;
        out += inner;
        index += 1;
        if (inner === "/" && !inClass) break;
      }
      lastSignificant = "/";
      continue;
    }
    if (!/\s/.test(character)) lastSignificant = character;
    else if (character === "\n") lastSignificant = "\n";
    out += character;
    index += 1;
  }
  return out;
}

test("no production code names the retired chiefd-address stamp", () => {
  const offenders = [];
  for (const relativePath of productionFiles()) {
    const contents = readFileSync(join(repoRoot, relativePath), "utf8");
    if (!contents.includes(STAMP)) continue;
    for (const [index, line] of stripComments(contents).split("\n").entries()) {
      if (line.includes(STAMP)) offenders.push(`${relativePath}:${index + 1}: ${line.trim()}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    `${STAMP} is retired: one chiefd address per PROCESS cannot be correct in a host ` +
      `that serves many companies, and a wrong daemon ANSWERS rather than refusing. ` +
      `A company's daemon is resolved from beacond, per company, once per install. ` +
      `Production code must not name it. (Comments may: the past-tense account ` +
      `of how this failed is worth keeping, and is where a grep for the name ` +
      `should land.)\n  ${offenders.join("\n  ")}`
  );
});

test("the scan is non-vacuous: every root yields production files", () => {
  for (const root of PRODUCTION_ROOTS) {
    const collected = walk(join(repoRoot, root), []);
    assert.ok(
      collected.length > 0,
      `production root '${root}' matched no scannable file — the guard would pass by seeing nothing`
    );
  }
});

test("the roots still include the actuator and the multi-company web host", () => {
  // The two places a returning reader does the most damage. Named here so that
  // narrowing the scan is a deliberate, visible edit rather than a quiet one.
  for (const required of ["apps/chiefd/crates", "apps/web/src"]) {
    assert.ok(
      PRODUCTION_ROOTS.includes(required),
      `'${required}' must stay in the scanned set: it is ground this guard exists to protect`
    );
  }
});

test("test files are excluded by convention, and the convention actually matches", () => {
  // The exclusion has to be real, or the guard's own supporting assertions
  // (which must write the name down) would make it unmaintainable.
  for (const path of [
    join("apps", "chiefd", "crates", "chiefd-host", "src", "converge_apply", "cycle", "tests.rs"),
    join("apps", "chiefd", "crates", "chiefd-api", "tests", "api_host_launch_profile_http.rs"),
    join("packages", "piing", "test", "PaneEndpoint.test.ts"),
  ]) {
    assert.ok(isTestPath(path), `'${path}' must be recognized as test code`);
  }
  for (const path of [
    join("apps", "chiefd", "crates", "chiefd-host", "src", "converge_apply", "cycle.rs"),
    join("apps", "web", "src", "server", "ExtensionTools.ts"),
  ]) {
    assert.ok(!isTestPath(path), `'${path}' is production and must be scanned`);
  }
});
