// The runtime row's process map keeps its honest name: nothing in the row's
// schema, its structs, or production code may call it a pane or a window.
//
// # What the name cost, and why a name is worth a guard
//
// The durable `runtime` row published a map called `panes`. It has held no tmux
// pane since #751 moved tmux out of the backend: it is person -> the actuator's
// process handle — the pid as a decimal string, or the EMPTY STRING when the
// actuator proved a person alive without reading a pid. The KEY SET is the
// fact; the value is a diagnostic.
//
// Two readers believed the name instead of the contents, and the two failure
// modes together are the whole argument for this guard:
//
//   * `organization-intercom.ts` validated the map as a set of `%\d+` tmux ids
//     and refused the real payload FIVE ways (empty, not `%\d+`, two `""`
//     values read as duplicate ids, and a `windows[department]` lookup that can
//     never hit). `org_roster` failed for every person in every company, and a
//     live CEO escalated it to its own operator. LOUD.
//   * `RuntimeWake.ts` broke identically and FAILED OPEN. Every wake decision
//     degraded to `unsafe_projection`, SSE-C2 coalescing was silently dead, and
//     every mailbox delivery to a live person spawned a redundant reconcile.
//     Nothing was red. Nobody found it by watching.
//
// A field whose name states the opposite of its contents is not cosmetic. It is
// a standing invitation to the next reader to make the same mistake, and the
// quiet half of that mistake is invisible to every other gate in this repo.
//
// # Two legs, both derived
//
//   1. SCHEMA — reads the runtime row's own definition out of the tree
//      (`schema.rs`'s `runtime*` CREATE TABLE statements and the serde field
//      names of `RuntimeState` / `RuntimeObservation` in `runtime_rows.rs`) and
//      asserts no identifier matches /pane|window/i. This leg has no list in
//      it at all: it re-derives the subject on every run, so a field added
//      tomorrow is covered tomorrow.
//   2. TOKENS — bans the retired identifiers by name in production code across
//      derived roots. The schema leg cannot see a READER that still spells the
//      old name (a struct field elsewhere, a JSON key in a TS payload), and a
//      reader is what did the damage both times.
//
// # Comments are excluded, deliberately
//
// Same call `no-chiefd-url-stamp.test.mjs` and `beacond-port-single-definition.
// test.mjs` made, for the same reason: the past-tense account of how this
// failed is the most valuable documentation in `organization-intercom.ts`,
// `RuntimeWake.ts` and `RosterRuntimeProcessProjection.test.ts`, and it is
// where an engineer who greps for `panes` should land. Comments are STRIPPED
// before the token scan rather than excepted afterwards, so there is no
// per-file judgement and nothing to keep in sync.
//
// Test files are out of scope by PATH CONVENTION, again derived rather than
// listed: several suites must write the retired names down to assert they are
// gone, and to record the payload the outage was measured on.
//
// # Why the bare word `panes` is not in the token list
//
// It cannot be: `chief-cli` is the tmux actuator and its `owned_panes` hold
// real `%N` ids, and `apps/web` draws real panes in its own layout model. Both
// are correct uses of the word. The bare key is covered where it can be covered
// EXACTLY — by the schema leg, on the row that is the subject — instead of by a
// repo-wide ban that would have to grow exceptions and then rot. The tokens
// below are the ones that name this map and nothing else.
//
// Run with `node --test scripts/test/runtime-row-process-handle-naming.test.mjs`.
//
// NAMED FOR WHAT IT PROTECTS, not for what it forbids. The obvious name spelled
// one of the retired tokens, so this guard's own manifest entry failed its own
// token leg -- which is the leg working, and the fix is the honest name.

import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

const SCHEMA_RS = join(
  repoRoot,
  "apps/chiefd/crates/chiefd-core/src/schema.rs".split("/").join(sep)
);
const RUNTIME_ROWS_RS = join(
  repoRoot,
  "apps/chiefd/crates/chiefd-core/src/store/runtime_rows.rs".split("/").join(sep)
);
const RUNTIME_LIFECYCLE_RS = join(
  repoRoot,
  "apps/chiefd/crates/chiefd-host/src/runtime_lifecycle.rs".split("/").join(sep)
);

/** The retired identifiers, assembled so this guard's own source carries no
 * greppable occurrence its sibling scanners would have to except. */
const RETIRED = [
  ["runtime", "panes"].join("_"),
  ["pane", "generations"].join("_"),
  ["pane", "Generations"].join(""),
  ["pane", "people"].join("_"),
  ["Pane", "Row"].join(""),
  ["runtime", "pane"].join("-"),
];

