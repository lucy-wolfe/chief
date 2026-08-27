// The guard `chiefd`'s own USAGE already had, applied to every OTHER surface
// that instructs an operator or a model.
//
// # Why this file exists
//
// A sweep closed 14 sites where the product stated something untrue to the
// person or the model reading it: a command that never worked (`chiefd catalog
// --json`), a whole CLI namespace taught to models that no binary has ever
// routed (`chiefd company create|boot|launch|tree|show`, `chiefd department
// launch`, a `--replace-launcher-pane` flag present in NO file in the tree),
// and retired product names. Its closing observation is the reason for this
// file: `chiefd`'s own `USAGE` was the single surface that had not rotted, and
// the reason sits directly beside it — `crates/chief-cli/src/main.rs`'s
// `the_usage_text_names_exactly_the_operator_surface` asserts that internal
// verbs are never advertised and that "launcher", "Launcher" and "triber"
// never appear. Nothing of that shape guarded skills, tool descriptions,
// schemas or docs, which is where all fourteen defects lived.
//
// A rule with no test is a comment.
//
// # The three rules
//
// 1. RETIRED NAME. `launcher` / `Launcher` / `triber` (and therefore "launcher
//    mode" and "Tribe Launcher") name products this program does not have.
//    Internal identifiers are EXEMPT and listed in `EXEMPT_IDENTIFIERS` below:
//    this governs copy a human or a model reads, not `ORG_LAUNCHER_ROOT` and
//    not `isForbiddenLauncherResource`. The `launcher_calls` observable key was
//    exempt here until #1044 deleted the observable; an exemption for an
//    identifier that no longer exists exempts nothing and hides the next one.
// 2. UNROUTED VERB. A `chiefd <verb>` that the binary does not route. The verb
//    table is DERIVED, at run time, from the operator client's `route()` and
//    the daemon's own `MODES` table — never transcribed. A hardcoded list is the
//    next thing to rot, and this guard's whole subject is copy that fell out
//    of step with code.
// 3. GLOSSARY (#375), on the RUST surface, the SKILLS surface and the
//    EXTENSION surface — see "The Rust surface" below for why the scanned
//    Rust text is narrower than "every string literal", and the surface
//    matrix for why a skill is scanned whole. A skill is markdown a person's
//    Pi session is handed verbatim, so every word of it is model-facing copy
//    and there is no identifier column to exempt; an extension is
//    TypeScript, so it is scanned through `extensionProse` — the same
//    copy/identifier boundary its retired-name column already uses, and the
//    reason `assignments` the keyspace and `assignmentId` the parameter do
//    not fire. A goal
//    (the supervision ledger) and a task (`/v1/tasks/*`) are two products and
//    stay two words; "assignment"/"assign" is a verb and never a noun naming a
//    thing the reader tracks; generic filler ("work item", "objective") never
//    labels either concept; "owned task" is banned outright.
//
// # What counts as a command claim, and why the rules are narrow
//
// This repo has one cautionary tale about a broad regex guard
// (`dep-declaration.test.mjs`'s header: it classified correctly in principle
// and drowned its one true positive in 27 false ones). So a `chiefd <verb>` is
// only read as a CLAIM when it is written the way a command is written:
//
//   * inside an inline code span — `` `chiefd catalog --json` `` — which is
//     exactly how all fourteen defects were written, in markdown and inside
//     description strings alike; or
//   * as a line inside a fenced block whose info string is a SHELL language
//     (```bash / ```sh / ```shell / ```console). A bare or ```text fence is a
//     diagram or a table in this tree (README's port table and directory tree,
//     ARCHITECTURE.md's route map) and is not a command claim.
//
// Measured against the real tree, that pair produces zero English-prose false
// positives ("chiefd derives", "chiefd already", "chiefd must" — 40+ such
// bigrams exist and none is a code span). Three further exclusions, each for a
// measured reason:
//
//   * a span containing `${` is a JS/Rust template literal, not a documented
//     command (`` `chiefd docstore ${path} returned an invalid outcome` `` is
//     an error message, not a claim that `chiefd docstore` exists);
//   * a `<placeholder>` is a metavariable, so `chiefd <verb>` claims nothing;
//   * a token starting with `#` is a trailing shell comment on a bare
//     `chiefd`, which is a real invocation.
//
// # Run it
//
//   node --test scripts/test/model-facing-copy.test.mjs

import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

const readRepo = (relativePath) => readFileSync(join(repoRoot, relativePath), "utf8");

// ───────────────────────────────────────────────────────────────────────────
// The verb table, derived from the binary
// ───────────────────────────────────────────────────────────────────────────

const CLIENT_MAIN_RS = "apps/chiefd/crates/chief-cli/src/main.rs";
const DAEMON_MAIN_RS = "apps/chiefd/crates/chiefd-daemon/src/main.rs";

/**
 * Every verb `chiefd` actually answers, read out of the routing itself.
 *
 * TWO FILES, because P6 split the one binary that was operator CLI and backend
 * daemon at once into two programs:
 *
 *   * `chief-cli/src/main.rs`'s `route()` — the operator surface (`ls`, `new`,
 *     `create`, `attach`, `stop`, `reset`, `host`), the help and version
 *     spellings it claims before anything else looks at the argv, and its
 *     `DAEMON_VERBS` table: the modes it `exec`s into the daemon rather than
 *     answering. An operator types `chiefd run`; that it lands in a second
 *     executable is an implementation fact, not a difference in the surface.
 *   * `chiefd-daemon/src/main.rs`'s `MODES` table — the same modes, from the
 *     side that actually serves them, plus the flags it answers before
 *     dispatch.
 *
 * The daemon modes are included deliberately. They are never ADVERTISED — that
 * is the client's own USAGE assertion's job and it stays there — but they are
 * real: an engineering doc naming `chiefd docstore-only` is describing a mode
 * that exists, and calling that a lie would make this guard something people
 * route around. What this guard refuses is a verb that is not there AT ALL,
 * which is every one of the fourteen defects.
 */
export function deriveChiefdVerbs(clientSource, daemonSource) {
  const verbs = new Set();

  const routeStart = clientSource.indexOf("pub(crate) fn route(");
  assert.notEqual(routeStart, -1, `${CLIENT_MAIN_RS} must define route()`);
  const routeEnd = clientSource.indexOf("\n}\n", routeStart);
  assert.notEqual(routeEnd, -1, `${CLIENT_MAIN_RS}'s route() must have a body`);
  const routeBody = clientSource.slice(routeStart, routeEnd);

  // `if matches!(verb, "help" | "--help" | "-h")` — claimed before the match.
  for (const guard of routeBody.matchAll(/matches!\(\s*verb\s*,([^)]*)\)/g)) {
    for (const literal of guard[1].matchAll(/"([^"]+)"/g)) verbs.add(literal[1]);
  }
  // `match verb { "ls" => … }` — the arm heads, at their one indentation.
  const matchStart = routeBody.indexOf("match verb {");
  assert.notEqual(matchStart, -1, `${CLIENT_MAIN_RS}'s route() must match on verb`);
  for (const arm of routeBody.slice(matchStart).matchAll(/^\s{8}"([^"]+)"\s*=>/gm)) {
    verbs.add(arm[1]);
  }
  for (const mode of clientForwardedDaemonVerbs(clientSource)) verbs.add(mode);
  for (const mode of daemonDispatchedModes(daemonSource)) verbs.add(mode);

  return verbs;
}

