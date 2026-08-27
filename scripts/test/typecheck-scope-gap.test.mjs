// The guard for "typechecked by nothing, tested by something".
//
// `scripts/coverage-scope-gap.mjs` (#970) reports files outside BOTH scopes,
// so a file inside the test scope can never appear in its answer. That left
// the repo with no instrument at all for a file some suite RUNS that no type
// checker ever READS -- and the tree held a live instance the whole time:
// `apps/web/tsconfig.json`'s `"exclude": ["node_modules", "test"]` hid all
// 61 web test files plus their harnesses from `bun run typecheck`, the gate
// whose entire job is finding type errors. Vitest was the only thing that
// ever compiled them, and it strips types instead of checking them. Removing
// the exclusion surfaced 66 real errors. Every gate was green throughout.
//
// SEEN TO FAIL, NOT ASSUMED TO WORK
// ---------------------------------
// This repo has produced six instruments that could not see their subject,
// and every one of them was green on arrival. So this file does not merely
// assert the live gap is empty -- an empty gap and a derivation that returns
// nothing at all are the same green. Three arms make the difference visible:
//
//   1. NON-VACUITY on both scopes: a floor on each derived set's size, so a
//      typecheck scope that has collapsed to nothing cannot report a clean
//      gap. Low and wide on purpose (see the floor's own comment).
//   2. DETECTION, both directions, against a controlled fixture tree: the
//      exact defect shape (a workspace member whose tsconfig excludes its own
//      `test` directory) is built on disk, and the real derivation must name
//      the file; the exclusion is then removed and the same derivation must
//      stop naming it. Neither a derivation that returns everything nor one
//      that returns nothing can pass both halves.
//   3. A REAL-REPO CONTROL: `apps/web/test/harness/FakeChiefApi.ts` -- the
//      exact tree the incident hid -- must be present in BOTH derived scopes
//      right now. If the exclusion is ever restored, arm 3 still passes (the
//      file stays in the test scope) while the main assertion fails by name,
//      which is the split that makes the failure readable.
//
// The manual proof was also run once, by hand, before this guard landed:
// re-adding `"test"` to `apps/web/tsconfig.json`'s `exclude` turned the main
// assertion below red, naming 45 files; removing it turned it green again.

import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { deriveAllJsFiles, deriveTestCoveredFiles, deriveTypecheckedFiles } from "../coverage-scope-gap.mjs";
import { deriveScopeSizes, deriveTypecheckScopeGap } from "../typecheck-scope-gap.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(__dirname, "..", "..");

// The reviewed baseline: files a suite's scope covers that no `tsc` leg
// reads, each with a stated reason for staying that way. This is NOT an
// enumeration of either scope -- both scopes are derived from the real
// configs -- it is a register of reviewed EXCEPTIONS to the derived result,
// the same shape `coverage-scope-gap.test.mjs`'s own KNOWN_GAP uses. The
// stale-entry check below makes it track reality in both directions: a row
// that stops appearing in the live derivation fails just as loudly as a new
// file that starts appearing.
const KNOWN_UNTYPECHECKED = new Map([
  [
    "packages/eslinter/test/project-service-warmup/warmup.ts",
    // The reason is a STRING, not a comment: a registry whose rows carry
    // their justification as data can print it in the failure message that
    // reviews it, and `new Map([[key]])` -- a row with a key and no value at
    // all, which is what this was -- is not a shape any type checker accepts.
    // #1041 found that the moment JavaScript entered a typecheck leg.
    "Not code any suite executes -- it is INPUT to one. `packages/eslinter` is a plain JavaScript " +
      "package (index.js, rules/*.js, utils.js) with exactly one `.ts` file in it, and that file's " +
      "whole job is to be a source file typescript-eslint's project service opens to warm its " +
      "program, driven from `test/no-promise-to-serializer.test.mjs`. It carries its own two-line " +
      "`tsconfig.json` (`files: [\"warmup.ts\"]`) precisely so it resolves as a standalone project " +
      "rather than through `allowDefaultProject`; that test asserts it stays out of the " +
      "default-project fixture set. Pulling it into the workspace reference graph would defeat the " +
      "property it exists to establish. It appears here only because the derived TEST-COVERED scope " +
      "is coarse -- \"inside a member's test/ tree\" -- and this is that coarseness being honest.",
  ],
]);