/**
 * WORD-BOUNDED, never `includes`: `_` and letters are word characters, so `\b`
 * never fires mid-identifier.
 *
 * `pane_ids` is deliberately NOT on this list, and the reason is the whole
 * design of the token leg. `chief-cli` is the tmux actuator: `layout.rs`
 * computes a real tmux layout string from real `pane_ids`, `interpret.rs` reads
 * them off the display, and `host.rs` declares `dead_pane_ids` on a live trait.
 * Every one of those is CORRECT. A repo-wide ban on that token would flag
 * thirteen correct lines, and a guard that cries wolf is a guard somebody
 * deletes. The retired `paneIds` REPORT fields are covered exactly instead, by
 * the struct leg below, on the two structs that actually carry them.
 */
const RETIRED_PATTERNS = RETIRED.map((token) => ({
  token,
  pattern: new RegExp(`\\b${token.replace(/[-]/g, "\\-")}\\b`),
}));

const PRODUCTION_ROOTS = ["apps/chiefd/crates", "apps/web/src", "packages", "scripts"];

const SCANNED = [".rs", ".ts", ".tsx", ".mjs", ".js", ".json", ".jsonc"];

const SKIPPED_DIRS = new Set(["node_modules", "target", "dist", ".next", ".turbo", "fixtures"]);

/** A path is TEST code when a path segment is a test directory, or the base
 * name follows a test-file convention. Derived from the convention, so a new
 * test file is covered the day it is written and never needs a row here. */
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
 * Blank out line and block comments, leaving line structure (and therefore line
 * numbers) intact.
 *
 * String literals are tracked so a `//` inside a URL is not read as a comment
 * start. Two hazards are handled explicitly because both occur in this tree and
 * both would DESYNC the scan — a desynced scan reads code as comment, which is
 * a false NEGATIVE and the only kind of bug that matters in a guard:
 *
 *   * a REGEX literal containing a quote (`organization-intercom.ts` has
 *     `/'([^'\n]{1,80})'/g`);
 *   * an unterminated `'`/`"` for any other reason. Neither language allows a
 *     bare newline inside those, so the state resets at end of line.
 */
