// #939: unit coverage for scripts/turbo-env-audit.mjs against controlled
// fixture trees -- proves the derivation and its refusals actually fire,
// not just that the real tree happens to be clean today.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  deriveEnvReadsForPackage,
  discoverPackages,
  effectiveDeclaredEnv,
  findUndeclaredTestUnitEnv,
} from "../turbo-env-audit.mjs";

function withFixtureRoot(fn) {
  const root = mkdtempSync(join(tmpdir(), "turbo-env-audit-test-"));
  try {
    return fn(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function writeFile(path, contents) {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, contents);
}

function writePackage(root, group, name, srcFiles = {}) {
  const dir = join(root, group, name.replace("@chief/", ""));
  mkdirSync(dir, { recursive: true });
  writeFile(join(dir, "package.json"), JSON.stringify({ name }));
  for (const [relativePath, contents] of Object.entries(srcFiles)) {
    writeFile(join(dir, "src", relativePath), contents);
  }
  return dir;
}

// ---- 1. discoverPackages: real disk, not the workspaces glob -------------

test("discoverPackages finds every apps/*/package.json and packages/*/package.json with a name", () => {
  withFixtureRoot((root) => {
    writePackage(root, "apps", "@chief/one");
    writePackage(root, "packages", "@chief/two");
    mkdirSync(join(root, "apps", "no-package-json"), { recursive: true });
    const found = discoverPackages(root).map((p) => p.name).sort();
    assert.deepEqual(found, ["@chief/one", "@chief/two"]);
  });
});

// ---- 2. deriveEnvReadsForPackage: literal reads, src-only -----------------

test("deriveEnvReadsForPackage finds a literal process.env.X read under src/ and ignores test/", () => {
  withFixtureRoot((root) => {
    const dir = writePackage(root, "packages", "@chief/fixture", {
      "thing.ts": 'export const x = process.env.MY_FIXTURE_VAR\n',
    });
    writeFile(join(dir, "test", "thing.test.ts"), 'process.env.MY_TEST_ONLY_VAR = "x"\n');
    const { reads, unresolvedDynamicSites } = deriveEnvReadsForPackage(dir, root);
    assert.ok(reads.has("MY_FIXTURE_VAR"));
    assert.ok(!reads.has("MY_TEST_ONLY_VAR"), "test/ must never contribute reads");
    assert.deepEqual(unresolvedDynamicSites, []);
  });
});

// ---- 2b. #943: the LOCAL alias class -- a parameter default or a same-
//          scope `??`/`||` fallback bound to `process.env` -------------

test("THE #943 SHAPE: a parameter defaulted to process.env credits every .PROP read off it within the same function, matching the real chiefdLauncherRoot/ORG_LAUNCHER_ROOT incident", () => {
  withFixtureRoot((root) => {
    const dir = writePackage(root, "packages", "@chief/fixture", {
      "thing.ts": [
        "export function launcherRoot(environment: Record<string, string | undefined> = process.env): string {",
        "  return environment.ORG_LAUNCHER_ROOT ?? ''",
        "}",
      ].join("\n"),
    });
    const { reads } = deriveEnvReadsForPackage(dir, root);
    assert.ok(reads.has("ORG_LAUNCHER_ROOT"));
  });
});

test("THE #943 SHAPE: a local const falling back to process.env (options.environment ?? process.env) credits reads off it the same way", () => {
  withFixtureRoot((root) => {
    const dir = writePackage(root, "packages", "@chief/fixture", {
      "thing.ts": [
        "export function run(options: { environment?: Record<string, string | undefined> } = {}) {",
        "  const environment = options.environment ?? process.env",
        "  return environment.MY_FALLBACK_VAR",
        "}",
      ].join("\n"),
    });
    const { reads } = deriveEnvReadsForPackage(dir, root);
    assert.ok(reads.has("MY_FALLBACK_VAR"));
  });
});

test("THE #943 SHAPE, negative control: a same-named parameter with NO process.env default is never credited -- this is scope-bound, not name-bound", () => {
  withFixtureRoot((root) => {
    const dir = writePackage(root, "packages", "@chief/fixture", {
      "thing.ts": [
        "export function run(environment: Record<string, string> = { fixed: 'value' }) {",
        "  return environment.NOT_A_REAL_ENV_VAR",
        "}",
      ].join("\n"),
    });
    const { reads } = deriveEnvReadsForPackage(dir, root);
    assert.ok(!reads.has("NOT_A_REAL_ENV_VAR"), "a parameter not defaulted to process.env must never be treated as an alias");
  });
});

test("THE #943 SHAPE, scope discipline: an unrelated function's own `environment` parameter (no process.env default) is not credited just because another function in the same file has one", () => {
  withFixtureRoot((root) => {
    const dir = writePackage(root, "packages", "@chief/fixture", {
      "thing.ts": [
        "export function withAlias(environment: Record<string, string | undefined> = process.env) {",
        "  return environment.REAL_ALIAS_VAR",
        "}",
        "export function withoutAlias(environment: { local: string }) {",
        "  return environment.NOT_AN_ENV_VAR",
        "}",
      ].join("\n"),
    });
    const { reads } = deriveEnvReadsForPackage(dir, root);
    assert.ok(reads.has("REAL_ALIAS_VAR"));
    assert.ok(!reads.has("NOT_AN_ENV_VAR"));
  });
});

// ---- 3. dynamic reads: unresolved is a REFUSAL, not a zero ----------------

test("deriveEnvReadsForPackage reports an UNRESOLVED dynamic read as a site, not silently as zero vars", () => {
  withFixtureRoot((root) => {
    const dir = writePackage(root, "packages", "@chief/fixture", {
      "thing.ts": 'const key = pick()\nexport const x = process.env[key]\n',
    });
    const { reads, unresolvedDynamicSites } = deriveEnvReadsForPackage(dir, root);
    assert.equal(reads.size, 0, "an unresolved dynamic read must not be silently credited as zero real reads");
    assert.equal(unresolvedDynamicSites.length, 1);
    assert.match(unresolvedDynamicSites[0], /thing\.ts:2$/);
  });
});

// ---- 4. effectiveDeclaredEnv: package-specific override replaces, not merges ----

test("a package-specific <pkg>#test:unit task REPLACES the generic test:unit env, never merges with it", () => {
  const turboConfig = {
    tasks: {
      "test:unit": { env: ["GENERIC_ONLY"] },
      "@chief/special#test:unit": { env: ["SPECIAL_ONLY"] },
    },
  };
  const special = effectiveDeclaredEnv(turboConfig, "@chief/special");
  assert.ok(special.usedScopedOverride);
  assert.ok(special.declared.has("SPECIAL_ONLY"));
  assert.ok(!special.declared.has("GENERIC_ONLY"), "the override must not inherit the generic task's env");

  const generic = effectiveDeclaredEnv(turboConfig, "@chief/ordinary");
  assert.ok(!generic.usedScopedOverride);
  assert.ok(generic.declared.has("GENERIC_ONLY"));
});

// ---- 5. THE demonstrated red: an undeclared real read fails the check ----

test("findUndeclaredTestUnitEnv FAILS a package whose src/ reads a var absent from its effective task env", () => {
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/leaky", {
      "thing.ts": "export const x = process.env.UNDECLARED_REAL_VAR\n",
    });
    const turboConfig = { tasks: { "test:unit": { env: ["DEBUG_891"] } } };
    const { problems } = findUndeclaredTestUnitEnv(turboConfig, root);
    assert.equal(problems.length, 1);
    assert.equal(problems[0].package, "@chief/leaky");
    assert.deepEqual(problems[0].undeclared, ["UNDECLARED_REAL_VAR"]);
  });
});

