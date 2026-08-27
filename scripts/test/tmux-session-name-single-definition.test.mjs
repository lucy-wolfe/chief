// A company's tmux session name is spelled in ONE place per language, and the
// tmux-target spellings all carry the terminator.
//
// # The defect this closes
//
// `bc67fe2d2` moved the company session convention from `org-<slug>` to
// `org-<slug>_`. The terminator is load-bearing: `tmux -t <name>` matches
// exactly first and falls back to PREFIX, so before it a probe for a STOPPED
// `acme` resolved to a RUNNING `acme-corp` — `chief attach acme` walked the
// operator into another company's panes. A canonical slug is `[a-z0-9-]`
// only, so a terminator a slug cannot contain makes that collision
// structurally impossible (see `placement.rs::session_name_for_slug` for the
// two-line proof).
//
// `chief-cli` moved. `scripts/deploy/lib/deploy-common.sh` and
// `scripts/deploy/live_verify.py` did not, and stayed wrong for weeks in two
// different ways:
//
//   * `tmux -t "org-$SLUG"` — prefix-collidable, the exact fault the
//     terminator exists to remove;
//   * `grep -Fqx "org-$SLUG"` — an EXACT-match probe that, after the move,
//     could no longer match any live session at all. `company_pane_snapshot`
//     returned empty for every real company, so `assert_company_panes_unchanged`
//     compared "" against "" and passed. A safety gate that protects Pi panes
//     across a daemon hand-off silently checked nothing.
//
// # Why it did not reach the shell, which is the part worth fixing
//
// Because there was never ONE definition to follow. The convention has always
// had more than one producer, and `bc67fe2d2` moved one of them:
//
//   * `chief-cli::placement::session_name_for_slug` — the TMUX TARGET. What
//     `attach`/`stop`/the actuator hand to `tmux -t`. Carries the terminator.
//   * `chiefd_core::store::organization::runtime_session_for_slug` — the
//     `sessionName` DOCUMENT FIELD on the CEO boot lease, the launch intent
//     and the quiesce row. Never handed to tmux. Does NOT carry the
//     terminator, and its TypeScript twin
//     (`organization-intercom.ts::conventionalRuntimeSession`) re-derives the
//     same bare string in order to VALIDATE that field. The two must agree
//     with each other or lease validation rejects every lease.
//
// So this file does not demand one global spelling — that would be a lie about
// a system with two genuinely different values. It demands that every spelling
// is a NAMED producer with a stated job, that the tmux-target producers all
// carry the terminator, and that the doc-field pair moves together. A fourth
// copy, in any language, fails here by file and line.
//
// This guard once had a BEHAVIOURAL half that drove the real bash functions in
// `scripts/deploy/lib/deploy-common.sh` against a stubbed tmux; the go-public
// redaction (E.2) deleted that private deploy tree, so the behavioural half was
// retired with its subject and only the structural check below remains.
//
// Run with `node --test scripts/test/tmux-session-name-single-definition.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { skipSet } from "../tree-walk-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** The one file the convention is DEFINED in, terminator and all. */
const PLACEMENT_RS = "apps/chiefd/crates/chief-cli/src/placement.rs";

/**
 * The producers that hand a name to `tmux -t`. Each must spell the terminator
 * — either as the Rust constant itself or as its literal value.
 */
const TMUX_TARGET_PRODUCERS = new Set([
  PLACEMENT_RS,
  // `scripts/deploy/lib/deploy-common.sh` and `scripts/deploy/live_verify.py`
  // were producers here until the go-public redaction (E.2) deleted the whole
  // private deploy tree. The Rust constant in `placement.rs` is the single
  // definition again; nothing under `scripts/deploy/` spells a session name any
  // more because there is no `scripts/deploy/`.
  // The cold-start latency harness looks for the company's session and the
  // actuator's to prove its start is COLD, and to observe the pane it is
  // timing. It cannot import the Rust constant, so it spells the name itself
  // and is held to it here — which is the whole point of this register: the
  // guard caught this copy on the packet's first gate, which is the outcome
  // this file exists for rather than an obstacle to it.
  //
  // The spelling lives in the harness's LIBRARY, not the harness, because the
  // rule has to be testable: `scripts/test/cold-start-latency.test.mjs` drives
  // it against a warm fixture. The instrument itself runs `main()` at module
  // scope and cannot be imported.
  "scripts/cold-start-latency-lib.mjs",
]);

