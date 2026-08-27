// Every slug producer in the tree is a NAMED producer with a stated keyspace,
// and every producer of a COMPANY slug is tested against the exact validator.
//
// # The defect this closes
//
// `chief-cli/src/placement.rs::session_name_for_slug` carries a proof that two
// company tmux sessions can never prefix-collide. The conclusion is true. Its
// first fact was not:
//
//     A slug is `[a-z0-9-]` only ... `crate::paths::is_canonical_slug` is the
//     validator, and `genesis::slugify` is the ONLY producer.
//
// There is a second producer — `chiefd_core::store::organization_spec::slugify`
// — which mints `manifest.slug` and every department and person id derived from
// it. The proof survived only because that second producer independently
// enforces the same character set, and NO TEST ANYWHERE asserted that. Six
// comments across `placement.rs` and `tmux.rs` leaned on the same argument, so
// the day someone relaxed the second producer, six comments would have become
// false at once and the structural collision guarantee would have died in
// silence. A comment that is correct for a reason that has already changed
// survives review forever, which makes it more dangerous than a plainly wrong
// one.
//
// # Why the check is here and not in one Rust test
//
// It cannot be one Rust test. `is_canonical_slug` is `pub(crate)` inside the
// operator client's BINARY, and `chief-cli` and `chiefd-core` are forbidden to
// link in either direction by `backend-tmux-boundary.test.mjs` — the boundary
// is the architecture, not an accident. So each producer asserts the property
// in its own crate, and this file is what stops the two halves from drifting:
// it holds the corpora identical, holds the copied validator identical to the
// original, and — the part that actually rotted — enumerates the producers so a
// third one cannot land unnoticed.
//
// # How the enumeration works, and what it is exhaustive over
//
// BY SHAPE, not by name. A grep for the name `slugify` would have missed a
// producer called anything else, and this fleet has already been burned by an
// enumeration that was exhaustive over the wrong set. A slug producer collapses
// runs of unwanted characters into a single `-`, and in every language this
// repo writes that means one of two shapes: pushing a literal `'-'` (Rust), or
// replacing/joining into a literal `-` (TS/JS). Both are scanned across every
// source file in the tree. It is exhaustive over "code that builds a
// hyphen-joined token", which is the set that matters; it does not claim to
// find a producer that builds a slug some third way, and a producer that did
// would still have to satisfy the validator to reach a path join.
//
// Run with `node --test scripts/test/slug-producers-agree.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { skipSet } from "../tree-walk-lib.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

const GENESIS_RS = "apps/chiefd/crates/chief-cli/src/genesis.rs";
const ORGANIZATION_SPEC_RS = "apps/chiefd/crates/chiefd-core/src/store/organization_spec.rs";
const PATHS_RS = "apps/chiefd/crates/chief-cli/src/paths.rs";

/**
 * EVERY slug producer in the tree, each with the keyspace it produces into.
 *
 * `company` producers mint a company slug — a DISPLAY name. It is no longer a
 * path component: a company is the directory the operator ran `chief` in, and
 * nothing joins a slug into a path. What still rests on its shape is the tmux
 * SESSION NAME, `org-<slug>-<short key>_`, which an operator reads and types.
 * Those producers and only those must satisfy `paths.rs::is_canonical_slug`,
 * and each carries the property test named below.
 *
 * The premise narrowed and the rule did not: the character set has to hold for
 * a tmux target whatever else the slug stopped being, and the two producers
 * still have to agree with each other.
 *
 * A producer in another keyspace is named here with its reason and is NOT
 * asserted against the company rule. Asserting a property the product does not
 * require is how a guard goes red for a change that was correct.
 */
const PRODUCERS = {
  [GENESIS_RS]: {
    symbol: "slugify",
    keyspace: "company",
    why: "the slug bare `chief` mints; `launch` hands it to the path join and to the tmux session name",
  },
  [ORGANIZATION_SPEC_RS]: {
    symbol: "slugify",
    keyspace: "company",
    why: "`manifest.slug` and every department/person id derived from it, minted inside chiefd",
  },
};

/** The company-slug producers, and the test each one must carry. */
const PROPERTY_TEST = "no_input_makes_this_producer_emit_a_non_canonical_slug";