test("findUndeclaredTestUnitEnv PASSES clean once the var is declared for the effective task", () => {
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/leaky", {
      "thing.ts": "export const x = process.env.UNDECLARED_REAL_VAR\n",
    });
    const turboConfig = { tasks: { "test:unit": { env: ["DEBUG_891", "UNDECLARED_REAL_VAR"] } } };
    const { problems } = findUndeclaredTestUnitEnv(turboConfig, root);
    assert.deepEqual(problems, []);
  });
});

// ---- 6. THE class #939 was actually filed for: a scoped override missing what the generic task has ----

test("THE #939 SHAPE: a package-specific override that forgot to redeclare a var the generic task has is still caught", () => {
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/chiefing-like", {
      "thing.ts": "export const x = process.env.CARGO_TARGET_DIR\n",
    });
    const turboConfig = {
      tasks: {
        "test:unit": { env: ["CARGO_TARGET_DIR"] },
        // The override exists (e.g. for a different dependsOn) but forgot env entirely.
        "@chief/chiefing-like#test:unit": { dependsOn: ["build"] },
      },
    };
    const { problems } = findUndeclaredTestUnitEnv(turboConfig, root);
    assert.equal(problems.length, 1);
    assert.equal(problems[0].package, "@chief/chiefing-like");
    assert.ok(problems[0].usedScopedOverride);
    assert.deepEqual(problems[0].undeclared, ["CARGO_TARGET_DIR"]);
  });
});

