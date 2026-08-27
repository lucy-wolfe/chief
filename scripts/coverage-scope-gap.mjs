// #970: what code is OUTSIDE every instrument's scope, and is real anyway.
//
// eng-4 found `tests/e2e/harness/org-world.ts`'s `chiefd-new` deps object
// missing a required `createCompany` field -- invisible to `tsc` (`tests/**`
// is deliberately not typechecked, per `scripts/typecheck.sh`'s own comment)
// and invisible to every test run (the flow needs real chiefd+beacond
// binaries, so it is not part of any routine gate). The defect sat for two
// days (since `fc93ac9f8`, 2026-08-05) in a test HARNESS -- code whose
// entire job is producing evidence.
//
// This DERIVES the general version of that gap rather than hand-listing
// files, the same discipline every other tool built tonight used: read the
// tsconfig graph for what is typechecked, read the vitest configs for what
// is test-covered, and report the DIFFERENCE -- restricted to files
// something in the tree actually imports, since a file nothing depends on
// being correct is not this finding's class (a fixture, a generated
// artifact, or genuine dead code legitimately sits outside every scope).
//
// SCOPE, STATED EXPLICITLY:
// - TYPECHECKED is the exact union of `fileNames` from the four real `tsc`
//   legs `scripts/typecheck.sh` runs (tsconfig.json's workspace reference
//   graph, tsconfig.extensions.json, #1041's
//   tsconfig.guards.json -- the `allowJs`+`checkJs` leg over the JavaScript
//   corpus -- and tsconfig.scripts.json, the strict leg over that same
//   directory's TypeScript) -- derived via the TypeScript compiler API against the REAL
//   configs, not a hand-copied list of paths. A bun-check leg was one of them
//   until its only checkable driver went with the E2E corpus, and a
//   capabilities leg (tsconfig.capabilities.json, package skill sources) was
//   another until the package skills stopped shipping TypeScript; see
//   scripts/typecheck.sh for why each was deleted rather than floored to zero.
// - TEST-COVERED is coarse, not a full resolved-import graph: every
//   workspace package's own `src/**` and `test/**`, PLUS the JavaScript a
//   suite runs: everything under `scripts/` (the `node --test
//   scripts/test/*.test.mjs` gate, and the sibling derivations those guards
//   import), and every file a workspace member's OWN vitest
//   `test.include`/`coverage.include` globs resolve to, read out of the real
//   configs -- which is what finally reaches `packages/eslinter`, whose
//   coverage.include names `rules/**`, `index.js` and `utils.js` rather than
//   a `src/` tree. #1041: both `scripts/` clauses were CLAIMED by this
//   header, and by `deriveTestCoveredFiles`'s own docstring, while the walk
//   they delegated to filtered to `.ts`/`.tsx` and returned not one of them.
//   This is NOT a resolved import-reachability proof -- a file
//   inside a package's `src/**` that nothing actually imports still counts
//   as "test-covered" here, because vitest's own coverage.include already
//   makes that exact (generous) claim; narrowing it further is a different,
//   harder tool this one does not attempt to be.
// - "Something depends on it" is a TEXT-SPECIFIER match (does any file
//   anywhere in the tree contain an import/require specifier whose final
//   path segment equals this file's own basename-without-extension), not a
//   resolved-module proof. This can both under- and over-count: a dynamic
//   `import(computedPath)` is invisible to it, and two unrelated files
//   sharing a basename would be conflated. It is the same class of
//   deliberate simplification `deletion-scope-audit.mjs` documents for its
//   own "unresolved, plausible for stem" bucket -- cheap, real, and named
//   as an approximation rather than presented as a proof.
// - This is a text-search/config-parse claim about which files are
//   REACHED by an instrument's own stated scope, not a claim about whether
//   those instruments actually assert anything meaningful about the files
//   they do reach (a file INSIDE every scope can still be poorly tested).
//   Never read a clean report here as "everything is covered well" --
//   only as "this specific two-day-blind-spot class did not recur."

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";
import ts from "typescript";