const SCANNED_EXTENSIONS = [".rs", ".ts", ".tsx", ".mjs", ".cjs", ".js", ".py", ".sh"];
/**
 * Directories this scan never descends into — the shared definition, so
 * `.claude/worktrees/<name>/` (another agent's full checkout of this repo) is
 * skipped here the same way it is everywhere else. This guard was one of the
 * five that reported slug producers inside somebody else's branch.
 */
const SKIPPED_DIRS = skipSet();

/**
 * The two shapes a collapse-into-a-hyphen takes in the languages this repo
 * writes. Rust pushes the character; TS/JS replaces or joins into it.
 */
const COLLAPSE_SHAPES = [/\.push\('-'\)/, /\.replace\([^)]*,\s*["'`]-/, /\.join\(["'`]-["'`]\)/];

/**
 * Lines whose hyphen-building is evidence or unrelated string work rather than
 * slug production. Each entry is a path and the reason it is not a producer;
 * an unexplained exemption is how the original "only producer" claim survived.
 */
const NOT_A_PRODUCER = {
  "scripts/test/workflow-script-resolution.test.mjs":
    "re-spaces a YAML list marker (`- `), nothing to do with slugs",
  "scripts/test/runtime-row-process-handle-naming.test.mjs":
    "builds the literal handle prefix `runtime-pane` from two constants",
  "scripts/test/slug-producers-agree.test.mjs":
    "this file names the shapes it scans for, which is unavoidable",
  "apps/chiefd/crates/chiefd-api/tests/conformance_tasks.rs":
    "spells a refusal code both ways for one assertion. Test evidence, not a producer",
  "packages/piing/extensions/organization-intercom.ts":
    "#1046: `departmentMatchKey` compares a department id the caller typed against the display " +
    "names already in the manifest, so a refusal can say `'Engineering' is the NAME of department " +
    "'engineering'`. It MINTS nothing — its result is never stored, never sent, and never becomes " +
    "an id; it exists only to pick which sentence to print. Every id in this file still comes from " +
    "chiefd, which mints them",
};

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

function isComment(line, path) {
  if (path.endsWith(".sh") || path.endsWith(".py")) return /^\s*#/.test(line);
  return /^\s*(\/\/|\*|\/\*)/.test(line);
}

/** Every line in the tree that collapses something into a literal hyphen. */
export function collapseSites(root = repoRoot) {
  const found = [];
  for (const absolute of sourceFiles(root)) {
    const path = relative(root, absolute).split(sep).join("/");
    const lines = readFileSync(absolute, "utf8").split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index];
      if (isComment(line, path)) continue;
      if (COLLAPSE_SHAPES.some((shape) => shape.test(line))) {
        found.push({ path, line: index + 1, text: line.trim() });
      }
    }
  }
  return found;
}

/** A Rust `const NAME: &[&str] = &[ ... ];`, as the list of its string literals. */
function rustStringSlice(source, name) {
  const start = source.indexOf(`const ${name}: &[&str] = &[`);
  assert.notEqual(start, -1, `${name} must be declared as a &[&str] slice`);
  const end = source.indexOf("];", start);
  assert.notEqual(end, -1, `${name} must be terminated`);
  return source
    .slice(start, end)
    .split("\n")
    .slice(1)
    .map((line) => line.trim())
    .filter((line) => line.endsWith(","))
    .map((line) => line.slice(0, -1));
}