// ---- 7. THE #945 SHAPE: no exemption set survives -- CI (and everything else) must be declared ----
//
// #945: this scanner used to hold a DEFAULT_PASSTHROUGH_ENV set of
// {CI, HOME, PATH, LANG}, exempted from declaration because a probe test
// showed the value still visible in the child process with nothing
// declared. That probe proved VISIBILITY, not that turbo's cache key
// HASHES the value -- and for all four, it does not (confirmed empirically:
// CI unset vs CI=1 against an unchanged tree produced the identical cache
// entry, replaying a run where `chiefdBinaryTestGate` had skipped rather
// than thrown). There is no exemption set anymore; every real read must be
// declared, full stop. This test proves the shape that shipped in #939
// cannot recur: a package reading CI (or anything) with nothing declared
// for it must be flagged, not silently passed.
test("THE #945 SHAPE: CI (and any other real read) is flagged when undeclared -- no exemption set exists", () => {
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/quiet", {
      "thing.ts": "export const inCI = !!process.env.CI\n",
    });
    const turboConfig = { tasks: { "test:unit": { env: [] } } };
    const { problems } = findUndeclaredTestUnitEnv(turboConfig, root);
    assert.equal(problems.length, 1);
    assert.deepEqual(problems[0].undeclared, ["CI"]);
  });
});

test("THE #945 SHAPE, green side: CI passes clean once declared like any other real read", () => {
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/quiet", {
      "thing.ts": "export const inCI = !!process.env.CI\n",
    });
    const turboConfig = { tasks: { "test:unit": { env: ["CI"] } } };
    const { problems } = findUndeclaredTestUnitEnv(turboConfig, root);
    assert.deepEqual(problems, []);
  });
});

// ---- 9. vacuity: zero packages found refuses rather than reporting clean --

test("findUndeclaredTestUnitEnv refuses when discoverPackages resolves zero workspace members", () => {
  withFixtureRoot((root) => {
    assert.throws(() => findUndeclaredTestUnitEnv({ tasks: {} }, root), /zero workspace members/);
  });
});

// ---- 10. KNOWN_DYNAMIC_READS staleness: a resolved site that moved is caught too ----

test("a KNOWN_DYNAMIC_READS-style resolution that no longer matches a real dynamic read is reported stale, not silently trusted", () => {
  // Driven from a SYNTHETIC record, not the live one.
  //
  // This proof used to run the module's real `KNOWN_DYNAMIC_READS` against an
  // unrelated fixture tree and assert every entry came back stale. That worked
  // only while the record happened to be non-empty: #983 removed its last
  // entry, and the proof silently became "zero entries, zero stale, nothing
  // asserted" — vacuous at the exact moment the record had nothing left to
  // catch. The subject here is the staleness WALK, not the current contents of
  // a record that is supposed to shrink to nothing, so the walk is fed a
  // record of its own.
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/unrelated", {
      "thing.ts": "export const x = 1\n",
    });
    const turboConfig = { tasks: { "test:unit": { env: [] } } };
    const synthetic = {
      "packages/gone/src/Moved.ts:12": ["SOME_VAR"],
      "packages/gone/src/AlsoMoved.ts:44": [],
    };
    const { staleKnownSites } = findUndeclaredTestUnitEnv(turboConfig, root, synthetic);
    assert.deepEqual(
      staleKnownSites,
      ["packages/gone/src/AlsoMoved.ts:44", "packages/gone/src/Moved.ts:12"],
      "every resolution whose file:line holds no dynamic read must be named stale"
    );
  });
});

test("an EMPTY resolution record is clean, not stale — the shrink direction must terminate", () => {
  // The complement of the test above, and the reason it had to stop reading
  // the live record: a record that has correctly shrunk to nothing must report
  // clean rather than failing, or the audit would punish the very outcome its
  // staleness check exists to force.
  withFixtureRoot((root) => {
    writePackage(root, "packages", "@chief/unrelated", {
      "thing.ts": "export const x = 1\n",
    });
    const turboConfig = { tasks: { "test:unit": { env: [] } } };
    const { staleKnownSites } = findUndeclaredTestUnitEnv(turboConfig, root, {});
    assert.deepEqual(staleKnownSites, []);
  });
});

// ---- 11. real repo: the actual tree and actual turbo.json pass clean today ----

test("REAL REPO: the actual tree's turbo.json declares every process.env read (literal and resolved-dynamic) turbo would otherwise strip", () => {
  const { problems, unresolvedDynamicSites, staleKnownSites } = findUndeclaredTestUnitEnv();
  assert.deepEqual(problems, [], JSON.stringify(problems));
  assert.deepEqual(unresolvedDynamicSites, [], "a real dynamic read appeared with no resolution recorded");
  assert.deepEqual(staleKnownSites, [], "a recorded resolution no longer matches the code it was resolved against");
});