/** The modes the operator client `exec`s into the daemon — its `DAEMON_VERBS`
 * table, read as the table it is. */
export function clientForwardedDaemonVerbs(clientSource) {
  const start = clientSource.indexOf("pub(crate) const DAEMON_VERBS");
  assert.notEqual(start, -1, `${CLIENT_MAIN_RS} must declare DAEMON_VERBS`);
  const end = clientSource.indexOf("];", start);
  assert.notEqual(end, -1, `${CLIENT_MAIN_RS}'s DAEMON_VERBS must be an array literal`);
  return [...clientSource.slice(start, end).matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

/** The modes the daemon really serves — its `MODES` table, one `("name", …)`
 * row per mode. */
export function daemonDispatchedModes(daemonSource) {
  const start = daemonSource.indexOf("const MODES");
  assert.notEqual(start, -1, `${DAEMON_MAIN_RS} must declare MODES`);
  const end = daemonSource.indexOf("];", start);
  assert.notEqual(end, -1, `${DAEMON_MAIN_RS}'s MODES must be an array literal`);
  return [...daemonSource.slice(start, end).matchAll(/\("([^"]+)",/g)].map((match) => match[1]);
}

// ───────────────────────────────────────────────────────────────────────────
// The two rules
// ───────────────────────────────────────────────────────────────────────────

/**
 * Internal identifiers that legitimately carry the retired word.
 *
 * Stripped from the text BEFORE the retired-name match, so this list is the
 * one place a reviewer checks before widening the guard. Every entry is
 * code-shaped: an env-var name, an exported function, an observable key. None
 * of them is copy anyone reads as English.
 */
const EXEMPT_IDENTIFIERS = [
  // `ORG_LAUNCHER_ROOT`, `ORG_LAUNCHER_DATA_ROOT`, … — SCREAMING_SNAKE only.
  /[A-Z][A-Z0-9]*_LAUNCHER(?:_[A-Z0-9]+)*/g,
  // `packages/piing/src/policy/CapabilityPolicy.ts`'s exported refusal.
  /isForbiddenLauncherResource/g,
];

const RETIRED_NAMES = /launcher|triber/gi;

/** Every retired product name in `text`, after the exemptions are removed. */
export function retiredNameHits(text) {
  let scannable = text;
  for (const exemption of EXEMPT_IDENTIFIERS) scannable = scannable.replace(exemption, "");
  return [...scannable.matchAll(RETIRED_NAMES)].map((match) => match[0]);
}

/** Fence info strings whose contents are commands rather than a diagram. */
const SHELL_FENCES = new Set(["bash", "sh", "shell", "console", "zsh"]);

/**
 * Every `chief <verb>` / `chiefd <mode>` this text CLAIMS exists — see the
 * header for why a claim is narrower than an occurrence.
 *
 * TWO PROGRAM NAMES, because there are two programs: `chief` is the front door
 * an operator types and `chiefd` is the daemon it spawns. Scanning only one of
 * them would leave every claim made about the other unexamined, which is how a
 * rename ships copy naming a verb no binary answers.
 *
 * Returns `[program, verb]` pairs so the caller can hold each name to the verbs
 * that name really answers.
 *
 * @param {string} text
 * @param {{ fences: boolean }} options `fences` reads shell fenced blocks too,
 *   which only makes sense for markdown.
 */
export function chiefdVerbClaims(text, { fences }) {
  const claimed = [];

  for (const span of text.matchAll(/`([^`\n]{1,140})`/g)) {
    const inner = span[1];
    if (inner.includes("${")) continue;
    const tokens = inner.trim().split(/\s+/);
    if ((tokens[0] === "chief" || tokens[0] === "chiefd") && tokens.length > 1) {
      claimed.push([tokens[0], tokens[1]]);
    }
  }

  if (fences) {
    let fenceLanguage = null;
    for (const line of text.split("\n")) {
      const fence = line.match(/^\s*```(\S*)/);
      if (fence) {
        fenceLanguage = fenceLanguage === null ? (fence[1] || "").toLowerCase() : null;
        continue;
      }
      if (fenceLanguage === null || !SHELL_FENCES.has(fenceLanguage)) continue;
      const invocation = line.trim().match(/^(chief|chiefd)\s+(\S+)/);
      if (invocation) claimed.push([invocation[1], invocation[2]]);
    }
  }

  return claimed.filter(
    ([, verb]) => !/^<.*>$/.test(verb) && !verb.startsWith("#")
  );
}

/**
 * The subset of `chiefdVerbClaims` the named binary does not answer.
 *
 * `chief` answers the whole derived surface — its own operator verbs plus the
 * daemon modes it forwards. `chiefd` answers ONLY the daemon's own `MODES`, so
 * `chiefd attach` is copy naming something that does not exist even though
 * `chief attach` is real. Held apart deliberately: collapsing the two into one
 * accepted set is exactly the leniency that would let a half-finished rename
 * read as correct.
 */
export function unroutedVerbHits(text, options, verbs, daemonModes) {
  const answered = (program) =>
    program === "chiefd" && daemonModes !== undefined ? daemonModes : verbs;
  return chiefdVerbClaims(text, options)
    .filter(([program, verb]) => !answered(program).has(verb))
    .map(([program, verb]) => `${program} ${verb}`);
}

// ───────────────────────────────────────────────────────────────────────────
// Rule 3: the #375 glossary
// ───────────────────────────────────────────────────────────────────────────

/**
 * The glossary rules, as `chief/CLAUDE.md` states them, in the SAME three
 * patterns `apps/web/test/utils/Glossary.test.ts` already enforces over the web
 * components. Deliberately identical: two guards on one product rule that
 * disagree about what the rule is are worse than one, because a sweep can pass
 * on one surface and fail on the other for reasons nobody can name.
 *
 * Why the noun-`assignment` rule is ARTICLE-PRECEDED rather than every
 * occurrence of the word: "assigned to @val" is correct copy, `assignmentId` is
 * an exempt param name, and `assignments` is an exempt keyspace. What the
 * glossary bans is the word used as a NOUN naming a thing the reader tracks,
 * and in English that noun almost always arrives behind a determiner. Measured
 * against the real tree, the article form finds every violation a reader would
 * call one and no false positive; the un-anchored form additionally fires 28
 * times, 24 of them inside `chiefd-core/src/store/supervision*`, which is a
 * DIFFERENT migration (see the register) rather than a defect this rule shape
 * would help anyone fix.
 */
const GLOSSARY_RULES = [
  ["glossary-filler", /\bwork items?\b|\bobjectives?\b|\bowned tasks?\b/gi],
  [
    "glossary-noun-assignment",
    /\b(?:a|an|the|this|that|your|its|each|every|one|another|new|same)\s+assignments?\b/gi,
  ],
  ["glossary-noun-delegation", /\bdelegations?\b/gi],
];

