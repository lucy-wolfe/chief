// #944: real coverage for scripts/turbo-cache-correctness.mjs -- runs real
// `turbo run --dry=json` against THIS repo's real `turbo.json` and real
// `test:unit` task. Deliberately not a synthetic-fixture test: a fixture
// with its own toy config would prove turbo's own semantics work, which
// was never in doubt, and would have stayed green through #945's entire
// lifetime (`CI` silently missing from every bucket in the REAL config).
// This checks the configuration the fleet actually ships.

import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

import { checkTaskEnvHashing, checkVariable, dryRunHash, effectiveTaskDef, readTurboJson, REPO_ROOT } from "../turbo-cache-correctness.mjs";

const TURBO_BIN = join(REPO_ROOT, "node_modules", ".bin", "turbo");

function requireTurbo() {
  if (!existsSync(TURBO_BIN)) {
    throw new Error(`turbo binary not found at ${TURBO_BIN} -- run 'bun install' first`);
  }
}

// ---- 1. effectiveTaskDef: replace-not-merge, same rule #939's audit needed ----

test("effectiveTaskDef prefers a package-specific override over the generic task, never merging the two", () => {
  const turboConfig = {
    tasks: {
      "test:unit": { env: ["GENERIC_ONLY"] },
      "@chief/special#test:unit": { env: ["SPECIAL_ONLY"] }
    }
  };
  assert.deepEqual(effectiveTaskDef(turboConfig, "@chief/special", "test:unit"), { env: ["SPECIAL_ONLY"] });
  assert.deepEqual(effectiveTaskDef(turboConfig, "@chief/ordinary", "test:unit"), { env: ["GENERIC_ONLY"] });
});

// ---- 2. dryRunHash: real turbo, real repo, no execution -------------------

test("REAL TURBO: --dry=json produces a stable hash for an unchanged invocation", { timeout: 30_000 }, () => {
  requireTurbo();
  const h1 = dryRunHash({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", env: { CARGO_TARGET_DIR: "/tmp/944-stable-probe" } });
  const h2 = dryRunHash({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", env: { CARGO_TARGET_DIR: "/tmp/944-stable-probe" } });
  assert.equal(h1, h2, "two dry-runs with an identical env must produce the identical hash");
});

test("REAL TURBO: --dry=json hash changes when a declared 'env' var's value changes", { timeout: 30_000 }, () => {
  requireTurbo();
  const h1 = dryRunHash({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", env: { CARGO_TARGET_DIR: "/tmp/944-a" } });
  const h2 = dryRunHash({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", env: { CARGO_TARGET_DIR: "/tmp/944-b" } });
  assert.notEqual(h1, h2, "CARGO_TARGET_DIR is declared in env; a changed value must change the hash");
});

// ---- 3. checkVariable: the per-variable check, both buckets, real turbo --

test("REAL TURBO: checkVariable PASSES for CARGO_TARGET_DIR (env) -- correctly hashed", { timeout: 30_000 }, () => {
  requireTurbo();
  const result = checkVariable({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", varName: "CARGO_TARGET_DIR", bucket: "env" });
  assert.equal(result.conclusive, true, JSON.stringify(result));
  assert.equal(result.pass, true, JSON.stringify(result.problems));
});

test("REAL TURBO: checkVariable PASSES for TMPDIR (passThroughEnv) -- correctly NOT hashed", { timeout: 30_000 }, () => {
  requireTurbo();
  const result = checkVariable({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", varName: "TMPDIR", bucket: "passThroughEnv" });
  assert.equal(result.conclusive, true, JSON.stringify(result));
  assert.equal(result.pass, true, JSON.stringify(result.problems));
});

// ---- 4. THE DEMONSTRATED RED: prove this leg can actually fail -----------
//
// #944's own instruction, still true under the new design even though the
// harness shape changed: "do not weaken it into a smoke test... prove it
// can fail by making it fail." Asserting a var declared in `env` behaves
// like `passThroughEnv` (and vice versa) reproduces exactly the two wrong
// directions #939/#945 actually shipped, without editing turbo.json.

test("THE DEMONSTRATED RED: asserting an env-bucket var like a passThroughEnv-bucket var fails, naming the reason", { timeout: 30_000 }, () => {
  requireTurbo();
  // CARGO_TARGET_DIR really is in `env` (hashed) -- asking checkVariable to
  // verify it as if it were `passThroughEnv` (unhashed) must fail, because
  // the real hash DOES change.
  const result = checkVariable({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", varName: "CARGO_TARGET_DIR", bucket: "passThroughEnv" });
  assert.equal(result.conclusive, true, JSON.stringify(result));
  assert.equal(result.pass, false);
  assert.ok(result.problems.some((p) => p.includes("changed the hash")));
});

test("THE DEMONSTRATED RED, the #945 shape exactly: asserting TMPDIR as 'env' fails, naming the reason", { timeout: 30_000 }, () => {
  requireTurbo();
  // TMPDIR really is in `passThroughEnv` (unhashed) -- asking checkVariable
  // to verify it as if it were `env` (must-differ) must fail, because the
  // real hash does NOT change. This is the exact shape #945's `CI` bug had:
  // a var whose declared bucket claims hashing that isn't actually happening.
  const result = checkVariable({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing", varName: "TMPDIR", bucket: "env" });
  assert.equal(result.conclusive, true, JSON.stringify(result));
  assert.equal(result.pass, false);
  assert.ok(result.problems.some((p) => p.includes("did NOT change")));
});

// ---- 5. vacuity: a task with neither bucket populated refuses -------------

test("checkTaskEnvHashing refuses when the effective task declares nothing in either bucket", () => {
  assert.throws(
    () => checkTaskEnvHashing({ turboBin: TURBO_BIN, task: "build", pkg: "@chief/testing", turboConfig: { tasks: { build: {} } } }),
    /nothing to check/
  );
});

// ---- 6. REAL REPO: every declared var, both buckets, checked at once -----
//
// This is the actual guard: not a spot check on two or three vars picked
// by hand, but every variable turbo.json currently declares for the real
// test:unit task, checked against its own claimed bucket. A future var
// landing in the wrong bucket -- #945's exact shape, for ANY variable, not
// just CI -- is caught by name here without anyone having to think to test
// that specific one.

test("REAL REPO: every env/passThroughEnv variable declared for @chief/testing's test:unit task is correctly bucketed", { timeout: 120_000 }, () => {
  requireTurbo();
  const results = checkTaskEnvHashing({ turboBin: TURBO_BIN, task: "test:unit", pkg: "@chief/testing" });
  assert.ok(results.length > 0, "checkTaskEnvHashing returned zero variables to check -- vacuity");
  const failures = results.filter((r) => r.conclusive && !r.pass);
  const inconclusive = results.filter((r) => !r.conclusive);
  assert.deepEqual(
    inconclusive,
    [],
    `${inconclusive.length} variable(s) produced an inconclusive (unstable-hash) result: ${JSON.stringify(inconclusive)}`
  );
  assert.deepEqual(
    failures.map((f) => ({ varName: f.varName, bucket: f.bucket, problems: f.problems })),
    [],
    `${failures.length} variable(s) are in the wrong bucket`
  );
});