/** A Rust `fn NAME(...) { ... }` body, whitespace-normalized. */
function rustFunctionBody(source, name) {
  const signature = source.indexOf(`fn ${name}(slug: &str) -> bool {`);
  assert.notEqual(signature, -1, `${name} must be declared in this file`);
  let depth = 0;
  let index = source.indexOf("{", signature);
  const open = index;
  do {
    if (source[index] === "{") depth += 1;
    if (source[index] === "}") depth -= 1;
    index += 1;
  } while (depth > 0 && index < source.length);
  assert.equal(depth, 0, `${name}'s body is unbalanced`);
  return source
    .slice(open + 1, index - 1)
    .split("\n")
    .map((line) => line.replace(/\/\/.*$/, "").trim())
    .filter((line) => line.length > 0)
    .join("\n");
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

test("every place in the tree that collapses into a hyphen is a NAMED producer with a stated keyspace", () => {
  const unnamed = collapseSites().filter(
    ({ path }) => !(path in PRODUCERS) && !(path in NOT_A_PRODUCER)
  );
  assert.deepEqual(
    unnamed.map(({ path, line, text }) => `${path}:${line}: ${text}`),
    [],
    "a new slug producer landed without being classified. Add it to PRODUCERS with its keyspace, " +
      "or to NOT_A_PRODUCER with the reason it is not one — the whole point of this guard is that " +
      "`placement.rs` once claimed there was exactly one producer and there were two"
  );
});

test("the enumeration is not vacuous — it still finds every producer it names", () => {
  // A scan root that stopped resolving returns an empty result that looks
  // exactly like verified-clean, so every named producer is asserted found
  // individually rather than by a total count.
  const byPath = new Map();
  for (const site of collapseSites()) byPath.set(site.path, site);
  for (const path of Object.keys(PRODUCERS)) {
    assert.ok(
      byPath.has(path),
      `${path} is named as a producer but the shape scan no longer finds it there — either the ` +
        "producer moved, or the scan has gone blind"
    );
  }
});

test("every company-slug producer carries the property test that runs it through the validator", () => {
  const missing = Object.entries(PRODUCERS)
    .filter(([, row]) => row.keyspace === "company")
    .filter(([path]) => !read(path).includes(`fn ${PROPERTY_TEST}()`))
    .map(([path]) => path);
  assert.deepEqual(
    missing,
    [],
    `every company-slug producer must carry \`${PROPERTY_TEST}\`. Deleting it removes the only ` +
      "assertion that this producer cannot emit the tmux session terminator"
  );
});

test("both company-slug producers are driven against the SAME adversarial corpus", () => {
  const genesis = rustStringSlice(read(GENESIS_RS), "SLUG_PRODUCER_CORPUS");
  const spec = rustStringSlice(read(ORGANIZATION_SPEC_RS), "SLUG_PRODUCER_CORPUS");
  assert.ok(genesis.length > 0, "the corpus must not be empty");
  assert.deepEqual(
    spec,
    genesis,
    "the two corpora have drifted. Two producers tested against two different inputs are two " +
      "producers nobody compared, which is the state this guard exists to end"
  );
});

test("the corpus contains the session terminator, the one character the whole proof turns on", () => {
  const corpus = rustStringSlice(read(GENESIS_RS), "SLUG_PRODUCER_CORPUS");
  const terminator = read("apps/chiefd/crates/chief-cli/src/placement.rs").match(
    /pub const SESSION_TERMINATOR: char = '(.)';/
  );
  assert.ok(terminator, "placement.rs must define SESSION_TERMINATOR as a char literal");
  assert.ok(
    corpus.some((entry) => entry.includes(terminator[1])),
    `no corpus entry contains '${terminator[1]}'. A corpus that never feeds a producer the ` +
      "terminator cannot prove the producer collapses it — which is exactly the hole the " +
      "pre-existing chief-cli corpus had"
  );
});

test("chiefd-core's copy of the validator is character-identical to chief-cli's original", () => {
  // The copy exists because the two crates may not link. It is only worth
  // having while it is the SAME rule; a copy that drifts is worse than no copy,
  // because it asserts a property the real validator does not have.
  assert.equal(
    rustFunctionBody(read(ORGANIZATION_SPEC_RS), "is_canonical_slug"),
    rustFunctionBody(read(PATHS_RS), "is_canonical_slug"),
    `${ORGANIZATION_SPEC_RS}'s test-module copy of \`is_canonical_slug\` no longer matches ` +
      `${PATHS_RS}'s original. Copy the original across — do not adjust the copy to fit`
  );
});

test("no comment in chief-cli claims a producer count", () => {
  // The exact regression: `placement.rs` said "`genesis::slugify` is the only
  // producer" and was correct for years about a fact that had already changed.
  const offenders = [];
  for (const file of ["placement.rs", "tmux.rs", "company.rs", "paths.rs", "genesis.rs"]) {
    const path = `apps/chiefd/crates/chief-cli/src/${file}`;
    const lines = read(path).split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      // "used to say ... the only producer" is the record of the defect and
      // must survive; a fresh CLAIM is what is forbidden.
      if (/\b(only|sole|single)\s+producer\b/.test(lines[index]) && !/used to|was false|not the whole set/.test(lines[index])) {
        offenders.push(`${path}:${index + 1}: ${lines[index].trim()}`);
      }
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "a comment claims there is one producer of a slug. There are two, in crates forbidden to " +
      "link each other, and the collision proof does not need the count — it needs the validator"
  );
});