/** Every glossary violation in `text`, as `[rule, hit]` pairs. */
export function glossaryHits(text) {
  const found = [];
  for (const [rule, pattern] of GLOSSARY_RULES) {
    for (const match of text.matchAll(pattern)) found.push([rule, match[0]]);
  }
  return found;
}

// ───────────────────────────────────────────────────────────────────────────
// The surfaces
// ───────────────────────────────────────────────────────────────────────────

function listFiles(root, relative = "", collected = []) {
  for (const entry of readdirSync(join(root, relative), { withFileTypes: true })) {
    const next = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) listFiles(root, next, collected);
    else collected.push(next);
  }
  return collected;
}

/** Skills: the markdown a person's Pi session is literally handed. */
function skillFiles() {
  const root = join(repoRoot, "packages/piing/skills");
  return listFiles(root)
    .filter((file) => file.endsWith(".md"))
    .map((file) => `packages/piing/skills/${file}`);
}

/**
 * Docs: `README.md`, `AGENTS.md`, and every `docs/*.md` LINKED from either.
 *
 * Derived rather than listed, and derived this way on purpose. `docs/` also
 * holds dated evidence records (`concept-collision-audit.md`,
 * `store-implementation-audit.md`, `docs/testing/**`) whose subject IS the retired
 * system: naming it there is a correct quotation of history, not rot, and
 * 5,300 such occurrences would bury every real finding. A doc someone is
 * POINTED AT is instruction; a doc nobody links is a record. Linking a new doc
 * from the README enrolls it here automatically.
 */
function docFiles() {
  const entryPoints = ["README.md", "AGENTS.md"];
  const linked = new Set();
  for (const entry of entryPoints) {
    for (const link of readRepo(entry).matchAll(/docs\/[A-Za-z0-9_.-]+\.md/g)) linked.add(link[0]);
  }
  return [...entryPoints, ...[...linked].sort()];
}

// ───────────────────────────────────────────────────────────────────────────
// The Rust surface
// ───────────────────────────────────────────────────────────────────────────

const RUST_ROOT = "apps/chiefd/crates";

/**
 * Every shipped `.rs` file in the chiefd workspace, DERIVED from the directory
 * listing rather than named.
 *
 * Excluded: anything under a `tests/` directory and every `tests.rs`. Those are
 * harness prose — `assert!(…, "the assignment is a durable relational row")` —
 * which nobody outside this repository ever reads. Inline `#[cfg(test)] mod`
 * blocks are excluded too, by [`shippedRust`], for the same reason and because
 * roughly two hundred of them live inside otherwise-shipping files.
 */
export function rustCopyFiles(root = join(repoRoot, RUST_ROOT)) {
  return listFiles(root)
    .filter((file) => file.endsWith(".rs"))
    .filter((file) => !file.split("/").includes("tests"))
    .filter((file) => file !== "tests.rs" && !file.endsWith("/tests.rs"))
    .sort()
    .map((file) => `${RUST_ROOT}/${file}`);
}

/** Where a Rust string literal beginning at `start` ends (raw strings and
 * escapes included). Returns the index one past the closing quote. */
function endOfRustLiteral(source, start) {
  if (source[start] === "r") {
    let cursor = start + 1;
    let hashes = 0;
    while (source[cursor] === "#") {
      hashes += 1;
      cursor += 1;
    }
    if (source[cursor] !== '"') return start + 1;
    const terminator = `"${"#".repeat(hashes)}`;
    const end = source.indexOf(terminator, cursor + 1);
    return end === -1 ? source.length : end + terminator.length;
  }
  let cursor = start + 1;
  while (cursor < source.length) {
    if (source[cursor] === "\\") {
      cursor += 2;
      continue;
    }
    if (source[cursor] === '"') return cursor + 1;
    cursor += 1;
  }
  return source.length;
}

function startsRustLiteral(source, index) {
  return (
    source[index] === '"' ||
    (source[index] === "r" && (source[index + 1] === '"' || source[index + 1] === "#"))
  );
}

/**
 * Rust source with every comment and every `#[cfg(test)] mod … { … }` block
 * replaced by spaces, preserving byte offsets so a reported line number is the
 * real one.
 *
 * Comments are removed because doc comments are the glossary's own named
 * exemption ("doc comments about internals"): `store/org_ops.rs`'s "a
 * resolved handle to one work item" describes a Rust type and is not copy.
 * Removing them is also what makes the scan a scan of STRINGS rather than of
 * the file, so a `//` inside a literal cannot end a comment and a `"` inside a
 * comment cannot open a literal.
 */
