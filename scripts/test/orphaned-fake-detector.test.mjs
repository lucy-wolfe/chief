// #952: proves the detector reproduces the exact #947 shape (a test-seam
// setter whose paired accessor production code no longer calls) as a
// demonstrated RED, and does not false-positive on a live seam whose
// accessor production still calls.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { detectOrphanedFakes, discoverSeamSetters } from "../orphaned-fake-detector.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "orphaned-fake-detector-"));
  mkdirSync(join(root, "src"), { recursive: true });
  mkdirSync(join(root, "test"), { recursive: true });

  // The exact #947 shape: a seam module exporting a real accessor
  // (`durableStore`) alongside its test setter (`setDurableStoreForTests`).
  writeFileSync(
    join(root, "src", "durable-store.ts"),
    [
      "let store;",
      "export function durableStore() { return store; }",
      "export function setDurableStoreForTests(next) { store = next; }",
    ].join("\n"),
  );
  // Production code has MOVED OFF durableStore() entirely -- it never
  // imports it. This is the orphaning: the accessor exists but nothing in
  // production reads it any more.
  writeFileSync(
    join(root, "src", "org-store.ts"),
    "export function createOrganization() { return 'direct-client-path'; }\n",
  );
  // A test file still injects the fake -- this is the "green that observes
  // nothing" consumer the detector must name.
  writeFileSync(
    join(root, "test", "OrgStore.test.ts"),
    "import { setDurableStoreForTests } from '../src/durable-store'\nsetDurableStoreForTests({})\n",
  );

  // A SECOND, LIVE seam in the same fixture: production still calls its
  // accessor, so this one must classify load-bearing, never orphaned.
  writeFileSync(
    join(root, "src", "feature-flags.ts"),
    [
      "let flag = false;",
      "export function featureEnabled() { return flag; }",
      "export function setFeatureEnabledForTests(next) { flag = next; }",
    ].join("\n"),
  );
  writeFileSync(
    join(root, "src", "consumer.ts"),
    "import { featureEnabled } from './feature-flags'\nexport function run() { return featureEnabled(); }\n",
  );
  writeFileSync(
    join(root, "test", "FeatureFlags.test.ts"),
    "import { setFeatureEnabledForTests } from '../src/feature-flags'\nsetFeatureEnabledForTests(true)\n",
  );

  return root;
}

test("discovers every set<X>ForTests export repo-wide with its sibling real exports", () => {
  const root = fixture();
  try {
    const setters = discoverSeamSetters(root);
    const names = setters.map((s) => s.name).sort();
    assert.deepEqual(names, ["setDurableStoreForTests", "setFeatureEnabledForTests"]);
    const durable = setters.find((s) => s.name === "setDurableStoreForTests");
    assert.deepEqual(durable.otherExports, ["durableStore"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("RED (the exact #947 shape): a seam whose accessor has zero production importers, still injected by a test, is ORPHANED and the misled test file is named", () => {
  const root = fixture();
  try {
    const results = detectOrphanedFakes(root);
    const durable = results.find((r) => r.setter === "setDurableStoreForTests");
    assert.equal(durable.status, "orphaned");
    assert.deepEqual(durable.productionImportersOfAccessors, []);
    assert.deepEqual(durable.testFilesConsumingSetter, ["test/OrgStore.test.ts"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a seam whose accessor production code still calls is load-bearing, never a false-positive orphan", () => {
  const root = fixture();
  try {
    const results = detectOrphanedFakes(root);
    const flag = results.find((r) => r.setter === "setFeatureEnabledForTests");
    assert.equal(flag.status, "load-bearing");
    assert.deepEqual(flag.productionImportersOfAccessors, ["src/consumer.ts"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a seam setter with no test consumer and no production importer is 'unused', never conflated with 'orphaned' (no test is being misled)", () => {
  const root = mkdtempSync(join(tmpdir(), "orphaned-fake-detector-unused-"));
  try {
    mkdirSync(join(root, "src"), { recursive: true });
    writeFileSync(
      join(root, "src", "dead-seam.ts"),
      ["let v;", "export function readValue() { return v; }", "export function setValueForTests(next) { v = next; }"].join("\n"),
    );
    const results = detectOrphanedFakes(root);
    const dead = results.find((r) => r.setter === "setValueForTests");
    assert.equal(dead.status, "unused");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a string that merely looks like an import is never treated as a real reference (AST parsing, never regex)", () => {
  const root = fixture();
  try {
    writeFileSync(
      join(root, "test", "Decoy.test.ts"),
      "const description = \"setDurableStoreForTests from '../src/durable-store'\"\nconsole.log(description)\n",
    );
    const results = detectOrphanedFakes(root);
    const durable = results.find((r) => r.setter === "setDurableStoreForTests");
    assert.ok(!durable.testFilesConsumingSetter.includes("test/Decoy.test.ts"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("REAL REPO sanity: running against the actual tree does not crash and returns the expected shape", () => {
  // Shape only, deliberately — this asserted `results.length >= 1` as a
  // non-vacuity floor, back when `set<X>ForTests` seams existed. Every one of
  // them lived in `apps/cli/src/legacy/organization/`, which #751/E4 ported
  // into chiefd; the two `*ForTests` exports left in the tree are `reset…`,
  // which this detector's `^set[A-Za-z0-9]*ForTests$` pattern deliberately
  // does not match. A repo with no seams has no orphaned fakes, which is the
  // right answer, not a broken run — and a floor the tree cannot clear makes
  // the gate REFUSE TO RUN, so the detector would stop being exercised against
  // the real tree at all.
  //
  // Non-vacuity is not lost, it moves to where it belongs: the five tests
  // above CONSTRUCT seams and assert this detector finds and classifies them.
  // That proves the detector works whether or not this tree happens to contain
  // one, which is exactly the property a floor keyed on the tree's contents
  // could never give.
  //
  // (This surfaced only after 247MB of stale nested checkouts were cleaned off
  // the build host: the detector was walking `.claude/worktrees/**` and finding
  // seams in DELETED code, so the guard's answer depended on whether the
  // machine happened to have agent worktrees lying around.)
  const repoRoot = new URL("../..", import.meta.url).pathname;
  const results = detectOrphanedFakes(repoRoot);
  assert.ok(Array.isArray(results));
  for (const r of results) {
    assert.ok(["load-bearing", "orphaned", "unused"].includes(r.status));
  }
});