import { skipSet } from "./tree-walk-lib.mjs";

// `.claude` holds the agent harness's own working area, including
// `.claude/worktrees/<agent>/` — FULL nested checkouts of this same repo,
// excluded from git by `.git/info/exclude`. Walking them made this
// derivation report another checkout's files as THIS tree's coverage gap
// (observed live: 1387 "referenced-but-uncovered" files, of which 1339 were
// six worktree copies of the 48 real ones). That is not a stricter result,
// it is a wrong one: the gap this tool measures is a property of one working
// tree, and a nested checkout's `apps/api/src/common/Env.ts` is not a file
// this tree's instruments were ever supposed to reach. Excluded here rather
// than filtered at the call site so every consumer of the derivation gets
// the same answer, and so the guard's result stops depending on whether the
// machine running it happens to have agent worktrees on disk.
/**
 * Directories this scan never descends into.
 *
 * The shared members come from `tree-walk-lib`, including the `.claude`
 * exclusion this file used to carry on its own. That one-off was correct and
 * invisible: it sat here while five other walking guards had no such line and
 * were red on every seat's machine, which is the whole argument for one
 * definition. `generated` is this scan's own — generated sources are not
 * hand-written coverage scope.
 */
const EXCLUDED_DIRS = skipSet(["generated"]);
const CODE_EXTENSIONS = new Set([".ts", ".tsx"]);

// #1041: JavaScript is code here, not an exception to it. The 60 gates
// `scripts/guard-count.mjs` derives are written in `.mjs`, and
// `packages/eslinter` is 40 `.js` files that decide what every other package
// may compile. Kept as its own set rather than merged into CODE_EXTENSIONS
// because the two are used for different questions: CODE_EXTENSIONS is the
// candidate universe `deriveCoverageGap` partitions (a `.ts`-only question,
// unchanged), and this is the executed-JavaScript corpus the typecheck-scope
// gap has to see.
const EXECUTED_JS_EXTENSIONS = new Set([".mjs", ".js", ".cjs"]);

// The exact four tsc legs `scripts/typecheck.sh` runs, in the order it runs
// them. A fifth leg added there without a matching entry here is exactly
// the class of drift this tool exists to notice -- see the accompanying
// test's own real-repo check that this list still matches the script.
// tsconfig.scripts.json was the fifth, and it arrived because `scripts/**/*.ts`
// was in no typechecked project at all while its `.mjs` siblings had been in
// one since #1041 -- so this list said "typechecked" over a directory it was
// reading only half of.
export const TSC_LEGS = [
  "tsconfig.json",
  "tsconfig.extensions.json",
  "tsconfig.guards.json",
  "tsconfig.scripts.json",
];

function isSolutionStyleConfig(config) {
  const noInclude = !Array.isArray(config.include) || config.include.length === 0;
  const noFiles = !Array.isArray(config.files) || config.files.length === 0;
  const hasRefs = Array.isArray(config.references) && config.references.length > 0;
  return noInclude && noFiles && hasRefs;
}

/** Every file TypeScript itself would include for one config path, walking
 *  solution-style `references` recursively -- the same resolution
 *  `deletion-scope-audit.mjs`'s `memberLeafConfigs` performs for a single
 *  workspace member, generalized here to any config path (a plain leaf
 *  config, e.g. tsconfig.extensions.json, is its own single leaf). */