function stripComments(source) {
  const REGEX_ALLOWED_AFTER = new Set([
    "(", ",", "=", ":", "[", "!", "&", "|", "?", "{", "}", ";", "+", "-", "*", "%", "~", "^", "<", ">", "\n",
  ]);
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

/** Strip `--` line comments from SQL text, keeping line structure. */
function stripSqlComments(source) {
  return source
    .split("\n")
    .map((line) => {
      const at = line.indexOf("--");
      return at === -1 ? line : line.slice(0, at);
    })
    .join("\n");
}

/**
 * Every column and table identifier the runtime row's own DDL declares.
 *
 * Derived from `schema.rs` at run time — the `CREATE TABLE runtime…` blocks and
 * nothing else — so a child table added tomorrow is measured tomorrow without
 * anyone remembering to add it here.
 */
function runtimeSchemaIdentifiers() {
  const sql = stripSqlComments(readFileSync(SCHEMA_RS, "utf8"));
  const identifiers = [];
  const createTable = /CREATE TABLE(?: IF NOT EXISTS)?\s+(runtime\w*)\s*\(([\s\S]*?)\n\);/g;
  for (const match of sql.matchAll(createTable)) {
    identifiers.push(match[1]);
    for (const line of match[2].split("\n")) {
      const column = /^\s*([a-z_][a-z0-9_]*)\s+(TEXT|INTEGER|REAL|BLOB|NUMERIC)/i.exec(line);
      if (column) identifiers.push(column[1]);
    }
  }
  return identifiers;
}

/**
 * The serde field names of the two structs that ARE the runtime row's wire:
 * the whole document (`RuntimeState`) and one converge pass's contribution to
 * it (`RuntimeObservation`). Field names are read straight out of the source
 * rather than from a list, for the same reason as above.
 */
function structFields(path, names) {
  const source = readFileSync(path, "utf8");
  const fields = [];
  for (const name of names) {
    const start = source.indexOf(`pub struct ${name} {`);
    assert.notEqual(
      start,
      -1,
      `'${name}' is gone from ${relative(repoRoot, path)} — this guard has stopped measuring its subject`
    );
    const end = source.indexOf("\n}", start);
    assert.ok(end > start, `could not find the end of '${name}'`);
    for (const line of source.slice(start, end).split("\n")) {
      const field = /^\s*pub ([a-z_][a-z0-9_]*):/.exec(line);
      if (field) fields.push(`${name}.${field[1]}`);
    }
  }
  return fields;
}

test("the runtime row's schema names no pane and no window", () => {
  const identifiers = runtimeSchemaIdentifiers();
  assert.ok(
    identifiers.length >= 10,
    `the runtime DDL derivation found only ${identifiers.length} identifiers — it has stopped ` +
      `parsing schema.rs and would pass by seeing nothing`
  );
  assert.ok(
    identifiers.includes("runtime"),
    "the runtime DDL derivation did not find the `runtime` table itself"
  );
  assert.deepEqual(
    identifiers.filter((identifier) => /pane|window/i.test(identifier)),
    [],
    "the runtime row stores person -> the actuator's process handle. Naming a column or a " +
      "child table after a pane or a window states the opposite of its contents, and that is " +
      "exactly the lie a reader validated against when it refused every real payload and took " +
      "org_roster down company-wide."
  );
});

// `RuntimeObservation` was the SECOND struct this checked, and it is deleted
// with the observed-runtime write path (`publish_observation`,
// `runtime_publish_observation`): chiefd receives no report of what is running,
// so there is no observation struct to misname. `RuntimeState` survives and is
// still served, so the guard keeps its live subject rather than being deleted
// with the struct it lost.
test("the runtime row's wire structs name no pane and no window", () => {
  const fields = structFields(RUNTIME_ROWS_RS, ["RuntimeState"]);
  assert.ok(
    fields.length >= 15,
    `the struct-field derivation found only ${fields.length} fields — it has stopped parsing ` +
      `runtime_rows.rs and would pass by seeing nothing`
  );
  assert.ok(
    fields.includes("RuntimeState.process_handles"),
    `the runtime struct must carry the process-handle map under its real name; found ${fields.join(", ")}`
  );
  assert.deepEqual(
    fields.filter((field) => /pane|window/i.test(field)),
    [],
    "a serde field name on this struct IS the wire key every TypeScript reader sees. " +
      "`panes` is what made one reader validate pids as tmux ids and refuse them, and made its " +
      "neighbour fail open with nothing red."
  );
});

test("the runtime REPORT struct names no pane and no window either", () => {
  // `RuntimeLaunchReport` serves the SAME map, read back from the same row,
  // over `/v1/org/runtime/launch`. It carried it as `paneIds` on the wire, with
  // a doc comment that said so and called it temporary. A rename of the row
  // that left its own report lying would have moved the defect, not fixed it.
  //
  // `RuntimeObservationReport` stood beside it, over `/v1/org/runtime/observe`.
  // Both the report and the route are deleted: chiefd receives no observation,
  // so there is no second report to keep honest.
  const fields = structFields(RUNTIME_LIFECYCLE_RS, ["RuntimeLaunchReport"]);
  assert.ok(
    fields.length >= 6,
    `the report-field derivation found only ${fields.length} fields — it has stopped parsing ` +
      `runtime_lifecycle.rs and would pass by seeing nothing`
  );
  assert.ok(
    fields.includes("RuntimeLaunchReport.process_handles"),
    `the runtime report must carry the process-handle map under its real name; found ${fields.join(", ")}`
  );
  assert.deepEqual(
    fields.filter((field) => /pane|window/i.test(field)),
    [],
    "these serde field names are the wire an operator client reads. They must not name a " +
      "display chiefd has not been able to see since #751."
  );
});

test("no production code names the retired runtime-pane identifiers", () => {
  const offenders = [];
  for (const relativePath of productionFiles()) {
    const contents = readFileSync(join(repoRoot, relativePath), "utf8");
    if (!RETIRED_PATTERNS.some(({ pattern }) => pattern.test(contents))) continue;
    for (const [index, line] of stripComments(contents).split("\n").entries()) {
      for (const { token, pattern } of RETIRED_PATTERNS) {
        if (pattern.test(line)) offenders.push(`${relativePath}:${index + 1}: ${token}`);
      }
    }
  }
  assert.deepEqual(
    offenders,
    [],
    `these names are retired: the runtime row holds person -> the actuator's process handle, ` +
      `not a pane. Production code must not name them. (Comments may: the past-tense account of ` +
      `how this failed is worth keeping, and is where a grep for the name should land.)\n  ` +
      offenders.join("\n  ")
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

test("the roots still include the backend and every reader package", () => {
  // The backend publishes the map; `packages` holds both readers that broke on
  // it. Narrowing the scan must be a deliberate, visible edit.
  for (const required of ["apps/chiefd/crates", "packages"]) {
    assert.ok(
      PRODUCTION_ROOTS.includes(required),
      `'${required}' must stay in the scanned set: it is ground this guard exists to protect`
    );
  }
});

test("comment stripping actually blanks a comment and spares a string", () => {
  // The token leg's correctness rests entirely on this: a stripper that
  // desyncs reads code as comment and passes over a real offender.
  const stripped = stripComments(
    ['const a = "x";', `// ${RETIRED[0]}`, `const b = "${RETIRED[0]}";`].join("\n")
  );
  const lines = stripped.split("\n");
  assert.equal(lines.length, 3, "line structure must survive stripping");
  assert.ok(!lines[1].includes(RETIRED[0]), "a line comment must be blanked");
  assert.ok(lines[2].includes(RETIRED[0]), "a string literal must survive stripping");
});