/**
 * The producers of the `sessionName` DOCUMENT FIELD — now EMPTY, and kept as an
 * empty table rather than deleted.
 *
 * #751 deleted the field itself. It was `"org-" + slug` derived on read and
 * stored nowhere, guarded by two validators that could not fail: the Rust
 * compared a constant against itself, and the TypeScript assigned the value it
 * then checked. Both producers are gone, so the "do the two still agree" test
 * that lived here has no subject and went with them — a guard whose subject was
 * deleted is retired, never weakened into passing.
 *
 * The empty table stays because the stray-spelling check below reads it: a NEW
 * `org-<slug>` document-field producer must land as a deliberate row here, not
 * slide in as an unnamed spelling.
 */
const DOC_FIELD_PRODUCERS = {};

/**
 * The one surviving `org-<slug>` derivation, and what it is FOR.
 *
 * `organization::runtime_session_for_slug` outlived the field it used to fill.
 * Its only remaining callers are the two zero-loss shadow-diff verifiers
 * (`launch_intent_rows`, `goal_delivery_quiesce_rows`) — `boot_lease_rows` was
 * the third until chief-home-is-cwd §4c deleted it whole — which
 * read `sessionName` out of a HISTORICAL blob's `extra` and check it against
 * this derivation before recording the key as `Derived` rather than `Lost`.
 *
 * That is verification of retired data, not production of a live value, and it
 * is why the function is not deleted with the field: the migration's own proof
 * needs to know what the retired key should have contained. Named here so the
 * stray check stays exhaustive — if a live producer ever reappears at this path,
 * it lands as a deliberate edit to this list.
 */
const RETIRED_KEY_VERIFIER = "apps/chiefd/crates/chiefd-core/src/store/organization.rs";

/**
 * Paths whose spellings are evidence rather than definition. A test that
 * asserts an OBSERVABLE session name legitimately writes it out in full —
 * that is what makes it a test and not a restatement of the code — and the
 * conformance host builds a fixture company, not a real one. Same exemption,
 * and same reasoning, as `beacond-port-single-definition.test.mjs`.
 */
const EVIDENCE_PATHS = [
  /(^|\/)tests?\//,
  /\.test\.(ts|tsx|mjs|js)$/,
  /^conformance\//,
  // This file names the patterns it forbids, which is unavoidable.
  /^scripts\/test\/tmux-session-name-single-definition\.test\.mjs$/,
];

const SCANNED_EXTENSIONS = [".rs", ".sh", ".py", ".ts", ".tsx", ".mjs", ".cjs", ".js"];
/**
 * Directories this scan never descends into — the shared definition, so a
 * second session-name "definition" cannot be found inside
 * `.claude/worktrees/<name>/`, which is another agent's checkout and not this
 * tree's code. This guard was one of the five that reported exactly that.
 */
const SKIPPED_DIRS = skipSet();

/** A slug being interpolated straight after `org-`, in any of the four
 * languages this repo writes: `org-{slug}` (Rust/Python), `org-${slug}` (TS),
 * `org-$SLUG` (shell), `org-%s` (shell printf). */
const INTERPOLATED = /org-(?:\{|\$|%s)/;

function sourceFiles(dir, collected = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (SKIPPED_DIRS.has(entry.name)) continue;
      sourceFiles(join(dir, entry.name), collected);
    } else if (SCANNED_EXTENSIONS.some((extension) => entry.name.endsWith(extension))) {
      collected.push(join(dir, entry.name));
    }
  }
  return collected;
}

/** Comment lines are excluded on purpose: several of them NARRATE this
 * incident (`tmux.rs`'s "`session_name_for_slug` to `format!(\"org-{slug}\")`",
 * this repo's own changelog entries), and a guard that forbade describing the
 * defect would delete the only record of why the rule exists. */
function isComment(line, path) {
  if (path.endsWith(".sh") || path.endsWith(".py")) return /^\s*#/.test(line);
  return /^\s*(\/\/|\*|\/\*)/.test(line);
}

function interpolatedSpellings() {
  const found = [];
  for (const absolute of sourceFiles(repoRoot)) {
    const path = relative(repoRoot, absolute).split(sep).join("/");
    const lines = readFileSync(absolute, "utf8").split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index];
      if (isComment(line, path)) continue;
      if (INTERPOLATED.test(line)) found.push({ path, line: index + 1, text: line.trim() });
    }
  }
  return found;
}