function filesForConfig(configPath) {
  const files = new Set();
  const seen = new Set();
  function addLeaf(path) {
    const absPath = resolve(path);
    if (seen.has(absPath) || !existsSync(absPath)) return;
    seen.add(absPath);
    const { config, error } = ts.readConfigFile(absPath, ts.sys.readFile);
    if (error) throw new Error(`cannot read ${absPath}: ${ts.flattenDiagnosticMessageText(error.messageText, " ")}`);
    if (isSolutionStyleConfig(config)) {
      for (const reference of config.references ?? []) {
        const refPath = join(dirname(absPath), reference.path);
        addLeaf(refPath.endsWith(".json") ? refPath : join(refPath, "tsconfig.json"));
      }
      return;
    }
    const parsed = ts.parseJsonConfigFileContent(config, ts.sys, dirname(absPath));
    for (const file of parsed.fileNames) files.add(resolve(file));
  }
  addLeaf(configPath);
  return files;
}

/** Union of every file the five real `tsc` legs would check. */
export function deriveTypecheckedFiles(root) {
  const files = new Set();
  for (const leg of TSC_LEGS) {
    for (const file of filesForConfig(join(root, leg))) files.add(file);
  }
  return files;
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

/** Every workspace member directory (mirrors `deletion-scope-audit.mjs`'s
 *  own `workspaceMembers`). */
function workspaceMembers(root) {
  const rootPkg = readJson(join(root, "package.json"));
  const members = [];
  for (const glob of rootPkg.workspaces ?? []) {
    const base = String(glob).replace(/\/\*$/, "");
    const dir = join(root, base);
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir)) {
      const memberDir = join(base, name);
      if (existsSync(join(root, memberDir, "package.json"))) members.push(memberDir);
    }
  }
  return members;
}

function walk(dir, out) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (EXCLUDED_DIRS.has(entry.name)) continue;
      walk(join(dir, entry.name), out);
      continue;
    }
    if (entry.isFile() && CODE_EXTENSIONS.has(extname(entry.name))) out.push(join(dir, entry.name));
  }
}

/** Every `.ts`/`.tsx` file in the tree, excluding the usual build/vendor
 *  directories. The candidate universe this tool partitions. */
export function deriveAllCodeFiles(root) {
  const out = [];
  walk(root, out);
  return out.map((f) => resolve(f));
}

/** Like `walk`, but over any predicate on the full path -- the JavaScript
 *  half needs suffix matching (`*.test.mjs`) that an extension set cannot
 *  express. */
function walkWhere(dir, keep, out) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (EXCLUDED_DIRS.has(entry.name)) continue;
      walkWhere(path, keep, out);
      continue;
    }
    if (entry.isFile() && keep(path)) out.push(path);
  }
}

/** Every `.mjs`/`.js`/`.cjs` file anywhere in the tree, excluding the usual
 *  build/vendor directories. Not a coverage claim: this is the candidate
 *  universe for "is this file read by ANY type checker", which is a question
 *  about the whole JavaScript corpus rather than only its executed part. */
export function deriveAllJsFiles(root) {
  const out = [];
  walkWhere(root, (f) => EXECUTED_JS_EXTENSIONS.has(extname(f)), out);
  return out.map((f) => resolve(f));
}

/** The include globs a workspace member's OWN vitest config names -- both
 *  `test.include` (what it runs) and `coverage.include` (what it claims to
 *  measure). Read out of the real config, because the hardcoded `src`+`test`
 *  proxy below is only correct for members that happen to use that layout:
 *  `packages/eslinter`'s coverage.include is `['rules/**', 'index.js',
 *  'utils.js']`, none of which is under either directory, which is one half
 *  of why that whole package was invisible to this file. A TEXT scan, not an
 *  evaluation of the config -- every include in this repo is a literal array
 *  of string literals, and a scan that cannot see a computed one is honest
 *  about that by finding nothing rather than by guessing. */
function vitestIncludeGlobs(memberAbs) {
  for (const name of ["vitest.config.ts", "vitest.config.mts", "vitest.config.js", "vitest.config.mjs"]) {
    const path = join(memberAbs, name);
    if (!existsSync(path)) continue;
    const globs = [];
    for (const includeArray of readFileSync(path, "utf8").matchAll(/include:\s*\[([^\]]*)\]/g)) {
      for (const quoted of includeArray[1].matchAll(/['"]([^'"]+)['"]/g)) globs.push(quoted[1]);
    }
    return globs;
  }
  return [];
}