// The second register, and the one #1041 exists to keep empty: JavaScript
// files anywhere in the tree that NO type checker reads. This is a wider
// question than the gap above -- it does not ask whether a suite runs the
// file, only whether anything compiles it -- and it is deliberately wider,
// because the failure it guards against is a NEW `.mjs` landing outside
// `tsconfig.guards.json`'s include globs, which the gap cannot see unless
// that file also happens to sit inside a suite's scope.
//
// It is EMPTY, and empty is the claim: every `.mjs`/`.js` in this repo is
// read by a type checker today, including the four parked `tests/fixtures`
// modules and the root/package `eslint.config.mjs` files. The stale-row
// check below still runs against it, so the day a row is added with a
// reason, the row has to keep being true.
const KNOWN_UNTYPECHECKED_JS = new Map();

test("no file a suite runs is invisible to every type checker -- the derived gap matches the reviewed baseline exactly", () => {
  const gap = deriveTypecheckScopeGap(repoRoot);

  const newlyBlind = gap.filter((file) => !KNOWN_UNTYPECHECKED.has(file));
  assert.deepEqual(
    newlyBlind,
    [],
    `${newlyBlind.length} file(s) sit inside a test scope and outside EVERY typecheck leg -- code some suite ` +
      `runs that "bun run typecheck" never compiles. This is the apps/web/test class recurring (61 test files ` +
      `hidden behind one tsconfig "exclude", 66 real type errors accumulated invisibly). Bring each into a ` +
      `typecheck leg, or add it to KNOWN_UNTYPECHECKED with a stated reason:\n${newlyBlind.join("\n")}`,
  );

  const stale = [...KNOWN_UNTYPECHECKED.keys()].filter((file) => !gap.includes(file));
  assert.deepEqual(
    stale,
    [],
    `${stale.length} KNOWN_UNTYPECHECKED row(s) no longer appear in the live derivation -- they were either ` +
      `brought into a typecheck leg (good: delete the row) or deleted/moved (fix the path). A register that ` +
      `does not track reality is the failure #877's guard-wiring check exists to catch:\n${stale.join("\n")}`,
  );
});

test("NON-VACUITY: both derived scopes are real, so an empty gap cannot be an empty derivation", () => {
  const { typechecked, testCovered } = deriveScopeSizes(repoRoot);

  // VACUITY FLOORS, NOT INVENTORIES -- low and wide on purpose, so an
  // ordinary deletion never forces an edit here, while a scope that has
  // COLLAPSED (a tsconfig reference dropped, a workspaces glob broken)
  // cannot report a clean gap. Real counts when this landed: 477
  // typechecked, 424 test-covered. Do not tighten these toward the real
  // numbers; that turns a vacuity check into a census that rots.
  assert.ok(
    typechecked > 200,
    `the derived TYPECHECKED scope holds only ${typechecked} file(s) -- the tsc legs have collapsed, and an ` +
      `empty gap from a collapsed typecheck scope is the exact silent green this guard exists to refuse`,
  );
  assert.ok(
    testCovered > 200,
    `the derived TEST-COVERED scope holds only ${testCovered} file(s) -- the workspace-member walk has ` +
      `collapsed, so this guard is comparing against nothing`,
  );
});

test("NON-VACUITY, JavaScript half: both scopes reach real .mjs/.js, so #1041's extension cannot silently collapse", () => {
  const { typecheckedJs, testCoveredJs } = deriveScopeSizes(repoRoot);

  // These floors exist because the two aggregates above CANNOT catch this.
  // `.ts` alone puts both of them in the hundreds, so dropping
  // tsconfig.guards.json out of TSC_LEGS, or breaking the `scripts/` walk in
  // deriveExecutedJsFiles, would leave every assertion in this file green
  // while the entire JavaScript corpus went dark again -- which is the exact
  // shape of the defect #1041 was opened to fix, one layer up.
  //
  // VACUITY FLOORS, NOT INVENTORIES. Real counts when this landed: 155
  // typechecked JavaScript files, 143 test-covered ones. Low and wide on
  // purpose: deleting a guard must never force an edit here.
  assert.ok(
    typecheckedJs > 60,
    `only ${typecheckedJs} JavaScript file(s) are inside any typecheck leg -- tsconfig.guards.json has ` +
      `dropped out of TSC_LEGS or its include globs no longer resolve, and every other assertion in ` +
      `this file would stay green while the guard corpus went back to being compiled by nothing`,
  );
  assert.ok(
    testCoveredJs > 60,
    `only ${testCoveredJs} JavaScript file(s) are inside the derived TEST-COVERED scope -- ` +
      `deriveExecutedJsFiles has collapsed, so the gap below is comparing the JavaScript corpus ` +
      `against nothing and would report it clean`,
  );
});

