// Files a suite RUNS that no type checker ever READS.
//
// `scripts/coverage-scope-gap.mjs` (#970) reports files outside BOTH the
// typecheck scope and the test scope. That shape can never see this one: a
// file inside the test scope is, by construction, not in its answer. So the
// repo had no instrument at all for the opposite defect — code that is
// executed by a real suite while `bun run typecheck`, the gate whose entire
// job is finding type errors, never compiles it.
//
// It was not hypothetical. `apps/web/tsconfig.json` carried `"exclude":
// ["node_modules", "test"]`, so every file under `apps/web/test/**` — 61 test
// files plus their harnesses, all of them run on every `bun run test` — was
// compiled by nothing but Vitest's own transform, which strips types instead
// of checking them. Removing the exclusion surfaced 66 real type errors that
// had accumulated invisibly. Every gate was green throughout.
//
// This is the honest analogue of `ci.yml`'s `cargo fmt --check` non-vacuity
// step (which refuses when `apps/chiefd` resolves under ~200 `.rs` files):
// a check is only worth its green if it can be shown to be looking at
// something.
//
// DERIVED, NOT ENUMERATED
// -----------------------
// Both scopes come from `scripts/coverage-scope-gap.mjs`'s own exported
// derivations, which read the REAL configs:
//
//   TYPECHECKED  — the exact union of `fileNames` the three `tsc` legs
//                  `scripts/typecheck.sh` runs would compile, resolved
//                  through the TypeScript compiler API against the real
//                  tsconfigs and their reference graph.
//   TEST-COVERED — every workspace member's own `src/**` and `test/**`,
//                  matching every `vitest.config.ts`'s own
//                  `coverage.include`/`test.include` claim.
//
// Reusing those two functions is deliberate: a guard that carried its own
// copy of "what is typechecked" would be a second source of truth, and a
// second source of truth is the mistake this repo has paid for more than any
// other. It also means this guard inherits their stated limitations exactly,
// rather than inventing new ones — see that file's header.
//
// SCOPE, STATED EXPLICITLY
// ------------------------
// - TEST-COVERED here is the same COARSE claim `coverage-scope-gap.mjs`
//   makes: "inside a workspace member's own src/ or test/ tree", not "proven
//   reachable from a running test". A `src/**` file nothing imports still
//   counts as test-covered, because vitest's `coverage.include` already makes
//   that (generous) claim. That direction of error is the safe one here: it
//   can only make this guard report MORE files, never fewer.
// - This says nothing about whether the type checker that reads a file
//   asserts anything useful about it. A file inside the typecheck scope can
//   still be riddled with `any`. A clean report here means "no file a suite
//   runs is invisible to every type checker" — never "the types are good".
// - #1041: `.mjs`/`.js` used to be outside both derivations by construction,
//   and that exclusion hid the single largest instance of this file's own
//   defect from this file. The ~60 gates `node scripts/guard-count.mjs`
//   derives are `.mjs`; `packages/eslinter` is 40 `.js` rule files that
//   decide what every other package may compile; no type checker read a line
//   of any of it. The corpus this repo trusts most to decide whether a change
//   may land was the one corpus nothing compiled, and this guard's own header
//   said so and stopped there. Both derivations now count JavaScript:
//   `tsconfig.guards.json` is a real leg of `scripts/typecheck.sh`, and
//   `deriveTestCoveredFiles` unions `deriveExecutedJsFiles`. A blind spot
//   named in a header is still a blind spot -- it just reads better.

import { relative } from "node:path";

import { deriveTestCoveredFiles, deriveTypecheckedFiles } from "./coverage-scope-gap.mjs";

/** Repo-relative paths of every file inside the test scope and outside every
 *  typecheck leg — "run by something, read by no type checker". Sorted, so a
 *  caller can diff two runs. */
export function deriveTypecheckScopeGap(root) {
  const typechecked = deriveTypecheckedFiles(root);
  const testCovered = deriveTestCoveredFiles(root);
  return [...testCovered]
    .filter((file) => !typechecked.has(file))
    .map((file) => relative(root, file))
    .sort();
}

/** The scope sizes, so a consumer can refuse on a vacuous derivation instead
 *  of reading an empty gap as good news. A zero-file typecheck scope and a
 *  perfectly clean repo produce the identical empty answer.
 *
 *  The JavaScript halves are reported SEPARATELY, and that separation is the
 *  point of them: `.ts` alone clears any aggregate floor either total could
 *  carry, so #1041's extension could collapse back to zero -- the guards leg
 *  dropped from `TSC_LEGS`, the `scripts/` walk broken -- while both
 *  aggregates stayed comfortably green and the gap stayed comfortably empty.
 *  A per-half floor is the only shape that tells "no JavaScript is blind"
 *  apart from "no JavaScript is being looked at". */
export function deriveScopeSizes(root) {
  const typechecked = deriveTypecheckedFiles(root);
  const testCovered = deriveTestCoveredFiles(root);
  const isJs = (file) => /\.(mjs|cjs|js)$/.test(file);
  return {
    typechecked: typechecked.size,
    testCovered: testCovered.size,
    typecheckedJs: [...typechecked].filter(isJs).length,
    testCoveredJs: [...testCovered].filter(isJs).length,
  };
}

function main() {
  const root = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
  const gap = deriveTypecheckScopeGap(root);
  const sizes = deriveScopeSizes(root);
  console.log(
    `[typecheck-scope-gap] TYPECHECKED=${sizes.typechecked} file(s) (${sizes.typecheckedJs} JavaScript), ` +
      `TEST-COVERED=${sizes.testCovered} file(s) (${sizes.testCoveredJs} JavaScript)`,
  );
  console.log(`[typecheck-scope-gap] ${gap.length} file(s) run by a suite but read by NO type checker:`);
  for (const file of gap) console.log(`  ${file}`);
  console.log(
    "\nSCOPE: both sets are DERIVED (scripts/coverage-scope-gap.mjs's own exported derivations -- the real tsc " +
      "legs scripts/typecheck.sh runs, and every workspace member's own src/**+test/**). TEST-COVERED is coarse: " +
      "'inside a member's src/ or test/ tree', not a resolved import-reachability proof. An empty answer is only " +
      "meaningful alongside the two scope sizes above -- a vacuous typecheck scope produces the same empty list " +
      "as a clean repo.",
  );
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