/** Resolve one vitest include glob to the real JavaScript files under it.
 *  Handles the two shapes this repo's configs use: a literal path
 *  (`index.js`) and a prefix/suffix pattern (`test/**` + `/*.test.mjs`,
 *  `rules/**`). */
function jsFilesForGlob(baseDir, glob, out) {
  const firstStar = glob.indexOf("*");
  if (firstStar === -1) {
    const path = join(baseDir, glob);
    if (existsSync(path) && EXECUTED_JS_EXTENSIONS.has(extname(path))) out.push(path);
    return;
  }
  const prefix = glob.slice(0, glob.lastIndexOf("/", firstStar) + 1);
  const suffix = glob.slice(glob.lastIndexOf("*") + 1);
  const scanRoot = join(baseDir, prefix);
  if (!existsSync(scanRoot)) return;
  walkWhere(scanRoot, (f) => f.endsWith(suffix) && EXECUTED_JS_EXTENSIONS.has(extname(f)), out);
}

/** The JavaScript a suite in this repo actually RUNS. Two sources, both
 *  derived from something real:
 *
 *  1. Everything under `scripts/`. `node --test scripts/test/*.test.mjs` is a
 *     real, CI-wired gate (`bun run test:pre-push-guards`), and every guard
 *     imports the derivation it checks out of a sibling `scripts/*.mjs`, so
 *     the directory is executed by that gate directly or transitively.
 *     Coarse in the same generous direction as the `src/**` claim -- a
 *     one-shot tool under `scripts/debug/` counts too -- and that error can
 *     only make a reported gap LARGER, never smaller.
 *  2. Every workspace member's own vitest include globs, resolved on disk.
 *
 *  This is the half `deriveTestCoveredFiles`'s docstring has always CLAIMED
 *  ("plus every `scripts/test/*.mjs` guard and every `scripts/*.mjs` file")
 *  and never implemented: the walk it delegated to filters to `.ts`/`.tsx`,
 *  so no JavaScript file has ever been in its answer. */
export function deriveExecutedJsFiles(root) {
  const covered = new Set();
  const scriptsDir = join(root, "scripts");
  if (existsSync(scriptsDir)) {
    const out = [];
    walkWhere(scriptsDir, (f) => EXECUTED_JS_EXTENSIONS.has(extname(f)), out);
    for (const f of out) covered.add(resolve(f));
  }
  for (const memberDir of workspaceMembers(root)) {
    const memberAbs = resolve(root, memberDir);
    const out = [];
    for (const glob of vitestIncludeGlobs(memberAbs)) jsFilesForGlob(memberAbs, glob, out);
    for (const f of out) covered.add(resolve(f));
  }
  return covered;
}

/** Coarse test-coverage scope: every workspace member's own `src/**` and
 *  `test/**` (mirrors every `vitest.config.ts`'s own `coverage.include:
 *  ['src/**']` claim), plus every JavaScript file a suite runs
 *  (`deriveExecutedJsFiles`). STATED LIMITATION: this is "inside a
 *  package's own src/test tree," not "actually imported by a running
 *  test" -- see the header comment. */
export function deriveTestCoveredFiles(root) {
  const covered = new Set();
  for (const memberDir of workspaceMembers(root)) {
    const memberAbs = resolve(root, memberDir);
    for (const sub of ["src", "test"]) {
      const subAbs = join(memberAbs, sub);
      if (!existsSync(subAbs)) continue;
      const out = [];
      walk(subAbs, out);
      for (const f of out) covered.add(resolve(f));
    }
  }
  for (const f of deriveExecutedJsFiles(root)) covered.add(f);
  return covered;
}