test("no JavaScript file in the tree is read by NO type checker -- the whole corpus, not only the part a suite runs", () => {
  const typechecked = deriveTypecheckedFiles(repoRoot);
  const blind = deriveAllJsFiles(repoRoot)
    .filter((file) => !typechecked.has(file))
    .map((file) => relative(repoRoot, file))
    .sort();

  const newlyBlind = blind.filter((file) => !KNOWN_UNTYPECHECKED_JS.has(file));
  assert.deepEqual(
    newlyBlind,
    [],
    `${newlyBlind.length} JavaScript file(s) are compiled by no type checker at all. Before #1041 that ` +
      `was true of the ENTIRE guard corpus and of packages/eslinter, and a typo in one was caught only ` +
      `when it ran, only if it ran, and only on the branch it took. Add the file to an include glob in ` +
      `tsconfig.guards.json, or add it to KNOWN_UNTYPECHECKED_JS with a stated reason:\n${newlyBlind.join("\n")}`,
  );

  const stale = [...KNOWN_UNTYPECHECKED_JS.keys()].filter((file) => !blind.includes(file));
  assert.deepEqual(
    stale,
    [],
    `${stale.length} KNOWN_UNTYPECHECKED_JS row(s) no longer appear in the live derivation -- they were ` +
      `either brought into a typecheck leg (good: delete the row) or deleted/moved (fix the path):\n${stale.join("\n")}`,
  );

  // The other half of "seen to fail": a derivation that found no JavaScript
  // at all would pass every assertion above with an empty `blind` list.
  assert.ok(
    deriveAllJsFiles(repoRoot).length > 100,
    "the JavaScript corpus walk found almost nothing -- an empty blind list from an empty walk is the " +
      "silent green this arm exists to refuse",
  );
});