/** The terminator, read from its definition rather than retyped here. */
function sessionTerminator() {
  const source = readFileSync(join(repoRoot, PLACEMENT_RS), "utf8");
  const match = source.match(/pub const SESSION_TERMINATOR: char = '(.)';/);
  assert.ok(match, `${PLACEMENT_RS} must define SESSION_TERMINATOR as a char literal`);
  return match[1];
}

test("every slug-interpolated session name belongs to a named producer", () => {
  const stray = interpolatedSpellings().filter(
    ({ path }) =>
      !TMUX_TARGET_PRODUCERS.has(path) &&
      !(path in DOC_FIELD_PRODUCERS) &&
      path !== RETIRED_KEY_VERIFIER &&
      !EVIDENCE_PATHS.some((pattern) => pattern.test(path))
  );
  assert.deepEqual(
    stray.map(({ path, line, text }) => `${path}:${line}: ${text}`),
    [],
    "a company session name may only be built by a producer named in this file — a new copy is how the shell " +
      "spent weeks probing a session that no longer existed"
  );
});

test("every tmux-target producer spells the terminator", () => {
  const terminator = sessionTerminator();
  const spellings = interpolatedSpellings();
  for (const producer of TMUX_TARGET_PRODUCERS) {
    const hits = spellings.filter(({ path }) => path === producer);
    assert.equal(
      hits.length,
      1,
      `${producer} must build the session name exactly once, not ${hits.length} times: ` +
        JSON.stringify(hits, undefined, 2)
    );
    // THE WHOLE SHAPE, not just the tail: `org-` + slug + `-` + key +
    // terminator, which is `placement.rs::session_name_for` exactly.
    //
    // The discriminator used to be OPTIONAL here, and that hole is what let
    // `cold-start-latency.mjs` sit in this register for a whole stage spelling
    // `org-${slug}_` — a name ending in the terminator, so the check passed,
    // and matching no live session on any box, so its cold proof asserted the
    // absence of something that could never be present. A guard that is green
    // over a producer which can never match is worse than no guard.
    //
    // Both halves are load-bearing and neither is optional:
    //   * the KEY, because a slug names no company — two directories may hold
    //     companies called the same thing, and a tmux server is box-wide;
    //   * the TERMINATOR, because `tmux -t` falls back to PREFIX matching, so
    //     without it the name is a prefix of every longer one.
    const INTERPOLATION = String.raw`(?:\{[^}]*\}|\$\{[^}]*\}|\$\w+|%s)`;
    const [{ text, line }] = hits;
    const spelling = text.slice(text.search(INTERPOLATED));
    const withoutSlug = spelling.replace(new RegExp(String.raw`^org-${INTERPOLATION}`), "");
    assert.ok(
      new RegExp(String.raw`^-${INTERPOLATION}`).test(withoutSlug),
      `${producer}:${line} builds a tmux target with no company-key discriminator — two directories ` +
        `holding companies with the same slug would share one session, and the second attach would ` +
        `land the operator inside the first company's panes: ${text}`
    );
    const tail = withoutSlug.replace(new RegExp(String.raw`^-${INTERPOLATION}`), "");
    assert.ok(
      tail.startsWith(terminator) || tail.startsWith("{SESSION_TERMINATOR}"),
      `${producer}:${line} builds a tmux target that does not end in the terminator '${terminator}' — ` +
        `this is prefix-collidable with every company whose slug extends this one: ${text}`
    );
  }
});

test("the sessionName document field stays deleted, in both languages", () => {
  // The replacement for "do the two producers still agree". #751 deleted the
  // field, so the question is no longer whether the copies match — it is
  // whether a copy has come back. Both spellings are checked, because the
  // defect this whole file exists for is one language moving without the other.
  const reintroduced = [
    { path: "apps/chiefd/crates/chiefd-core/src/store/context.rs", pattern: /fn session_name\(/ },
    { path: "packages/chiefing/src/types/RowDocs.ts", pattern: /^\s*sessionName: string/m },
  ].filter(({ path, pattern }) => pattern.test(readFileSync(join(repoRoot, path), "utf8")));

  assert.deepEqual(
    reintroduced.map(({ path }) => path),
    [],
    "the sessionName document field is back. It was `org-<slug>` derived on both sides and stored nowhere, " +
      "and the two validators guarding it were a tautology in TypeScript and a constant-against-itself " +
      "comparison in Rust. Reintroducing it restores a second source of truth for a value both sides compute."
  );
});