/** Every real import/require specifier's final path segment, across the
 *  whole tree (not scoped to `.ts`/`.tsx` alone -- `.mjs`/`.js` importers
 *  count as real dependents too). A crude, real, TEXT match -- see the
 *  header comment for exactly what this can and cannot see. */
function specifierBasenames(root) {
  const basenames = new Set();
  const files = [];
  walk(root, files);
  // Also scan .mjs/.js importers (scripts/, tests/e2e/harness/*.ts import
  // each other, but scripts/*.mjs also import product code sometimes).
  for (const dir of ["scripts", "tests"]) {
    const abs = join(root, dir);
    if (!existsSync(abs)) continue;
    (function walkAny(d) {
      for (const entry of readdirSync(d, { withFileTypes: true })) {
        if (entry.isDirectory()) {
          if (EXCLUDED_DIRS.has(entry.name)) continue;
          walkAny(join(d, entry.name));
          continue;
        }
        if (entry.isFile() && /\.(mjs|js|ts|tsx)$/.test(entry.name)) files.push(join(d, entry.name));
      }
    })(abs);
  }
  const pattern = /from\s+["']([^"']+)["']|require\(\s*["']([^"']+)["']\s*\)|import\(\s*["']([^"']+)["']\s*\)/g;
  for (const file of files) {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    for (const match of text.matchAll(pattern)) {
      const specifier = match[1] ?? match[2] ?? match[3];
      if (!specifier || !specifier.startsWith(".")) continue;
      const base = specifier.split("/").pop().replace(/\.(ts|tsx|mjs|js)$/, "");
      if (base) basenames.add(base);
    }
  }
  return basenames;
}

/** The gap: files outside BOTH the typecheck scope and the coarse test
 *  scope, restricted to files something in the tree textually imports by
 *  basename (see header for the exact, stated limitation of that check).
 *  Returns `{ referenced, unreferenced }` -- `unreferenced` is printed for
 *  transparency (per #970's own instruction not to silently drop it) but
 *  is NOT the finding: a file nothing imports is a fixture/dead-code
 *  question, not a "something depends on it and nothing checks it" one. */
export function deriveCoverageGap(root) {
  const typechecked = deriveTypecheckedFiles(root);
  const testCovered = deriveTestCoveredFiles(root);
  const all = deriveAllCodeFiles(root);
  const gap = all.filter((f) => !typechecked.has(f) && !testCovered.has(f));
  const basenames = specifierBasenames(root);
  const referenced = [];
  const unreferenced = [];
  for (const f of gap) {
    const stem = f.split("/").pop().replace(/\.(ts|tsx)$/, "");
    (basenames.has(stem) ? referenced : unreferenced).push(relative(root, f));
  }
  return { referenced: referenced.sort(), unreferenced: unreferenced.sort() };
}

function main() {
  const root = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
  const { referenced, unreferenced } = deriveCoverageGap(root);
  console.log(`[coverage-scope-gap] ${referenced.length} file(s) referenced by something in the tree but outside every typecheck/test scope:`);
  for (const f of referenced) console.log(`  ${f}`);
  console.log(`\n${unreferenced.length} more file(s) outside every scope but nothing textually references (fixtures/dead-code candidates, not this finding's class) -- not printed individually, see the guard test for the full list.`);
  console.log(
    "\nSCOPE: TYPECHECKED is the real union of the five tsc legs scripts/typecheck.sh runs (tsconfig.json graph, " +
      "extensions, capabilities, guards, scripts). TEST-COVERED is coarse -- every workspace member's own src/**+test/** " +
      "(matching every vitest.config.ts's own coverage.include claim), not a resolved import-reachability proof. " +
      "'Referenced' is a text-specifier basename match, not a resolved module proof -- can miss a dynamic " +
      "computed-path import, and can conflate two files that share a basename. A clean report is a claim that " +
      "this specific two-day-blind-spot class did not recur, never a claim that everything in scope is tested well.",
  );
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