export function shippedRust(source) {
  const characters = Array.from(source);
  const blank = (from, to) => {
    for (let index = from; index < to && index < characters.length; index += 1) {
      if (characters[index] !== "\n") characters[index] = " ";
    }
  };

  let cursor = 0;
  while (cursor < source.length) {
    if (source[cursor] === "/" && source[cursor + 1] === "/") {
      let end = cursor;
      while (end < source.length && source[end] !== "\n") end += 1;
      blank(cursor, end);
      cursor = end;
      continue;
    }
    if (source[cursor] === "/" && source[cursor + 1] === "*") {
      let depth = 1;
      let end = cursor + 2;
      while (end < source.length && depth > 0) {
        if (source[end] === "/" && source[end + 1] === "*") {
          depth += 1;
          end += 2;
        } else if (source[end] === "*" && source[end + 1] === "/") {
          depth -= 1;
          end += 2;
        } else end += 1;
      }
      blank(cursor, end);
      cursor = end;
      continue;
    }
    if (startsRustLiteral(source, cursor)) {
      cursor = endOfRustLiteral(source, cursor);
      continue;
    }
    cursor += 1;
  }

  let text = characters.join("");
  for (;;) {
    const attribute = text.indexOf("#[cfg(test)]");
    if (attribute === -1) break;
    const open = text.indexOf("{", attribute);
    if (open === -1) break;
    let depth = 0;
    let end = open;
    for (; end < text.length; end += 1) {
      if (startsRustLiteral(text, end)) {
        end = endOfRustLiteral(text, end) - 1;
        continue;
      }
      if (text[end] === "{") depth += 1;
      else if (text[end] === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    const blanked = Array.from(text);
    for (let index = attribute; index <= Math.min(end, blanked.length - 1); index += 1) {
      if (blanked[index] !== "\n") blanked[index] = " ";
    }
    text = blanked.join("");
  }
  return text;
}

/**
 * The argument of a call whose string is a DIAGNOSTIC, not copy: an
 * `assert!`/`expect`/`panic!` message exists for whoever is reading a failing
 * test or a backtrace.
 */
const DIAGNOSTIC_CALL =
  /(?:assert[a-z_]*!|debug_assert[a-z_]*!|expect|expect_err|unwrap_err|panic!|todo!|unimplemented!)\s*\(\s*$/;

/** A literal that is a SQL statement rather than a sentence. */
const EMBEDDED_SQL = /\b(?:CREATE TABLE|CREATE INDEX|CREATE UNIQUE|INSERT INTO|SELECT |UPDATE |DELETE FROM|PRAGMA )/;

/**
 * Every PROSE string literal in a Rust file — the shipped copy this guard
 * judges — as `{ line, value }`.
 *
 * "Prose" is four or more whitespace-separated words containing a lowercase
 * letter. That single test is what keeps this rule usable, and each half of it
 * is load-bearing against a measured population: the chiefd workspace holds
 * tens of thousands of string literals, and essentially all of them are route
 * paths, JSON keys, refusal CODES, column names, kebab store ids and env-var
 * names — none of which is copy, all of which carry the glossary's exempt
 * vocabulary (`assignments`, `open-assignment`, `goal-intents`), and every one
 * of which is one or two tokens long. Applying the glossary to identifiers
 * would produce hundreds of hits that no reviewer could act on, which is how a
 * guard becomes something people route around.
 *
 * Against the real tree this leaves 1,945 prose strings across 270 files — a
 * corpus large enough to be worth guarding and small enough that every finding
 * is a sentence a human wrote for a human or a model to read.
 */
export function rustProse(source) {
  const text = shippedRust(source);
  const found = [];
  let cursor = 0;
  while (cursor < text.length) {
    if (!startsRustLiteral(text, cursor)) {
      cursor += 1;
      continue;
    }
    const end = endOfRustLiteral(text, cursor);
    const value = text
      .slice(cursor, end)
      .replace(/^r#*"/, "")
      .replace(/^"/, "")
      .replace(/"#*$/, "");
    const isDiagnostic = DIAGNOSTIC_CALL.test(text.slice(Math.max(0, cursor - 200), cursor));
    if (
      !isDiagnostic &&
      !EMBEDDED_SQL.test(value) &&
      /[a-z]/.test(value) &&
      value.split(/\s+/).filter(Boolean).length >= 4
    ) {
      found.push({ line: text.slice(0, cursor).split("\n").length, value });
    }
    cursor = end;
  }
  return found;
}

/** Tool source: the extensions that declare the tools and their descriptions. */
function extensionFiles() {
  return readdirSync(join(repoRoot, "packages/piing/extensions"))
    .filter((file) => file.endsWith(".ts"))
    .sort()
    .map((file) => `packages/piing/extensions/${file}`);
}

/** Every string at a `description` key, at any depth. */
export function descriptionStrings(value, key = null, collected = []) {
  if (typeof value === "string") {
    if (key === "description") collected.push(value);
  } else if (Array.isArray(value)) {
    for (const item of value) descriptionStrings(item, key, collected);
  } else if (value && typeof value === "object") {
    for (const [childKey, child] of Object.entries(value)) {
      descriptionStrings(child, childKey, collected);
    }
  }
  return collected;
}

/**
 * TypeScript with its comments removed.
 *
 * Comments in an extension are engineer-facing, and two of them narrate the
 * transport error strings (`chiefd docstore …`, `chiefd rejected …`) that the
 * template-literal exclusion already covers in code. Model-facing copy in
 * these files is in the description strings and the refusal remedies, all of
 * which are executable lines.
 */
function withoutComments(text) {
  return text
    .split("\n")
    .filter((line) => !/^\s*(\/\/|\*|\/\*)/.test(line))
    .join("\n");
}

/**
 * Record the glossary hits of `texts` under `path`, ONE finding per rule.
 *
 * Grouped per rule rather than per hit because the register counts a
 * `{ path, rule }` pair; two surfaces that group differently would make the
 * same violation register two different ways.
 *
 * `texts` is a LIST and is deliberately never joined: the noun-`assignment`
 * pattern separates its determiner from its noun with `\s+`, which spans a
 * newline, so a Rust literal ending in "the" next to one beginning
 * "assignment" would invent a hit that neither string contains. A skill is
 * one text; a Rust file is one text per prose literal.
 */
function recordGlossary(record, path, texts) {
  const byRule = new Map();
  for (const text of texts) {
    for (const [rule, hit] of glossaryHits(text)) {
      if (!byRule.has(rule)) byRule.set(rule, []);
      byRule.get(rule).push(hit);
    }
  }
  for (const [rule, hits] of [...byRule].sort()) record(path, rule, hits);
}

/**
 * The string literals in an extension that a reader could read.
 *
 * Three conditions, each measured against this tree:
 *
 *   * a SPACE and two letters — `"launcher"` as a `fromPersonId` value is a
 *     wire id, exactly as exempt as a keyspace, and it has neither;
 *   * no literal NEWLINE — real tool copy here is one line, while a multi-line
 *     template is code (an extension's env-file reader produced seven phantom
 *     `launcher` hits inside one such blob before this condition existed);
 *   * comments already removed — an engineer-facing comment may name whatever
 *     it is explaining.
 *
 * Regex literals are excluded by construction: they are delimited by `/`, not
 * by a quote, and the two that carry the retired word do so to MATCH a message
 * another program writes. Renaming those would delete a detection, not a name.
 */
export function extensionProse(text) {
  const source = withoutComments(text);
  const literals = source.matchAll(/"((?:[^"\\\n]|\\.)*)"|`([^`\\]*(?:\\.[^`\\]*)*)`/g);
  return [...literals]
    .map((match) => (match[1] === undefined ? match[2] : match[1]))
    .filter((value) => value && value.includes(" ") && /[A-Za-z]{2}/.test(value) && !value.includes("\n"));
}

/**
 * Every violation in the tree, as `{ path, rule, hits }`.
 *
 * The rule/surface matrix, and why it is not uniform:
 *
 *   | surface       | scanned text            | retired | verb | glossary |
 *   |---------------|-------------------------|---------|------|----------|
 *   | skills md     | whole file              | yes     | +fen | yes      |
 *   | linked docs   | whole file              | yes     | +fen | no       |
 *   | extensions ts | non-comment lines,      | prose   | yes  | prose    |
 *   |               | prose literals for the  |         |      |          |
 *   |               | retired name AND the    |         |      |          |
 *   |               | glossary                |         |      |          |
 *   | rust crates   | prose string literals   | no      | no   | yes      |
 *
 * The glossary column covers Rust, skills and extensions.
 * `apps/web/test/utils/Glossary.test.ts` already runs the identical three
 * patterns over the web components.
 *
 * The skills column was enrolled after its two live findings were fixed at
 * source (the manager skill's SKILL.md -- then named
 * `organization-management` -- and its "bounded
 * assignments", and the `schema-org-delegate.json` fixture's "owned task",
 * which the generated fixture still carried after the schema it mirrors had
 * already been repaired). That stale fixture is the reason the column moved:
 * a banned phrase survived its own fix for as long as nothing scanned the
 * copy, and the next one would too. Skills are scanned WHOLE — a skill is
 * handed to a Pi session verbatim, so it has no identifier column to exempt,
 * unlike the extension TypeScript.
 *
 * The generated tool schemas are still NOT glossary-scanned, and that remains
 * a scope statement rather than a claim they are clean: their `description`
 * strings are generated from the extension registrations, so a hit there is
 * fixed at source and regenerated, and the fixtures for the delegate tool
 * carry `delegation` in their own test-narrating descriptions. Enrolling them
 * needs the source/fixture regeneration seam, not a register row. The register
 * below is for sites a packet chose not to fix INSIDE a scanned surface; a
 * surface nothing scans is not registered, it is simply not yet covered.
 *
 * The extensions' retired-name column was OFF, on a measured ground that has
 * since been re-measured: `launcher` was said to appear 99 times in non-comment
 * extension lines, essentially all identifiers, and a rule that fires 99 times
 * on day one is a rule nobody runs. The population today is 33, and the split
 * is not close — 3 sentences a model reads, 5 uses of the wire value
 * `fromPersonId: "launcher"`, 23 identifiers (`launcherRoot`,
 * `LauncherSystemNoticePresentation`, `inheritedLauncherRoot`,
 * `launcherAppliedModel`, `ORG_LAUNCHER_ROOT`) and 2 regexes that MATCH text a
 * different program writes (`/(?:ChiefD|Launcher) command ended without an exit
 * status/`), which a rename would break rather than clean.
 *
 * The extensions' GLOSSARY column was held back for one commit, and the
 * reason is the register's own clean-row rule rather than any doubt about the
 * surface: of the ten glossary hits then in extension prose, five were already
 * fixed on `fix/tool-copy-remainder`, which had not yet landed. Enrolling
 * before it landed would have registered five rows that go CLEAN the moment it
 * does — and a clean row FAILS by design, so the enrolment would have broken
 * the build of the branch that fixed it. That branch has landed. The
 * population today is 4, all in `organization-intercom.ts`, all
 * `glossary-noun-assignment`, and all four are the `ASSIGNMENT <id>` /
 * `assignmentId` / `completeAssignment` completion protocol that
 * chief/CLAUDE.md names a STANDING EXCEPTION — registered with that reason,
 * never reworded, because rewording prose that names the same object as the
 * parameter beside it is exactly the half-migration the exception exists to
 * prevent.
 *
 * So the column is on, against PROSE STRING LITERALS rather than lines. That
 * boundary is the rule CLAUDE.md already states — copy is governed, identifiers
 * are exempt — expressed structurally instead of as an allowlist that would
 * need a row per identifier and a new row per rename. Measured against the real
 * tree it fires 3 times, on the 3 sentences, and on nothing else.
 */
export function scanRepo(verbs, daemonModes) {
  const findings = [];
  const record = (path, rule, hits) => {
    if (hits.length > 0) findings.push({ path, rule, hits });
  };

  for (const path of [...skillFiles(), ...docFiles()]) {
    const text = readRepo(path);
    record(path, "retired-name", retiredNameHits(text));
    record(path, "unrouted-verb", unroutedVerbHits(text, { fences: true }, verbs, daemonModes));
  }

  for (const path of skillFiles()) {
    recordGlossary(record, path, [readRepo(path)]);
  }

  for (const path of extensionFiles()) {
    const text = readRepo(path);
    const prose = extensionProse(text);
    record(path, "unrouted-verb", unroutedVerbHits(withoutComments(text), { fences: false }, verbs, daemonModes));
    record(path, "retired-name", prose.flatMap(retiredNameHits));
    // The list is passed UNJOINED for the reason `recordGlossary` states: the
    // noun-`assignment` pattern spans a newline, so a literal ending "the"
    // beside one beginning "assignment" would invent a hit neither contains.
    recordGlossary(record, path, prose);
  }

  for (const path of rustCopyFiles()) {
    recordGlossary(record, path, rustProse(readRepo(path)).map(({ value }) => value));
  }

  return findings;
}

// ───────────────────────────────────────────────────────────────────────────
// The register of known sites
// ───────────────────────────────────────────────────────────────────────────

/**
 * Sites that were RED when this guard landed and are not this packet's to fix.
 *
 * NOT AN ALLOWLIST, and the difference is the whole point. An allowlist is a
 * permission that survives the thing it permitted; every row here is a
 * MEASURED FACT with an exact count and a date, checked in both directions:
 *
 *   * a registered path that no longer exists FAILS — the row is stale;
 *   * a registered site that is now CLEAN FAILS — the fix landed, delete the
 *     row in the same commit;
 *   * a count that moved either way FAILS, naming the delta.
 *
 * So a row cannot outlive its subject, and cleaning a site is never silently
 * absorbed. Line numbers are deliberately absent: they rot on the next edit
 * and would make this register wrong for reasons that have nothing to do with
 * copy.
 */
export const KNOWN_SITES = [
  // ── the retired name, the extension surface ───────────────────────────────
  {
    path: "packages/piing/extensions/organization-intercom.ts",
    rule: "retired-name",
    count: 3,
    registeredOn: "2026-08-10",
    owner: "the launcher-name retirement",
    note:
      "The two SOURCES of the schema-org-send and schema-org-escalate-to-operator rows above — `org_send`'s \"'launcher' is never a recipient\" and `org_escalate_to_operator`'s \"do NOT try to message a person named 'launcher'\" — plus the `'launcher' is infrastructure, never a message recipient` refusal they describe. Registered rather than reworded because they are TRUE: `launcher` is still the real `fromPersonId` chiefd stamps on a system notice, so a model warned about that id is being warned about a value it will actually see. The name is retired as a PRODUCT, not on the wire. Rewording these three while the wire keeps the value would leave a model unable to recognise the id it is being warned about. Two sites, two languages: these three strings and chiefd-core's agent_contracts.rs \"`launcher` is never a person\" — they move in one commit or not at all. This read \"four sites\" and counted two generated conformance fixtures until those fixtures were deleted with the rest of the quarantined corpus; the coupling is real, the count was not.",
  },
];

function registerKey(entry) {
  return `${entry.path} [${entry.rule}]`;
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

test("every daemon mode the installed `chiefd` forwards is a mode `chiefd` really serves, and vice versa", () => {
  // THE P6 SEAM, in both directions. The operator types `chiefd run`; the
  // client `exec`s `chiefd run`. Two tables, two crates, and neither
  // compiler can see the other — so this is the only place the pair is held
  // together.
  //
  //   * a mode the daemon serves but the client does not forward is
  //     UNREACHABLE through the name on the operator's PATH — a working
  //     invocation silently becoming `unknown command`;
  //   * a name the client forwards that the daemon does not serve `exec`s
  //     into a refusal from a second program, which is exactly the
  //     `unknown command 'chiefd'` failure the Bun delegation used to produce
  //     and which this whole guard file exists because of.
  const forwarded = clientForwardedDaemonVerbs(readRepo(CLIENT_MAIN_RS));
  const served = daemonDispatchedModes(readRepo(DAEMON_MAIN_RS));
  // The floor is an anti-vacuity check, not a target: a parse that returned
  // one entry, or none, would make the equality below pass while seeing
  // nothing. It reads 4 rather than 5 because deleting the `docstore-only`
  // mode took the real table from five modes to four, on both sides — the
  // number tracks the tree and is never carried from memory.
  assert.ok(forwarded.length >= 4, `implausible forward table (${forwarded.length}) — the parse is broken`);
  assert.ok(served.length >= 4, `implausible daemon mode table (${served.length}) — the parse is broken`);
  assert.deepEqual([...forwarded].sort(), [...served].sort());
  // And `host` is NOT among them: it is served by the client itself
  // (`crates/chief-cli/src/host/`), so forwarding it would send `apps/api`'s
  // whole company-lifecycle surface into a program that does not have it.
  assert.ok(!forwarded.includes("host"), "`chief host` is the client's own mode, not a forwarded one");
  assert.ok(!served.includes("host"), "the daemon must not claim `host`");
});

test("the chiefd verb table is derived from the binary's own routing", () => {
  const verbs = deriveChiefdVerbs(readRepo(CLIENT_MAIN_RS), readRepo(DAEMON_MAIN_RS));

  // The operator surface, individually, exactly as lifecycle.rs's own tests
  // assert reachability by name.
  for (const verb of ["ls", "attach", "stop", "reset", "help"]) {
    assert.ok(verbs.has(verb), `the derivation must find the routed verb '${verb}'`);
  }
  // The daemon modes main.rs dispatches. `docstore-only` is deliberately NOT
  // here: the mode is deleted, so the name belongs with the invented verbs
  // below, and a derivation that finds it again has found a resurrection.
  for (const mode of ["run", "host", "bootstrap-store", "set-actuation-config", "clear-breaker"]) {
    assert.ok(verbs.has(mode), `the derivation must find the daemon mode '${mode}'`);
  }
  // ANTI-VACUITY IN THE OTHER DIRECTION, which matters more: a derivation that
  // accidentally swept up every string literal in either file would make this
  // guard incapable of failing while looking perfectly healthy. These are the
  // exact verbs the sweep found taught to models, and not one of them is real.
  // `docstore-only` leads the list because it is the one name here that WAS
  // real: the mode, its dispatch row and its harness are deleted, so a
  // `chiefd docstore-only` in model-facing copy now `exec`s into a refusal.
  for (const invented of [
    "new",
    "create",
    "docstore-only",
    "catalog",
    "company",
    "department",
    "boot",
    "launch",
    "tree",
    "show",
    "start",
  ]) {
    assert.ok(
      !verbs.has(invented),
      `'chiefd ${invented}' is not routed anywhere — the derivation is too broad`
    );
  }
  assert.ok(verbs.size >= 12 && verbs.size <= 40, `implausible verb table size ${verbs.size}`);
});

test("every scanned surface is non-empty (the floor that stops a silent no-op)", () => {
  // A guard that scans nothing passes. Each floor is well below today's count
  // and well above zero, so a glob that stops resolving fails here by name
  // rather than reporting a clean tree.
  /** @type {[string, string[], number][]} */
  const surfaces = [
    // 8 -> 2: the browser, fal-ai, market-data and project-status-reporting
    // skills were deleted and organization-management became `manager`, so the
    // real count is 3 (founder/SKILL.md, founder/AGENTS.md, manager/SKILL.md).
    // Re-anchored below the tree the surface actually has, not lowered to make
    // a scan pass: a floor the tree can no longer reach makes the surface
    // refuse rather than report, which is strictly worse than a smaller floor
    // that is still well above zero.
    ["skills", skillFiles(), 2],
    ["linked docs", docFiles(), 4],
    // 12 -> 10: `extensions/zipbox-tribe-addons.ts` is deleted with
    // provider/model management, so the real count is 11 and the floor stays
    // below it rather than landing on it.
    ["extensions", extensionFiles(), 10],
    ["rust crates", rustCopyFiles(), 120],
  ];
  for (const [name, files, floor] of surfaces) {
    assert.ok(files.length >= floor, `${name}: ${files.length} files, floor ${floor}`);
    for (const file of files) {
      assert.ok(readRepo(file).length > 0, `${file} is empty`);
    }
  }

  // The tool-fixture surface is GONE, not disabled. The 121 quarantined
  // `schema-org-*` fixtures it scanned were deleted on the operator's ruling;
  // the 16 that remain are recorded OPERATIONS with a live Rust runner and
  // carry no tool copy at all. Scanning them would be a surface that cannot
  // fail, which is exactly what the floors above exist to refuse — so the
  // surface was removed rather than given a lower floor.

  // And the Rust surface must really yield PROSE. A file floor alone cannot
  // catch the failure that matters here: `rustProse` narrows 270 files down to
  // the sentences inside them, and a broken literal scanner, a `#[cfg(test)]`
  // blanker that swallows a whole file, or a word threshold raised one notch
  // too far would leave 270 files scanned and nothing read. The floor is half
  // the real count (1,945 at the time of writing) — low and wide, a vacuity
  // refusal rather than an inventory nobody may change.
  const prose = rustCopyFiles().reduce((total, file) => total + rustProse(readRepo(file)).length, 0);
  assert.ok(prose >= 900, `only ${prose} prose strings across the chiefd crates`);

  // And the EXTENSION surface must really yield prose, for the same reason and
  // now for two columns rather than one. `extensionProse` narrows 17 files to
  // the readable strings inside them through three conditions — a space, two
  // letters, no newline — and any one of them tightened by a notch, or a
  // literal scanner that stopped matching template strings, would leave 17
  // files scanned and nothing read while both the retired-name column and the
  // glossary column reported a clean tree. The floor is roughly half the real
  // count (1,299 at the time of writing): a vacuity refusal, not an inventory.
  const extensionCopy = extensionFiles().reduce(
    (total, file) => total + extensionProse(readRepo(file)).length,
    0
  );
  assert.ok(extensionCopy >= 600, `only ${extensionCopy} prose literals across the extensions`);
});

test("the register describes reality: no stale path, no clean row, no drifted count", () => {
  const verbs = deriveChiefdVerbs(readRepo(CLIENT_MAIN_RS), readRepo(DAEMON_MAIN_RS));
  const daemonModes = new Set(daemonDispatchedModes(readRepo(DAEMON_MAIN_RS)));
  const findings = new Map(
    scanRepo(verbs, daemonModes).map((finding) => [registerKey(finding), finding])
  );

  const seen = new Set();
  for (const entry of KNOWN_SITES) {
    const key = registerKey(entry);
    assert.ok(!seen.has(key), `${key} is registered twice`);
    seen.add(key);

    assert.ok(entry.count > 0, `${key}: a registered site with count 0 records nothing`);
    assert.match(entry.registeredOn, /^\d{4}-\d{2}-\d{2}$/, `${key}: needs a registration date`);
    assert.ok(entry.owner && entry.note, `${key}: needs an owner and a note`);

    let exists = true;
    try {
      readRepo(entry.path);
    } catch {
      exists = false;
    }
    assert.ok(exists, `${key}: registered path no longer exists — delete this row`);

    const finding = findings.get(key);
    assert.ok(
      finding,
      `${key}: registered as ${entry.count} known violation(s) and the file is now CLEAN. ` +
        `The fix landed; delete this row in the same commit.`
    );
    assert.equal(
      finding.hits.length,
      entry.count,
      `${key}: registered ${entry.count}, found ${finding.hits.length} ` +
        `(${JSON.stringify([...new Set(finding.hits)])}). Re-measure and update the row.`
    );
  }
});

test("no unregistered surface tells an operator or a model something untrue", () => {
  const verbs = deriveChiefdVerbs(readRepo(CLIENT_MAIN_RS), readRepo(DAEMON_MAIN_RS));
  const registered = new Set(KNOWN_SITES.map(registerKey));
  const daemonModes = new Set(daemonDispatchedModes(readRepo(DAEMON_MAIN_RS)));
  const unregistered = scanRepo(verbs, daemonModes)
    .filter((finding) => !registered.has(registerKey(finding)))
    .map((finding) => `${registerKey(finding)}: ${JSON.stringify([...new Set(finding.hits)])}`);

  assert.deepEqual(
    unregistered,
    [],
    `copy that names a retired product or an unrouted verb:\n  ${unregistered.join("\n  ")}`
  );
});

test("both rules FIRE — a negative self-test against a tampered fixture tree", () => {
  // A guard never seen to fail is indistinguishable from one that cannot.
  // Every assertion below runs the REAL rule functions over deliberately
  // rotten text, so the detector itself is under test rather than the tree.
  const fixtureRoot = mkdtempSync(join(tmpdir(), "model-facing-copy-"));
  try {
    const verbs = deriveChiefdVerbs(readRepo(CLIENT_MAIN_RS), readRepo(DAEMON_MAIN_RS));

    // --- rule 1: retired names, in every spelling the sweep found ---
    const skillDir = join(fixtureRoot, "skills", "rotten");
    mkdirSync(skillDir, { recursive: true });
    const rottenSkill = [
      "---",
      "name: rotten",
      "---",
      "Ask the Tribe Launcher to start it, or use launcher mode.",
      "The Launcher owns this; triber did too.",
      "Pass `--replace-launcher-pane` when replacing the current pane.",
    ].join("\n");
    writeFileSync(join(skillDir, "SKILL.md"), rottenSkill);

    const retired = retiredNameHits(readFileSync(join(skillDir, "SKILL.md"), "utf8"));
    assert.deepEqual(
      retired,
      ["Launcher", "launcher", "Launcher", "triber", "launcher"],
      "Tribe Launcher, launcher mode, The Launcher, triber, --replace-launcher-pane"
    );

    // --- and the exemptions really exempt, so the rule stays runnable ---
    const internalOnly = [
      'bun "$ORG_LAUNCHER_ROOT/packages/piing/skills/manager/runtime/run.ts" fred',
      "ORG_LAUNCHER_ORG_DIR cannot redirect this location.",
      "isForbiddenLauncherResource refuses it.",
    ].join("\n");
    assert.deepEqual(retiredNameHits(internalOnly), [], "internal identifiers must not fire");

    // --- and the extension surface really is copy-only ---
    // The whole column rests on this boundary, so it is demonstrated rather
    // than asserted in a comment: one prose sentence fires, and the four
    // shapes CLAUDE.md exempts do not.
    const extension = [
      'const sender = { fromPersonId: "launcher" };',
      'const root = requiredEnvironment(environment, "ORG_LAUNCHER_ROOT");',
      'let launcherAppliedModel;',
      'const ended = /(?:ChiefD|Launcher) command ended without an exit status/i;',
      'const blob = `line one',
      '  launcherRoot?: string;',
      'line three`;',
      'const refusal = "\'launcher\' is infrastructure, never a message recipient.";',
    ].join("\n");
    assert.deepEqual(
      extensionProse(extension).flatMap(retiredNameHits),
      ["launcher"],
      "exactly the sentence — not the wire value, the env var, the identifier, the matcher regex or the multi-line blob"
    );
    assert.deepEqual(
      extensionProse('const id = "launcher";').flatMap(retiredNameHits),
      [],
      "a bare wire id is a value, not copy"
    );
    assert.deepEqual(
      extensionProse('// launcher owns this pane\nconst kept = "no retired name here";').flatMap(retiredNameHits),
      [],
      "an engineer-facing comment may name whatever it explains"
    );

    // --- rule 2: the dead CLI namespace the sweep deleted ---
    const rottenDoc = [
      "Create it with `chiefd company create <name>` and then `chiefd company boot`.",
      "Inspect with `chiefd catalog --json`; the tree is `chiefd company tree`.",
      "",
      "```bash",
      "chiefd department launch engineering",
      "chief attach acme          # this one is real",
      "```",
      "",
      "```text",
      "chiefd   POST /v1/org/*     a route table, not a command",
      "```",
    ].join("\n");
    const unrouted = unroutedVerbHits(rottenDoc, { fences: true }, verbs);
    assert.deepEqual(
      unrouted.sort(),
      ["chiefd catalog", "chiefd company", "chiefd company", "chiefd company", "chiefd department"],
      JSON.stringify(unrouted)
    );

    // --- and the narrowness holds, or the guard is unusable ---
    const honest = [
      "chiefd derives the catalog itself and chiefd already knows the ids.",
      "Run `chief ls`, `chief attach`, or just `chiefd`.",
      "`chiefd bootstrap-store` seeds one document; `chief --help` prints usage.",
      "throw new Error(`chiefd docstore ${path} returned an invalid outcome`)",
      "",
      "```text",
      "chiefd/               the binary: the operator verbs, `host`, and the runtime",
      "```",
      "",
      "```bash",
      "chiefd        # list your companies, starting nothing",
      "```",
    ].join("\n");
    assert.deepEqual(unroutedVerbHits(honest, { fences: true }, verbs), []);

    // --- rule 2 reaches JSON description copy at any depth ---
    const schema = {
      name: "org_hire",
      description: "Select ids from `chiefd catalog --json`.",
      inputSchema: {
        properties: {
          person: {
            properties: {
              skills: { items: { description: "Exact id from `chiefd catalog --json`." } },
            },
          },
        },
      },
      // NOT copy: the recorded boundary call. Its body deliberately carries an
      // unrouted `chiefd company create` — if the scanner ever widened past
      // `description`, this would show up in the hit list below.
      expectState: [
        {
          read: "tools.chiefd_calls",
          equals: [{ path: "/v1/org/person/hire", body: { note: "chiefd company create acme" } }],
        },
      ],
    };
    const copy = descriptionStrings(schema).join("\n");
    assert.equal(descriptionStrings(schema).length, 2, "both depths must be reached");
    assert.deepEqual(unroutedVerbHits(copy, { fences: false }, verbs), [
      "chiefd catalog",
      "chiefd catalog",
    ]);
    assert.deepEqual(retiredNameHits(copy), [], "the argv and observable key are not copy");

    // --- the derivation is load-bearing: drop a verb, the rule fires ---
    const withoutAttach = new Set([...verbs].filter((verb) => verb !== "attach"));
    assert.deepEqual(unroutedVerbHits("Run `chief attach acme`.", { fences: false }, withoutAttach), [
      "chief attach",
    ]);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test("rule 3 FIRES on every #375 collapse, and stays silent on the exempt vocabulary", () => {
  // The four bans, one line each, in the exact spellings the sweep found.
  assert.deepEqual(
    glossaryHits("Close the owned task, log the work item, and restate the objective."),
    [
      ["glossary-filler", "owned task"],
      ["glossary-filler", "work item"],
      ["glossary-filler", "objective"],
    ]
  );
  assert.deepEqual(
    glossaryHits("Give every assignment one owner and do not duplicate an assignment."),
    [
      ["glossary-noun-assignment", "every assignment"],
      ["glossary-noun-assignment", "an assignment"],
    ]
  );
  assert.deepEqual(glossaryHits("Every delegation is a goal; never open a second delegation."), [
    ["glossary-noun-delegation", "delegation"],
    ["glossary-noun-delegation", "delegation"],
  ]);

  // AND THE NARROWNESS HOLDS, or this rule is unusable on a Rust tree. Every
  // line below is real copy or a real identifier from the crates it scans.
  const exempt = [
    "assigned to @val, and the work is handed to one direct report",
    "the reminders keyspace and the effects table",
  ].join("\n");
  assert.deepEqual(glossaryHits(exempt), []);
});

test("rule 3 fires THROUGH the extension surface, and not on its identifiers", () => {
  // The column enrolled over `extensionProse` is only as good as that filter,
  // so the pair is proved end to end rather than assumed from the rule test
  // above: the same TypeScript shapes the retired-name column is proved
  // against, judged by the glossary instead.
  //
  // A guard nobody has watched fail is a guard nobody has tested, and the
  // failure this refuses is the one that looks healthiest — a filter that
  // stops yielding copy leaves both extension columns green over an empty
  // corpus.
  const rotten = [
    'const CLOSE = "Close the owned task and restate the objective.";',
    'description: "Give every assignment one owner before you delegate.",',
    'const NOTE = `Open one delegation per report.`;',
  ].join("\n");
  assert.deepEqual(
    extensionProse(rotten).flatMap(glossaryHits),
    [
      ["glossary-filler", "owned task"],
      ["glossary-filler", "objective"],
      ["glossary-noun-assignment", "every assignment"],
      ["glossary-noun-delegation", "delegation"],
    ],
    "a description string, a const and a template literal are all model-facing copy"
  );

  // And the four exempt shapes CLAUDE.md names stay silent: a keyspace, a
  // parameter name, a route and an engineer-facing comment. Each is code, and
  // each carries the banned vocabulary on purpose.
  const exempt = [
    'const KEYSPACE = "assignments";',
    'assignmentId: Type.Optional(Type.String()),',
    'const ROUTE = "/v1/org/supervision/assignments";',
    '// every delegation the objective tracker owns is one work item',
    'const ok = "assigned to @val, and the goal is delegated to one direct report";',
  ].join("\n");
  assert.deepEqual(extensionProse(exempt).flatMap(glossaryHits), []);

  // The unjoined-list contract, demonstrated: joining these two literals would
  // manufacture "the\nassignment", a hit neither string contains.
  assert.deepEqual(extensionProse('const a = "one owner for the";\nconst b = "assignment lives here";').flatMap(glossaryHits), []);
});

test("the Rust surface reads shipped COPY and nothing else", () => {
  // Each exclusion below removed a whole class of false positive when it was
  // measured against the real tree, so each is proved rather than described.
  const source = [
    '//! A resolved handle to one work item — the module doc, not copy.',
    "/* an objective, in a block comment */",
    'const ROUTE: &str = "/v1/tasks/{id}/assignments";',
    'const CODE: &str = "assignment-generation-invalid";',
    'pub const SQL: &str = "CREATE TABLE assignments(id TEXT, the assignment TEXT)";',
    "pub fn refuse() -> String {",
    '    format!("Person \'{id}\' has an assignment without a home")',
    "}",
    "#[cfg(test)]",
    "mod tests {",
    "    #[test]",
    "    fn it_holds() {",
    '        assert!(ok, "the assignment is a durable relational row");',
    '        let banned = "this is an objective and a work item";',
    "    }",
    "}",
  ].join("\n");

  const prose = rustProse(source);
  assert.deepEqual(
    prose.map((entry) => entry.value),
    ["Person '{id}' has an assignment without a home"],
    "the doc comment, the block comment, the route, the code, the SQL and the whole #[cfg(test)] module must all be invisible"
  );
  assert.equal(prose[0].line, 7, "the reported line must be the real one, offsets preserved");
  assert.deepEqual(glossaryHits(prose[0].value), [["glossary-noun-assignment", "an assignment"]]);

  // The diagnostic exclusion is about the CALL, not the file: an `expect`
  // message inside shipped code is still a diagnostic.
  assert.deepEqual(
    rustProse('let row = lookup().expect("the assignment being fenced never vanishes");'),
    []
  );

  // A `#[cfg(test)]` module must not swallow what follows it.
  const after = rustProse(
    ["#[cfg(test)]", "mod tests {", '    let x = "an assignment inside the tests";', "}", 'pub const AFTER: &str = "an assignment after the tests";'].join("\n")
  );
  assert.deepEqual(after.map((entry) => entry.value), ["an assignment after the tests"]);
});

test("the Rust file list is DERIVED from the tree, and excludes harness prose", () => {
  const files = rustCopyFiles();
  assert.ok(files.length >= 120, `implausible Rust surface (${files.length} files)`);
  assert.ok(
    files.every((file) => file.startsWith(`${RUST_ROOT}/`) && file.endsWith(".rs")),
    "every scanned path must be a .rs file under the chiefd crates"
  );
  assert.ok(
    !files.some((file) => file.split("/").includes("tests") || file.endsWith("/tests.rs")),
    "harness prose must not be scanned as shipped copy"
  );
  // Anti-vacuity in the other direction: the files this rule EXISTS for.
  for (const file of [
    "apps/chiefd/crates/chiefd-core/src/store/agent_contracts.rs",
    "apps/chiefd/crates/chief-cli/src/main.rs",
    "apps/chiefd/crates/chiefd-api/src/docstore/router.rs",
  ]) {
    assert.ok(files.includes(file), `the derivation must find ${file}`);
  }
});

test("the register's own failure modes fire — stale path, clean row, drifted count", () => {
  // The register is the part of this guard most likely to rot quietly, so its
  // three refusals are proven against synthetic findings rather than trusted.
  const findings = new Map([
    ["a.md [retired-name]", { path: "a.md", rule: "retired-name", hits: ["launcher"] }],
  ]);
  const check = (entry) => {
    const finding = findings.get(registerKey(entry));
    if (!finding) return "clean";
    if (finding.hits.length !== entry.count) return "drifted";
    return "ok";
  };

  assert.equal(check({ path: "a.md", rule: "retired-name", count: 1 }), "ok");
  assert.equal(
    check({ path: "a.md", rule: "retired-name", count: 3 }),
    "drifted",
    "a count that moved must be refused, in EITHER direction"
  );
  assert.equal(
    check({ path: "b.md", rule: "retired-name", count: 1 }),
    "clean",
    "a registered site with no remaining violation must be refused"
  );

  // A stale path is the third: every registered path is read from the real
  // tree in the register test above, and this proves the read is what decides.
  assert.throws(
    () => readRepo("docs/a-file-that-was-deleted.md"),
    "a registered path that no longer exists must throw, not be skipped"
  );
});
