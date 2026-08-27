// #952: #947 found a test file asserting against a fake dependency
// (`setDurableStoreForTests`) that production code no longer consults —
// `createOrganization` had moved to constructing its own `ChiefdClient`
// directly, and every assertion in the old test passed while observing
// nothing. A green test that observes nothing is worse than a red one: a
// red test gets fixed, this shape passes indefinitely and nothing about a
// normal run reveals the gap. This derives the same class from the tree
// instead of waiting for the next one to be found by hand.
//
// Built on #919's `deletion-scope-audit.mjs` shape per explicit ruling:
// derive the real reference set from the tree (never a second hand-typed
// census), and state what was filtered out rather than hiding it. Two
// derivations of "who references what" in this repo would be two sources
// of truth waiting to disagree, so this reuses the same AST-resolution
// approach (real TypeScript parsing, real relative-path resolution) rather
// than inventing a second one.
//
// SCOPE, STATED EXPLICITLY (never silently assumed complete): this detects
// exactly the repo's established test-seam convention — an exported
// `set<X>ForTests` function living alongside the "real" accessor(s) it
// exists to substitute for in tests (e.g. `durableStore`/
// `setDurableStoreForTests` in org-durable-store.ts). A fake built a
// different way (a per-instance method on a locally constructed object,
// e.g. the dead `fakeChiefd().setAlwaysRefuse(...)` #947 also found) is
// NOT a global export this scanner can see and is out of scope — flagged
// here rather than silently implied covered.
//
// RESOLVER SCOPE, NAMED SEPARATELY FROM THE ABOVE: importers are resolved
// via RELATIVE specifiers only (`resolveRelative` below) -- a setter whose
// accessor is imported through a tsconfig path alias (`@/...`) will report
// as ORPHANED even when it is genuinely load-bearing. This is #919's own
// alias-resolver blind spot, inherited by reuse of the same relative-only
// approach rather than #919's later `ts.resolveModuleName` fix (deliberate,
// per the standing rule against two derivations of "who references what"
// disagreeing -- see the note above -- but the SHARED resolver's limit is
// real here too, not just #919's). Not yet a live wrong answer: all three
// `set<X>ForTests` exports on this tree live under `apps/cli/src/legacy/
// organization/`, which imports relatively; `@/` is a `packages/*`
// convention none of the three currently cross. The first `set<X>ForTests`
// added under `packages/*` is where this starts lying -- and it lies the
// dangerous way, reporting a LIVE seam as ORPHANED, which invites deleting
// a fake that is actually in use. Stated here so that day is a known risk,
// not a surprise.

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";
import ts from "typescript";

import { skipSet } from "./tree-walk-lib.mjs";

/**
 * Directories this scan never descends into — the shared definition. This
 * detector was one of the two that did not merely report a wrong finding inside
 * `.claude/worktrees/<name>/` but DIED there, on `ENOENT` from a dangling path
 * in a nested checkout; skipping the directory during the walk is what fixes
 * that, where filtering the results afterwards would not.
 */
const EXCLUDED_DIRS = skipSet();
const CODE_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".cts"]);
const SEAM_SETTER_PATTERN = /^set[A-Za-z0-9]*ForTests$/;
const TEST_PATH_PATTERN = /(^|\/)(test|tests)(\/|$)|\.(test|bun-check)\.[mc]?ts$/;

function walkFiles(root, extensions) {
  const out = [];
  (function walk(dir) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        if (EXCLUDED_DIRS.has(entry.name)) continue;
        walk(join(dir, entry.name));
        continue;
      }
      if (entry.isFile() && extensions.has(extname(entry.name))) out.push(join(dir, entry.name));
    }
  })(root);
  return out;
}

function isTestFile(relPath) {
  return TEST_PATH_PATTERN.test(relPath);
}

function parse(file) {
  return ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
}

/** Every top-level exported binding name in a file: `export function x`,
 * `export const x = ...`, `export class x`. Real AST parsing — never a
 * regex — so a string that merely contains the word `export` cannot be
 * mistaken for one, matching this repo's established standard
 * (dep-declaration.mjs, deletion-scope-audit.mjs, #873/#919). */
function exportedTopLevelNames(source) {
  const names = [];
  for (const node of source.statements) {
    const hasExportModifier = ts.canHaveModifiers(node) && ts.getModifiers(node)?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword);
    if (!hasExportModifier) continue;
    if ((ts.isFunctionDeclaration(node) || ts.isClassDeclaration(node)) && node.name) {
      names.push(node.name.text);
    } else if (ts.isVariableStatement(node)) {
      for (const decl of node.declarationList.declarations) {
        if (ts.isIdentifier(decl.name)) names.push(decl.name.text);
      }
    }
  }
  return names;
}

/** Every `set<X>ForTests` export repo-wide, with the other real exports
 * from the SAME file it sits alongside — the accessor(s) it exists to
 * substitute for in a test. */