test("DETECTION, both directions: the real derivation names an excluded test tree, and stops naming it once the exclusion goes", () => {
  const root = mkdtempSync(join(tmpdir(), "typecheck-scope-gap-"));
  try {
    // A minimal but REAL workspace: root package.json with a workspaces
    // glob, a solution-style root tsconfig referencing the member, and a
    // member with its own src/ and test/ trees. This is the live
    // `apps/web` shape reduced to its essentials, built on disk and read
    // back through the same compiler API the real derivation uses -- not a
    // stubbed return value.
    writeFileSync(join(root, "package.json"), JSON.stringify({ workspaces: ["apps/*"] }));
    writeFileSync(
      join(root, "tsconfig.json"),
      JSON.stringify({ files: [], references: [{ path: "apps/thing" }] }),
    );
    const member = join(root, "apps", "thing");
    mkdirSync(join(member, "src"), { recursive: true });
    mkdirSync(join(member, "test"), { recursive: true });
    writeFileSync(join(member, "package.json"), JSON.stringify({ name: "thing" }));
    writeFileSync(join(member, "src", "Thing.ts"), "export const thing = 1\n");
    writeFileSync(join(member, "test", "Thing.test.ts"), "export const check = 1\n");

    const memberTsconfig = join(member, "tsconfig.json");
    const withExclusion = {
      compilerOptions: { noEmit: true, composite: true },
      include: ["**/*.ts"],
      exclude: ["node_modules", "test"],
    };
    writeFileSync(memberTsconfig, JSON.stringify(withExclusion));

    const red = deriveTypecheckScopeGap(root);
    assert.deepEqual(
      red,
      ["apps/thing/test/Thing.test.ts"],
      "with the member's tsconfig excluding its own test/ tree, the derivation must name that test file -- " +
        "if it does not, this guard cannot see the defect it was built for",
    );

    // The other direction. A derivation that simply reported every test file
    // it found would pass the arm above and fail this one.
    writeFileSync(
      memberTsconfig,
      JSON.stringify({ ...withExclusion, exclude: ["node_modules"] }),
    );
    const green = deriveTypecheckScopeGap(root);
    assert.deepEqual(
      green,
      [],
      "with the exclusion removed, the same test file is inside the typecheck scope and must drop out of the gap",
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("REAL-REPO CONTROL: the web test tree the incident hid is genuinely reached by both derivations", () => {
  const subject = join(repoRoot, "apps", "web", "test", "harness", "FakeChiefApi.ts");

  assert.ok(
    deriveTestCoveredFiles(repoRoot).has(subject),
    "apps/web/test/harness/FakeChiefApi.ts is not in the derived TEST-COVERED scope -- the workspace-member " +
      "walk no longer reaches apps/web/test, so this guard would report a clean gap for a tree it cannot see",
  );
  assert.ok(
    deriveTypecheckedFiles(repoRoot).has(subject),
    "apps/web/test/harness/FakeChiefApi.ts is not in the derived TYPECHECKED scope -- apps/web/tsconfig.json " +
      "has stopped compiling its own test tree, which is the original defect verbatim",
  );
});

test("DETECTION, both directions, JavaScript: an executed .mjs/.js outside every leg is named, and stops being named once a leg reads it", () => {
  const root = mkdtempSync(join(tmpdir(), "typecheck-scope-gap-js-"));
  try {
    // The `packages/eslinter` shape reduced to its essentials: a workspace
    // member whose vitest config measures a directory that is NOT `src/` or
    // `test/`, holding JavaScript. Built on disk and read back through the
    // same derivations the real repo uses -- the `.ts` half is kept clean so
    // a failure here can only be about the JavaScript half.
    writeFileSync(join(root, "package.json"), JSON.stringify({ workspaces: ["packages/*"] }));
    writeFileSync(
      join(root, "tsconfig.json"),
      JSON.stringify({ files: [], references: [{ path: "packages/thing" }] }),
    );
    const member = join(root, "packages", "thing");
    mkdirSync(join(member, "src"), { recursive: true });
    mkdirSync(join(member, "rules"), { recursive: true });
    writeFileSync(join(member, "package.json"), JSON.stringify({ name: "thing" }));
    writeFileSync(join(member, "tsconfig.json"), JSON.stringify({ compilerOptions: { noEmit: true }, include: ["src/**/*.ts"] }));
    writeFileSync(join(member, "src", "Thing.ts"), "export const thing = 1\n");
    writeFileSync(join(member, "rules", "rule.js"), "export default {}\n");
    writeFileSync(
      join(member, "vitest.config.js"),
      "export default { test: { include: ['test/**/*.test.mjs'] }, coverage: { include: ['rules/**'] } }\n",
    );

    const red = deriveTypecheckScopeGap(root);
    assert.deepEqual(
      red,
      ["packages/thing/rules/rule.js"],
      "a JavaScript file a member's own vitest config measures, with no tsconfig reading it, must be " +
        "named -- this is packages/eslinter's exact situation before #1041, and the derivation could " +
        "not see it at all",
    );

    // The other direction, and it does double duty: a derivation that simply
    // reported every JavaScript file it found would pass the arm above and
    // fail this one, AND this only goes green if `TSC_LEGS` genuinely still
    // contains `tsconfig.guards.json` -- dropping that leg turns this red.
    writeFileSync(
      join(root, "tsconfig.guards.json"),
      JSON.stringify({
        compilerOptions: { allowJs: true, checkJs: true, noEmit: true },
        include: ["packages/*/rules/**/*.js"],
      }),
    );
    const green = deriveTypecheckScopeGap(root);
    assert.deepEqual(
      green,
      [],
      "with a guards leg reading that directory, the same file is inside the typecheck scope and must " +
        "drop out of the gap",
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("REAL-REPO CONTROL, JavaScript: a live guard and a live eslinter rule are genuinely reached by BOTH derivations", () => {
  // One file from each of the two corpora #1041 found unchecked, chosen
  // because each discriminates a different failure. `guard-wiring.test.mjs`
  // is a `scripts/test/*.test.mjs` gate: it can only be TEST-COVERED if the
  // `scripts/` walk in deriveExecutedJsFiles ran, and only TYPECHECKED if
  // tsconfig.guards.json's `scripts/**/*.mjs` glob resolved.
  // `no-barrel-re-export.js` sits under neither `src/` nor `test/`, so it can
  // only be TEST-COVERED if the vitest `coverage.include` parse worked --
  // the specific mechanism that made packages/eslinter invisible.
  const testCovered = deriveTestCoveredFiles(repoRoot);
  const typechecked = deriveTypecheckedFiles(repoRoot);

  for (const subject of [
    join(repoRoot, "scripts", "test", "guard-wiring.test.mjs"),
    join(repoRoot, "packages", "eslinter", "rules", "no-barrel-re-export.js"),
  ]) {
    assert.ok(
      testCovered.has(subject),
      `${relative(repoRoot, subject)} is not in the derived TEST-COVERED scope -- the JavaScript half of ` +
        `deriveTestCoveredFiles no longer reaches it, so this guard would report a clean gap for a corpus ` +
        `it cannot see`,
    );
    assert.ok(
      typechecked.has(subject),
      `${relative(repoRoot, subject)} is not in the derived TYPECHECKED scope -- tsconfig.guards.json has ` +
        `stopped compiling it, which is #1041's original defect verbatim`,
    );
  }
});
