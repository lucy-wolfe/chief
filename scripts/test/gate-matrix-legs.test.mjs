// #941 rework: proves scripts/gate-matrix-legs.mjs actually DERIVES its run
// list from the tree — mutation-tested, not read-and-judged. The merger's
// own stated standard: "a file that greps package.json and a file that
// enumerates can look identical at a glance; only the mutation separates
// them." Every test here mutates a scratch fixture and re-derives; none
// asserts a remembered count.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

import { deriveAllGuards } from "../guard-count.mjs";
import {
  guardCountsLine,
  legCommand,
  NODE_TEST_REPORTER_ARGS,
  reconcileShellGates,
  stripAnsi,
} from "../gate-matrix-legs.mjs";

function withScratchRepo(fn) {
  const root = mkdtempSync(join(tmpdir(), "gate-matrix-legs-"));
  const guardTestDir = join(root, "scripts", "test");
  const workflowsDir = join(root, ".github", "workflows");
  mkdirSync(guardTestDir, { recursive: true });
  mkdirSync(workflowsDir, { recursive: true });
  writeFileSync(
    join(root, "package.json"),
    JSON.stringify({ scripts: { typecheck: "bash scripts/typecheck.sh" } })
  );
  const packageJsonPath = join(root, "package.json");
  try {
    return fn({ root, guardTestDir, workflowsDir, packageJsonPath });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function writeWorkflow(workflowsDir, name, runLines) {
  const body = ["jobs:", "  repo-guards:", "    steps:", ...runLines.map((l) => `      - run: ${l}`)].join("\n");
  writeFileSync(join(workflowsDir, name), body);
}

test("MUTATION: adding one test:* guard file moves the derived leg count by exactly one, zero edits to this file", () => {
  withScratchRepo(({ root, guardTestDir, workflowsDir, packageJsonPath }) => {
    writeFileSync(join(guardTestDir, "a.test.mjs"), "");
    writeWorkflow(workflowsDir, "ci.yml", Array.from({ length: 12 }, (_, i) => `echo filler-${i}`));

    const before = deriveAllGuards({ guardTestDir, workflowsDir, packageJsonPath });
    assert.equal(guardCountsLine(before), "GATE_MATRIX_GUARD_COUNTS:test.mjs=1,shell-gate=0,bun-test-suite=0,combined=1");

    writeFileSync(join(guardTestDir, "b.test.mjs"), "");
    const after = deriveAllGuards({ guardTestDir, workflowsDir, packageJsonPath });
    assert.equal(
      guardCountsLine(after),
      "GATE_MATRIX_GUARD_COUNTS:test.mjs=2,shell-gate=0,bun-test-suite=0,combined=2",
      "adding one guard file must move the leg count by exactly one, with no edit to gate-matrix-legs.mjs"
    );
  });
});

test("MUTATION: removing one test:* guard file moves the derived leg count down by exactly one", () => {
  withScratchRepo(({ root, guardTestDir, workflowsDir, packageJsonPath }) => {
    writeFileSync(join(guardTestDir, "a.test.mjs"), "");
    writeFileSync(join(guardTestDir, "b.test.mjs"), "");
    writeWorkflow(workflowsDir, "ci.yml", Array.from({ length: 12 }, (_, i) => `echo filler-${i}`));

    const before = deriveAllGuards({ guardTestDir, workflowsDir, packageJsonPath });
    assert.equal(before.filter((g) => g.category === "test.mjs").length, 2);

    rmSync(join(guardTestDir, "b.test.mjs"));
    const after = deriveAllGuards({ guardTestDir, workflowsDir, packageJsonPath });
    assert.equal(
      after.filter((g) => g.category === "test.mjs").length,
      1,
      "removing one guard file must move the leg count down by exactly one"
    );
  });
});

test("MUTATION: wiring a new shell script into a workflow's run: line moves the shell-gate count, combined total unaffected in category", () => {
  withScratchRepo(({ root, guardTestDir, workflowsDir, packageJsonPath }) => {
    mkdirSync(join(root, "scripts"), { recursive: true });
    writeFileSync(join(guardTestDir, "a.test.mjs"), "");
    writeWorkflow(workflowsDir, "ci.yml", [
      ...Array.from({ length: 11 }, (_, i) => `echo filler-${i}`),
      "bash scripts/newly-wired.sh",
    ]);

    const guards = deriveAllGuards({ guardTestDir, workflowsDir, packageJsonPath });
    const line = guardCountsLine(guards);
    assert.equal(line, "GATE_MATRIX_GUARD_COUNTS:test.mjs=1,shell-gate=1,bun-test-suite=0,combined=2");
    const shellEntry = guards.find((g) => g.category === "shell-gate");
    assert.equal(shellEntry.name, "scripts/newly-wired.sh");
  });
});

test("MUTATION (#977 arm): wiring a new `bun run test:*` step whose script is `bun test apps/cli/test/*.test.ts` moves the bun-test-suite count by exactly one", () => {
  withScratchRepo(({ root, guardTestDir, workflowsDir, packageJsonPath }) => {
    writeFileSync(join(guardTestDir, "a.test.mjs"), "");
    writeWorkflow(workflowsDir, "ci.yml", [
      ...Array.from({ length: 11 }, (_, i) => `echo filler-${i}`),
      "bun run test:newly-wired-suite",
    ]);
    writeFileSync(
      packageJsonPath,
      JSON.stringify({
        scripts: {
          typecheck: "bash scripts/typecheck.sh",
          "test:newly-wired-suite": "bun test apps/cli/test/NewlyWired.test.ts",
        },
      })
    );

    const before = deriveAllGuards({ guardTestDir, workflowsDir: join(root, ".github", "workflows-empty"), packageJsonPath });
    assert.equal(before.filter((g) => g.category === "bun-test-suite").length, 0, "no workflow, no bun-test-suite entries -- the arm's own zero baseline");

    const after = deriveAllGuards({ guardTestDir, workflowsDir, packageJsonPath });
    const suiteEntries = after.filter((g) => g.category === "bun-test-suite");
    assert.equal(suiteEntries.length, 1, "wiring one new bun:test suite step must move the count by exactly one, with no edit to guard-count.mjs");
    assert.equal(suiteEntries[0].name, "apps/cli/test/NewlyWired.test.ts");
    assert.equal(suiteEntries[0].scriptName, "test:newly-wired-suite");
  });
});

test("CONTROL (#977): the real tree's derivation finds exactly the known-uncovered apps/cli bun:test suites, no more, no fewer", () => {
  // The control that can fail the way the defect presents: if a suite is
  // added or removed from the real tree without this derivation noticing,
  // this is the assertion that catches it -- not a smoke test that only
  // proves the function runs.
  const guards = deriveAllGuards({
    guardTestDir: join(repoRoot, "scripts", "test"),
    workflowsDir: join(repoRoot, ".github", "workflows"),
    packageJsonPath: join(repoRoot, "package.json"),
  });
  const names = guards.filter((g) => g.category === "bun-test-suite").map((g) => g.name).sort();
  // #751/E4: nine of the ten suites this control used to name are DELETED,
  // together with the TypeScript modules they tested (the supervision,
  // session-lifecycle and task-authorization slices among them all
  // moved into chiefd) and with their own `bun test apps/cli/test/<Name>.test.ts`
  // package.json scripts. Their rows are removed rather than repointed:
  // Mandate 0 forbids pointing a stale row at a substitute, and a control
  // that names a file which cannot exist proves nothing. The control's real
  // job is unchanged and still discriminating — it is an EXACT set equality,
  // so a suite silently entering or leaving the derivation still fails here
  // by name.
  //
  // #751/P0: the LAST of them is now gone too. Deleting `apps/cli/src/legacy`
  // took `LauncherRunnerStdin.test.ts` with the module it imported, so the
  // honest set is EMPTY. That is not a weakened control: exact set equality
  // against `[]` still fails the moment any suite enters the derivation, which
  // is precisely what this exists to catch. `guard-count` separately refuses
  // an empty derivation whenever root package.json still wires a
  // `bun test <file>.test.ts` script, so empty-because-nothing-is-wired and
  // empty-because-the-scan-broke stay distinguishable.
  assert.deepEqual(names, []);
});

test("legCommand builds `node --test <path>` for a test.mjs guard, resolved under the given root", () => {
  const { cmd, args } = legCommand({ category: "test.mjs", name: "foo.test.mjs" }, "/repo");
  assert.equal(cmd, "node");
  // THE REPORTER IS SPREAD FROM THE CONSTANT, not restated here. A literal
  // `"--test-reporter=tap"` in this assertion would be a second declaration of
  // the format, and the whole point of the constant is that the format asked
  // for and the format parsed are one fact.
  assert.deepEqual(args, [
    "--test",
    ...NODE_TEST_REPORTER_ARGS,
    join("/repo", "scripts", "test", "foo.test.mjs"),
  ]);
  // And it is really there — a constant that became empty would make the
  // assertion above pass while the reporter went back to being inherited.
  assert.ok(NODE_TEST_REPORTER_ARGS.length > 0, "the reporter must actually be asked for");
});

test("legCommand builds `bash <path>` for a shell-gate guard, using the derived file path directly", () => {
  const { cmd, args } = legCommand({ category: "shell-gate", name: "scripts/typecheck.sh" }, "/repo");
  assert.equal(cmd, "bash");
  assert.deepEqual(args, [join("/repo", "scripts/typecheck.sh")]);
});

test("stripAnsi removes color codes sitting between a label and the digits that follow it", () => {
  // The exact failure mode reported live: a per-package grep for `Tests`
  // dropped seven lines whole because ANSI codes sat between the word and
  // its count, even though the text read correctly in a color terminal.
  const raw = "Tests[32m 7 passed[0m, 0 failed";
  assert.equal(stripAnsi(raw), "Tests 7 passed, 0 failed");
});

test("stripAnsi is a no-op on plain text", () => {
  assert.equal(stripAnsi("plain text, no codes"), "plain text, no codes");
});

test("against the REAL tree: deriveAllGuards returns a non-empty, correctly-shaped corpus this file would actually run", () => {
  const guards = deriveAllGuards();
  assert.ok(guards.length > 20, `expected 20+ real guards, got ${guards.length}`);
  const testMjs = guards.filter((g) => g.category === "test.mjs");
  const shellGate = guards.filter((g) => g.category === "shell-gate");
  assert.ok(testMjs.length > 15);
  assert.ok(shellGate.length >= 3, "the real tree is known to wire at least 3 shell gates");
  assert.ok(shellGate.some((g) => g.name === "scripts/cargo-check-macos.sh"), "the darwin cross-check must be part of the derived corpus");
  assert.ok(shellGate.some((g) => g.name === "scripts/typecheck.sh"));
  assert.ok(shellGate.some((g) => g.name === "scripts/cargo-test-workspace.sh"));
});

// ---- #941 follow-up (merger): the CATEGORICAL shell-gate split ----
// The corpus runs [test.mjs] only; gate-matrix.sh runs the three CI-wired
// shell gates as explicit, order-sensitive stages. The two sides are
// reconciled by SET EQUALITY rather than by an exclusion list, because an
// exclusion list rots: add a fourth shell gate to ci.yml and it would be
// skipped by BOTH sides with nothing saying so.

test("reconcileShellGates: equal sets reconcile, order-independently", () => {
  const r = reconcileShellGates(["scripts/a.sh", "scripts/b.sh"], ["scripts/b.sh", "scripts/a.sh"]);
  assert.equal(r.ok, true);
  assert.deepEqual(r.missingFromMatrix, []);
  assert.deepEqual(r.notDerived, []);
});

test("reconcileShellGates: a NEW CI-wired shell gate nothing runs is REFUSED and NAMED — the exclusion-list rot this replaces", () => {
  const r = reconcileShellGates(
    ["scripts/cargo-test-workspace.sh", "scripts/typecheck.sh", "scripts/cargo-check-macos.sh", "scripts/brand-new.sh"],
    ["scripts/cargo-test-workspace.sh", "scripts/typecheck.sh", "scripts/cargo-check-macos.sh"]
  );
  assert.equal(r.ok, false);
  assert.deepEqual(r.missingFromMatrix, ["scripts/brand-new.sh"]);
});

test("reconcileShellGates: a matrix stage naming a shell gate the derivation does not know is REFUSED and NAMED", () => {
  const r = reconcileShellGates(["scripts/typecheck.sh"], ["scripts/typecheck.sh", "scripts/invented.sh"]);
  assert.equal(r.ok, false);
  assert.deepEqual(r.notDerived, ["scripts/invented.sh"]);
});

test("REAL REPO: the derived [shell-gate] set equals exactly what gate-matrix.sh runs explicitly", () => {
  const guards = deriveAllGuards({
    guardTestDir: join(repoRoot, "scripts", "test"),
    workflowsDir: join(repoRoot, ".github", "workflows"),
    packageJsonPath: join(repoRoot, "package.json"),
  });
  const derived = guards.filter((g) => g.category === "shell-gate").map((g) => g.name);
  const matrix = readFileSync(join(repoRoot, "scripts", "gate-matrix.sh"), "utf8");
  const explicit = [...matrix.matchAll(/--explicit-shell-gate\s+([^\s;\\]+)/g)].map((m) => m[1]);
  const r = reconcileShellGates(derived, explicit);
  assert.equal(
    r.ok,
    true,
    `derived-but-unrun: ${JSON.stringify(r.missingFromMatrix)}; run-but-underived: ${JSON.stringify(r.notDerived)}`
  );
  assert.ok(derived.length > 0, "vacuous: no shell gates derived at all");
});