export function discoverSeamSetters(root) {
  const setters = [];
  for (const file of walkFiles(root, CODE_EXTENSIONS)) {
    const source = parse(file);
    const names = exportedTopLevelNames(source);
    const setterNames = names.filter((n) => SEAM_SETTER_PATTERN.test(n));
    if (setterNames.length === 0) continue;
    const otherExports = names.filter((n) => !SEAM_SETTER_PATTERN.test(n));
    for (const name of setterNames) {
      setters.push({ name, file: relative(root, file), otherExports });
    }
  }
  return setters;
}

function namedImportSpecifiers(source) {
  const imports = [];
  for (const statement of source.statements) {
    if (!ts.isImportDeclaration(statement) || !statement.moduleSpecifier || !ts.isStringLiteral(statement.moduleSpecifier)) continue;
    const specifier = statement.moduleSpecifier.text;
    const clause = statement.importClause;
    if (!clause?.namedBindings || !ts.isNamedImports(clause.namedBindings)) continue;
    const names = clause.namedBindings.elements.map((el) => (el.propertyName ?? el.name).text);
    imports.push({ specifier, names });
  }
  return imports;
}

function resolveRelative(fromFile, specifier) {
  if (!specifier.startsWith("./") && !specifier.startsWith("../")) return undefined;
  const base = resolve(dirname(fromFile), specifier);
  const candidates = [base, ...[...CODE_EXTENSIONS].map((ext) => base + ext), ...[...CODE_EXTENSIONS].map((ext) => join(base, "index" + ext))];
  for (const candidate of candidates) {
    if (existsSync(candidate) && statSync(candidate).isFile()) return resolve(candidate);
  }
  return undefined;
}

/** For a target file + a set of exported names, every file that imports at
 * least one of those exact names from it — split into production vs test
 * importers, matching #919's load-bearing/informational split in spirit:
 * a NAMED import of the real export is the load-bearing edge; anything
 * else about the importing file is not this function's concern. */
function importersOfNames(root, targetAbs, names) {
  const nameSet = new Set(names);
  const production = [];
  const test = [];
  for (const file of walkFiles(root, CODE_EXTENSIONS)) {
    if (resolve(file) === targetAbs) continue;
    const source = parse(file);
    for (const { specifier, names: importedNames } of namedImportSpecifiers(source)) {
      const resolved = resolveRelative(file, specifier);
      if (resolved !== targetAbs) continue;
      if (!importedNames.some((n) => nameSet.has(n))) continue;
      const rel = relative(root, file);
      (isTestFile(rel) ? test : production).push(rel);
      break;
    }
  }
  return { production: [...new Set(production)].sort(), test: [...new Set(test)].sort() };
}

/** The full report: every seam setter classified as:
 *   'load-bearing' — at least one of its sibling real exports has a
 *                     production (non-test) importer somewhere in the tree.
 *   'orphaned'      — zero production importers among its sibling exports,
 *                      AND at least one test file still imports the SETTER
 *                      itself — a live fake nothing in production can see.
 *   'unused'        — zero production importers AND zero test consumers of
 *                      the setter either. Informational, stated separately:
 *                      this is dead code, not a "green that observes
 *                      nothing" test (there is no such test to mislead). */
export function detectOrphanedFakes(root) {
  const setters = discoverSeamSetters(root);
  const results = [];
  for (const setter of setters) {
    const targetAbs = resolve(root, setter.file);
    const accessorImporters = importersOfNames(root, targetAbs, setter.otherExports);
    const setterImporters = importersOfNames(root, targetAbs, [setter.name]);
    let status;
    if (accessorImporters.production.length > 0) status = "load-bearing";
    else if (setterImporters.test.length > 0) status = "orphaned";
    else status = "unused";
    results.push({
      setter: setter.name,
      file: setter.file,
      otherExports: setter.otherExports,
      productionImportersOfAccessors: accessorImporters.production,
      testFilesConsumingSetter: setterImporters.test,
      status,
    });
  }
  return results;
}

function main() {
  const root = resolve(new URL("..", import.meta.url).pathname);
  const results = detectOrphanedFakes(root);
  const orphaned = results.filter((r) => r.status === "orphaned");
  const unused = results.filter((r) => r.status === "unused");
  const loadBearing = results.filter((r) => r.status === "load-bearing");

  console.log(`${results.length} test-seam setter(s) found (${loadBearing.length} load-bearing, ${orphaned.length} orphaned, ${unused.length} unused/informational).`);
  for (const r of orphaned) {
    console.log(`\nORPHANED: ${r.setter} (${r.file})`);
    console.log(`  sibling exports with zero production importers: ${r.otherExports.join(", ") || "(none)"}`);
    console.log(`  test file(s) injecting this fake for nothing:`);
    for (const f of r.testFilesConsumingSetter) console.log(`    ${f}`);
  }
  if (unused.length > 0) {
    console.log(`\n${unused.length} unused/informational (dead seam, no misled test): ${unused.map((r) => r.setter).join(", ")}`);
  }
  console.log(
    "\nSCOPE: importers resolved via relative specifiers only; a setter whose accessor is imported " +
      "through a tsconfig path alias will report as ORPHANED.",
  );
  process.exit(orphaned.length > 0 ? 1 : 0);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
