// #919: a deletion story's "affected tests" table is hand-typed today, and
// #817/#946/#820/#830 each show that list falling short of the real
// reference set -- caught late (a follow-up commit, or CI at merge time)
// instead of by the story itself. This derives the real reference set for a
// deletion target from the tree, so a story's own list can be checked
// against ground truth instead of trusted as one.
//
// Two disjoint categories are reported, never merged into one number:
//   - load-bearing: a file that actually `import`/`require`s the target,
//     resolved via real TypeScript module resolution (never a regex or a
//     text-mention count) -- deleting the target breaks this file's
//     compile/runtime, full stop.
//   - informational: a file that merely MENTIONS the target's name as text
//     (comments, docs, CHANGELOG/DECISIONS lines, plan files) -- worth
//     knowing about, never silently dropped, but not a compile/runtime
//     break, so never conflated with a load-bearing hit.
//
// A prior incident this session (#948's own gate run) showed why the split
// matters: a naive tree-wide grep for a moved test file returned 13 hits: a
// seat scoped to "must remain referenced" would have gone and edited plan
// documents and append-only ledgers to satisfy the check. Scoped to
// load-bearing (real import edges), it was 2. 13 was a number; 2 was a
// finding. This tool states BOTH counts and which files fall in which
// bucket, never hiding the filtered-out 11 -- a stated filter is a scope, a
// hidden one is a lie by omission.
//
// STRUCTURAL LESSON, FOUND THREE TIMES ON THIS TOOL'S FIRST NIGHT, STATED
// HERE FOR WHOEVER ADDS A FOURTH RESOLVER: a reference-completeness tool is
// only as complete as its notion of what counts as a reference. This one's
// resolver has needed fixing twice already -- (1) it originally matched only
// LITERAL relative specifiers (`./`, `../`) and was blind to this repo's
// dominant `@/` tsconfig-path-alias import style, silently filing a real
// load-bearing importer (`packages/testing/src/index.ts` importing
// `ChiefdBinary.ts` via `@/ChiefdBinary`) as though it were not one --
// WORSE than a missing file, because it answered with confidence under a
// heading that told the reader it was safe to ignore. Fixed by resolving
// EVERY specifier through `ts.resolveModuleName` against the importing
// file's own real, parsed compiler options (the same machinery
// `dep-declaration.mjs` already proved correct for this exact alias
// convention), never a hand-rolled relative-path joiner. (2) It was blind to
// a module's API surface being encoded as string literals elsewhere (a JSON
// route table keyed by method name) -- see `deriveApiSurfaceReferences`
// below for that companion check. Whatever the next gap turns out to be, it
// will look like this: "my resolver models one way of referring to a thing,
// and the tree uses several." A specifier this resolver still cannot
// classify is never silently dropped either way -- see `unresolved` below.
//
// A LARGE `unresolved` COUNT ON A REAL RUN IS NOT NECESSARily THIS TOOL'S
// OWN DEFECT: this repo's own `tests/*.test.ts` root corpus carries a known,
// pre-existing, documented staleness (the "#937-class" `../src/...`-never-
// existed import path, from a prior reorganization) independent of whatever
// target is being audited. Those are genuinely unresolvable regardless of
// cause and are correctly surfaced here rather than silently dropped -- but
// their volume reflects that pre-existing corpus, not a new problem this
// audit created. `isLocallyShaped` below keeps the bucket meaningful by
// excluding bare package specifiers (`@chief/chiefing`, `bun:test`) that
// fail to resolve only because a sibling package's `dist/` has not been
// built yet -- a deletion target is always a local file, never a published
// package, so those can never be it and reporting them as `unresolved`
// would be noise, not signal.
//
// WHICH SIDE OF A DELETION TO RUN THIS ON: `deriveReferences` requires the
// target to physically exist (its whole design is "what would break if this
// were deleted"), so it is a PRE-DELETION check -- run it before deleting,
// or against a checkout/stash where the target is still present. Running it
// after the fact means restoring the file first; there is no "post-deletion"
// mode, and this is deliberate: after deletion there is no file left whose
// reference set could be derived, only informational mentions of a name
// that no longer resolves to anything.

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";
import ts from "typescript";

import { skipSet } from "./tree-walk-lib.mjs";

const EXCLUDED_DIRS = skipSet();

const CODE_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".cts", ".js", ".mjs"]);
const TEXT_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".rs", ".md", ".mdx"]);
const SURFACE_SEARCH_EXTENSIONS = new Set([
  ".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".rs", ".md", ".mdx",
  ".json", ".jsonc", ".yml", ".yaml",
]);

const NODE_BUILTINS = new Set([
  "assert", "buffer", "child_process", "crypto", "events", "fs", "http", "https",
  "net", "os", "path", "process", "stream", "timers", "url", "util", "worker_threads", "zlib",
]);

function isBuiltin(specifier) {
  return specifier.startsWith("node:") || NODE_BUILTINS.has(specifier);
}

function walkFiles(root, extensions) {
  const out = [];
  (function walk(dir) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        if (EXCLUDED_DIRS.has(entry.name)) continue;
        walk(join(dir, entry.name));
        continue;
      }
      if (!entry.isFile()) continue;
      if (extensions.has(extname(entry.name))) out.push(join(dir, entry.name));
    }
  })(root);
  return out;
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

/** Every workspace member (a directory matching the root package.json's
 *  `workspaces` globs that itself has a package.json), from source -- never
 *  a hardcoded list, so a new package's tsconfig is picked up automatically.
 *  Same instrument as `dep-declaration.mjs`'s `workspaceMembers`. */
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

/** A config is SOLUTION-STYLE when it names no input files of its own and
 *  exists only to point at other projects via `references`. Same instrument
 *  as `dep-declaration.mjs`'s `isSolutionStyleConfig` -- not a second
 *  implementation of the same idea. */
function isSolutionStyleConfig(config) {
  const noInclude = !Array.isArray(config.include) || config.include.length === 0;
  const noFiles = !Array.isArray(config.files) || config.files.length === 0;
  const hasRefs = Array.isArray(config.references) && config.references.length > 0;
  return noInclude && noFiles && hasRefs;
}

/** Every LEAF (non-solution-style) tsconfig reachable from a member's root
 *  tsconfig, each with its OWN parsed compilerOptions (including `paths`
 *  aliases like `@/*` -> `./src/*`) and resolved file set. A member with no
 *  tsconfig of its own falls back to the repo-wide `tsconfig.base.json`.
 *  Same instrument as `dep-declaration.mjs`'s `memberLeafConfigs`. */
function memberLeafConfigs(root, memberDir) {
  const leaves = [];
  const seen = new Set();
  function addLeaf(configPath) {
    const absConfigPath = resolve(configPath);
    if (seen.has(absConfigPath) || !existsSync(absConfigPath)) return;
    seen.add(absConfigPath);
    const { config, error } = ts.readConfigFile(absConfigPath, ts.sys.readFile);
    if (error) {
      throw new Error(`cannot read ${absConfigPath}: ${ts.flattenDiagnosticMessageText(error.messageText, " ")}`);
    }
    if (isSolutionStyleConfig(config)) {
      for (const reference of config.references ?? []) {
        const refPath = join(dirname(absConfigPath), reference.path);
        addLeaf(refPath.endsWith(".json") ? refPath : join(refPath, "tsconfig.json"));
      }
      return;
    }
    const parsed = ts.parseJsonConfigFileContent(config, ts.sys, dirname(absConfigPath));
    leaves.push({ options: parsed.options, fileNames: new Set(parsed.fileNames.map((f) => resolve(f))) });
  }
  addLeaf(join(root, memberDir, "tsconfig.json"));
  if (leaves.length === 0) {
    const { config } = ts.readConfigFile(join(root, "tsconfig.base.json"), ts.sys.readFile);
    const parsed = ts.parseJsonConfigFileContent(config, ts.sys, root);
    leaves.push({ options: parsed.options, fileNames: new Set() });
  }
  return leaves;
}

function optionsForFile(leaves, fileAbs) {
  for (const leaf of leaves) {
    if (leaf.fileNames.has(fileAbs)) return leaf.options;
  }
  return leaves[0].options;
}

/** Which workspace member (if any) owns a given absolute path -- longest
 *  prefix match. */
function ownerOfPath(root, members, absolutePath) {
  let best;
  for (const memberDir of members) {
    const memberAbs = resolve(root, memberDir);
    if (absolutePath === memberAbs || absolutePath.startsWith(memberAbs + "/")) {
      if (!best || memberAbs.length > best.absLength) best = { memberDir, absLength: memberAbs.length };
    }
  }
  return best?.memberDir;
}

/** Every import/export/require/dynamic-import specifier in a file, with its
 *  containing file -- real AST parsing, never a regex, so a string that
 *  only LOOKS like an import (a test description, a comment) is never
 *  mistaken for one. Every specifier is returned, not just relative ones:
 *  classifying `@/`-alias vs relative vs external is `ts.resolveModuleName`'s
 *  job, done against the importing file's REAL compiler options, never
 *  guessed from the specifier's own spelling. */
function importSpecifiersInFile(file) {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const specifiers = [];
  function visit(node) {
    let specifier;
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      specifier = node.moduleSpecifier.text;
    } else if (ts.isCallExpression(node)) {
      const isRequire = ts.isIdentifier(node.expression) && node.expression.text === "require";
      const isDynamicImport = node.expression.kind === ts.SyntaxKind.ImportKeyword;
      if ((isRequire || isDynamicImport) && node.arguments.length > 0 && ts.isStringLiteral(node.arguments[0])) {
        specifier = node.arguments[0].text;
      }
    }
    if (specifier) specifiers.push(specifier);
    ts.forEachChild(node, visit);
  }
  visit(source);
  return specifiers;
}

/** Resolves EVERY specifier style this repo actually uses (relative, `@/`
 *  tsconfig-path alias, bare workspace package name) through TypeScript's
 *  own resolver against the importing file's real compiler options -- the
 *  fix for the alias blind spot: a hand-rolled relative-path joiner can
 *  never see a `paths` remap, and `@/` is this repo's DOMINANT import style
 *  under `packages/*∕src`, not an edge case. Returns:
 *    - `{ kind: 'resolved', absolutePath }` -- lands on a real project file.
 *    - `{ kind: 'external' }` -- resolves into node_modules; out of scope
 *      for "does this file import the target" (the target is never in
 *      node_modules).
 *    - `{ kind: 'unresolved' }` -- TypeScript's own resolver could not
 *      classify it either. NEVER silently dropped by any caller -- see
 *      `deriveReferences`'s `unresolved` bucket. */
/** Whether a specifier is even SHAPED like something that could resolve to
 *  a workspace-local file: a relative path, or a prefix this file's own
 *  tsconfig `paths` declares an alias for (e.g. `@/*`). A deletion target
 *  is always a local file, never a published package, so a bare specifier
 *  matching neither shape (`@chief/chiefing`, `bun:test`, `lodash`) can
 *  never resolve to it regardless of whether `ts.resolveModuleName` can
 *  currently classify it (a common, entirely unrelated reason: sibling
 *  packages not yet built, so their `dist/` is absent) -- treating that as
 *  `external` rather than `unresolved` is what keeps the fail-closed
 *  `unresolved` bucket meaningful instead of drowned in noise from every
 *  unbuilt or genuinely external import in the whole repository. */
function isLocallyShaped(specifier, options) {
  if (specifier.startsWith("./") || specifier.startsWith("../")) return true;
  for (const prefix of Object.keys(options.paths ?? {})) {
    const stem = prefix.replace(/\*$/, "");
    if (specifier.startsWith(stem)) return true;
  }
  return false;
}

function resolveSpecifier(root, members, leafCache, fromFile, specifier) {
  if (isBuiltin(specifier)) return { kind: "external" };
  // A file outside every workspace member (e.g. this repo's own scripts/*.mjs)
  // still resolves against the repo-root tsconfig chain ("." as a pseudo
  // member) rather than being refused outright -- relative specifiers
  // resolve fine without any member-specific `paths`, and this repo's root
  // tsconfig.json is itself solution-style, recursing into every real
  // project's leaf configs via memberLeafConfigs's existing traversal.
  const memberDir = ownerOfPath(root, members, resolve(fromFile)) ?? ".";
  if (!leafCache.has(memberDir)) leafCache.set(memberDir, memberLeafConfigs(root, memberDir));
  const leaves = leafCache.get(memberDir);
  const options = optionsForFile(leaves, resolve(fromFile));
  const result = ts.resolveModuleName(specifier, fromFile, options, ts.sys);
  const resolved = result.resolvedModule;
  if (!resolved) {
    return isLocallyShaped(specifier, options) ? { kind: "unresolved" } : { kind: "external" };
  }
  if (resolved.isExternalLibraryImport) return { kind: "external" };
  return { kind: "resolved", absolutePath: resolve(resolved.resolvedFileName) };
}

/** A specifier is PLAUSIBLE for a given target stem when its own final path
 *  segment (its own "basename", stripped of extension) matches the stem --
 *  the same word-boundary discipline `deriveReferences`'s prose scan
 *  already uses, applied to the specifier's own text instead of a file's
 *  prose. Measured on the real repo (§ tool header): a fully built tree
 *  still carries ~1000 unresolved specifiers, almost entirely a
 *  pre-existing, already-documented corpus of stale `../src/...` imports in
 *  parked root `tests/*.test.ts` files (the "#937-class" staleness) --
 *  specifiers whose OWN final segment names some unrelated module, never
 *  this target. Failing a story's completeness check on every one of those
 *  regardless of relevance is a refusal so broad it invites exactly the
 *  bypass this program spent tonight documenting the cost of (the batch-
 *  merge.sh ledger-refusal incident, ruled on separately): a guard nobody
 *  can ever satisfy gets
 *  routed around rather than obeyed. Refusing ONLY on a plausible
 *  unresolved specifier keeps the refusal both fail-closed AND usable.
 *
 *  KNOWN RESIDUAL HOLE, NAMED RATHER THAN FIXED: a BARREL RE-EXPORT one hop
 *  away from an unresolved specifier is invisible to this heuristic. If
 *  `import { X } from '@/foo'` and `foo/index.ts` re-exports the target,
 *  but `'@/foo'` itself fails to resolve, this function checks `'@/foo'`
 *  (which names "foo") against the target's own stem -- never the true,
 *  one-hop-indirect name -- so a real reference through a broken barrel is
 *  both unresolved AND classified implausible, non-blocking by design. A
 *  heuristic over an already-unresolvable specifier cannot do better than
 *  a heuristic; this is stated so the next person extending this file
 *  knows the gap exists rather than assuming `plausible` is exhaustive. */
function isPlausibleForStem(specifier, stem) {
  const lastSegment = specifier.split("/").pop()?.replace(/\.(ts|tsx|mts|cts|js|mjs)$/, "");
  return lastSegment === stem;
}

/** The real reference set for a deletion target, split into load-bearing
 *  (a real resolved import edge, alias or relative), informational (a text
 *  mention of the target's basename), and unresolved (a specifier neither
 *  resolver arm could classify -- named explicitly, never dropped into
 *  either bucket, since silently absorbing an unresolvable specifier into
 *  "informational" is exactly the alias-blind-spot defect this rewrite
 *  fixes: a specifier this tool cannot classify is not evidence of
 *  anything, and must say so rather than imply "not load-bearing"). Every
 *  unresolved entry also carries `plausible` (see `isPlausibleForStem`) --
 *  `checkAgainstDeclared` only refuses on the plausible subset. */
export function deriveReferences(root, targetRelPath) {
  const targetAbs = resolve(root, targetRelPath);
  if (!existsSync(targetAbs)) {
    throw new Error(`deletion-scope-audit: target does not exist: ${targetRelPath} (already deleted? point this at the pre-deletion tree)`);
  }

  const stem = targetRelPath
    .split("/")
    .pop()
    .replace(/\.(ts|tsx|mts|cts|js|mjs)$/, "");

  const members = workspaceMembers(root);
  const leafCache = new Map();

  const loadBearing = [];
  const unresolved = [];
  for (const file of walkFiles(root, CODE_EXTENSIONS)) {
    if (resolve(file) === targetAbs) continue;
    let matched = false;
    for (const specifier of importSpecifiersInFile(file)) {
      const result = resolveSpecifier(root, members, leafCache, file, specifier);
      if (result.kind === "resolved" && result.absolutePath === targetAbs) {
        matched = true;
        break;
      }
      if (result.kind === "unresolved") {
        unresolved.push({ file: relative(root, file), specifier, plausible: isPlausibleForStem(specifier, stem) });
      }
    }
    if (matched) loadBearing.push(relative(root, file));
  }
  const loadBearingSet = new Set(loadBearing);

  const mentionPattern = new RegExp(`\\b${stem.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);

  const informational = [];
  let filteredNonWordBoundaryHits = 0;
  for (const file of walkFiles(root, TEXT_EXTENSIONS)) {
    const rel = relative(root, file);
    if (resolve(file) === targetAbs || loadBearingSet.has(rel)) continue;
    const text = readFileSync(file, "utf8");
    if (mentionPattern.test(text)) {
      informational.push(rel);
    } else if (text.includes(stem)) {
      // substring hit that isn't a real word (e.g. part of a longer
      // identifier) -- stated, not silently absorbed into either bucket.
      filteredNonWordBoundaryHits += 1;
    }
  }

  return {
    target: targetRelPath,
    loadBearing: loadBearing.sort(),
    informational: informational.sort(),
    unresolved: dedupeUnresolved(unresolved),
    filteredOut: {
      nonWordBoundarySubstringHits: filteredNonWordBoundaryHits,
    },
  };
}

function dedupeUnresolved(entries) {
  const seen = new Set();
  const out = [];
  for (const entry of entries) {
    const key = `${entry.file}\u0000${entry.specifier}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(entry);
  }
  return out.sort((a, b) => (a.file === b.file ? a.specifier.localeCompare(b.specifier) : a.file.localeCompare(b.file)));
}

/** Check a story's declared load-bearing file list against the derived
 *  ground truth. Only load-bearing omissions fail the check -- a story is
 *  never obligated to enumerate every informational mention, only never to
 *  claim completeness while missing a file the deletion will actually break.
 *
 *  A PLAUSIBLE unresolved specifier also fails the check (fail-closed): a
 *  story cannot be certified complete while this tool admits it could not
 *  tell whether one of the file's own imports pointed at the target, AND
 *  that specifier's own final path segment names the target. An
 *  IMPLAUSIBLE unresolved specifier (its own text names something else
 *  entirely) is reported but never blocks -- measured on the real repo,
 *  refusing on every unresolved specifier regardless of relevance stays
 *  non-empty even in a fully built tree (see `isPlausibleForStem`'s own
 *  doc comment), and an unconditional refusal that can never clear is a
 *  refusal that gets bypassed rather than obeyed, not a safer guard. */
export function checkAgainstDeclared(derived, declaredRelPaths) {
  const declared = new Set(declaredRelPaths);
  const missing = derived.loadBearing.filter((f) => !declared.has(f));
  const plausibleUnresolved = derived.unresolved.filter((u) => u.plausible);
  const implausibleUnresolved = derived.unresolved.filter((u) => !u.plausible);
  return {
    ok: missing.length === 0 && plausibleUnresolved.length === 0,
    missing,
    unresolved: plausibleUnresolved,
    implausibleUnresolved,
  };
}

const ROUTE_PATH_PATTERN = /^\/[a-z0-9][a-z0-9\-_/]*$/i;

/** Route-path string literals (e.g. `/v1/locks/acquire`) appearing directly
 *  in the target file's own source, via real AST parsing of every string
 *  literal node -- never a regex over raw text. */
function routePathLiteralsInFile(file) {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const paths = new Set();
  function visit(node) {
    if (ts.isStringLiteral(node) && ROUTE_PATH_PATTERN.test(node.text) && node.text.includes("/", 1)) {
      paths.add(node.text);
    }
    ts.forEachChild(node, visit);
  }
  visit(source);
  return [...paths];
}

/** Public method names of every exported class declared in the target
 *  file -- candidates for the `<key>.<method>` dot-notation this repo's
 *  route-dispatch tests/tables use (e.g. `locks.acquire`). */
function exportedClassMethodNames(file) {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const classes = [];
  for (const node of source.statements) {
    const isExported = ts.canHaveModifiers(node) && ts.getModifiers(node)?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword);
    if (!isExported || !ts.isClassDeclaration(node) || !node.name) continue;
    // flatMap, not filter().map(): `filter` does not narrow the element type,
    // so the `.map` step was reading `.name.text` off the unnarrowed member
    // union — the guard happened to be right at runtime and unprovable here.
    const methods = node.members.flatMap((m) =>
      ts.isMethodDeclaration(m) &&
      ts.isIdentifier(m.name) &&
      !ts.getModifiers(m)?.some((mod) => mod.kind === ts.SyntaxKind.PrivateKeyword)
        ? [m.name.text]
        : [],
    );
    classes.push({ className: node.name.text, methods });
  }
  return classes;
}

/** Where an exported class from the target file is MOUNTED elsewhere in the
 *  tree, e.g. `readonly locks: LocksClient` -- the property name (`locks`)
 *  is the dot-prefix this repo's route-dispatch convention keys on
 *  (`locks.acquire`). Heuristic (a property-declaration type-annotation
 *  scan), not a type-checker resolution -- a known limitation, stated
 *  rather than silently assumed exhaustive. */
function mountKeysForClass(root, className) {
  const keys = new Set();
  const pattern = new RegExp(`(?:readonly\\s+)?([a-zA-Z_$][\\w$]*)\\s*:\\s*${className}\\b`);
  for (const file of walkFiles(root, CODE_EXTENSIONS)) {
    const text = readFileSync(file, "utf8");
    const match = text.match(pattern);
    if (match) keys.add(match[1]);
  }
  return [...keys];
}

/** The deleted module's own API surface as string literals: its route-path
 *  literals, plus every `<mountKey>.<method>` dot-key a route-dispatch
 *  table/test might use to reference it. See this file's own header for
 *  the boundary this exists to cover and the boundary it still has. */
export function deriveApiSurfaceStrings(root, targetRelPath) {
  const targetAbs = resolve(root, targetRelPath);
  if (!existsSync(targetAbs)) {
    throw new Error(`deletion-scope-audit: target does not exist: ${targetRelPath} (already deleted? point this at the pre-deletion tree)`);
  }
  const routePaths = routePathLiteralsInFile(targetAbs);
  const dotKeys = [];
  for (const { className, methods } of exportedClassMethodNames(targetAbs)) {
    for (const mountKey of mountKeysForClass(root, className)) {
      for (const method of methods) dotKeys.push(`${mountKey}.${method}`);
    }
  }
  return { routePaths, dotKeys, candidates: [...routePaths, ...dotKeys] };
}

/** Search the WHOLE tree (source, docs, AND fixtures -- json/yaml included,
 *  which `deriveReferences`'s prose scan never covers) for an exact quoted
 *  occurrence of each candidate string. Quote-delimited match (never a bare
 *  substring) so `/v1/locks/acquire-legacy` cannot masquerade as a hit for
 *  `/v1/locks/acquire`; an un-quoted substring occurrence is filtered and
 *  the count stated, matching `deriveReferences`'s own filtered-count
 *  discipline. */
export function searchApiSurfaceStrings(root, targetRelPath, candidates) {
  const targetAbs = resolve(root, targetRelPath);
  const hits = [];
  let filteredUnquotedSubstringHits = 0;
  for (const file of walkFiles(root, SURFACE_SEARCH_EXTENSIONS)) {
    if (resolve(file) === targetAbs) continue;
    const text = readFileSync(file, "utf8");
    const rel = relative(root, file);
    for (const candidate of candidates) {
      const quoted = new RegExp(`["'\`]${candidate.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}["'\`]`);
      if (quoted.test(text)) {
        hits.push({ candidate, file: rel });
      } else if (text.includes(candidate)) {
        filteredUnquotedSubstringHits += 1;
      }
    }
  }
  return { hits, filteredOut: { unquotedSubstringHits: filteredUnquotedSubstringHits } };
}

/** Combines `deriveApiSurfaceStrings` + `searchApiSurfaceStrings` -- the
 *  companion check callers should actually run alongside `deriveReferences`. */
export function deriveApiSurfaceReferences(root, targetRelPath) {
  const surface = deriveApiSurfaceStrings(root, targetRelPath);
  if (surface.candidates.length === 0) {
    return { candidates: [], hits: [], filteredOut: { unquotedSubstringHits: 0 }, note: "no route-path literals or exported class methods found in this target -- this class of check does not apply to it" };
  }
  const searched = searchApiSurfaceStrings(root, targetRelPath, surface.candidates);
  return { candidates: surface.candidates, hits: searched.hits, filteredOut: searched.filteredOut };
}

async function main() {
  const [, , targetRelPath, ...rest] = process.argv;
  if (!targetRelPath) {
    console.error("usage: node scripts/deletion-scope-audit.mjs <target-relpath> [--against a.ts,b.ts]");
    process.exit(2);
  }
  const root = resolve(new URL("..", import.meta.url).pathname);
  const derived = deriveReferences(root, targetRelPath);
  console.log(`load-bearing (${derived.loadBearing.length}):`);
  for (const f of derived.loadBearing) console.log(`  ${f}`);
  console.log(`informational (${derived.informational.length}, not a compile/runtime break):`);
  for (const f of derived.informational) console.log(`  ${f}`);
  const plausible = derived.unresolved.filter((u) => u.plausible);
  const implausible = derived.unresolved.filter((u) => !u.plausible);
  if (plausible.length > 0) {
    console.log(`\nUNRESOLVED, PLAUSIBLE (${plausible.length}) -- this specifier's own text names this target, but resolution failed; NOT evidence the target is unused, and this blocks --against completeness:`);
    for (const u of plausible) console.log(`  ${u.file}  imports '${u.specifier}'`);
  }
  if (implausible.length > 0) {
    console.log(`\nunresolved, implausible (${implausible.length}, reported but does not block completeness -- this specifier's own text names something else):`);
    for (const u of implausible) console.log(`  ${u.file}  imports '${u.specifier}'`);
  }
  if (derived.filteredOut.nonWordBoundarySubstringHits > 0) {
    console.log(`filtered out: ${derived.filteredOut.nonWordBoundarySubstringHits} substring-only hit(s) (not a whole-word match)`);
  }

  const surface = deriveApiSurfaceReferences(root, targetRelPath);
  console.log(`\napi-surface string-literal hits (${surface.hits.length}, e.g. a route table keyed by method name):`);
  if (surface.note) console.log(`  ${surface.note}`);
  for (const h of surface.hits) console.log(`  ${h.file}  (matched "${h.candidate}")`);
  if (surface.filteredOut.unquotedSubstringHits > 0) {
    console.log(`filtered out: ${surface.filteredOut.unquotedSubstringHits} unquoted substring hit(s) (not an exact string-literal match)`);
  }
  console.log(
    "\nSCOPE: the counts above cover resolved import edges (relative AND tsconfig-path-alias, via " +
      "ts.resolveModuleName against each file's real compiler options), prose mentions of the filename, " +
      "and exact string-literal occurrences of this module's own route paths / <mountKey>.<method> dot-keys. " +
      "They do NOT cover: a numeric ID, a hashed key, or a string built by concatenation at runtime; a " +
      "PLAUSIBLE unresolved import specifier (named above, and refused by --against, but not proven load-bearing); " +
      "or a package-SUBPATH import into a SIBLING WORKSPACE package (e.g. `@chief/piing/extension-runtime`), " +
      "which resolves to that package's BUILT dist/ output and is therefore classified external -- it can never " +
      "be matched back to the source file it was built from, so a source-level target reached only this way " +
      "will never show as load-bearing even in a fully built tree; or a BARREL RE-EXPORT one hop away from an " +
      "unresolved specifier (import { X } from '@/foo' where foo/index.ts re-exports the target) -- if '@/foo' " +
      "itself fails to resolve, `plausible` checks '@/foo' against the target's stem, not the target's actual " +
      "name, so a real but indirect reference through a broken barrel is invisible AND non-blocking by design. " +
      "A zero above is not proof of a clean deletion, only proof against these specific reference shapes.",
  );

  const againstIdx = rest.indexOf("--against");
  if (againstIdx !== -1) {
    const declared = rest[againstIdx + 1].split(",").map((s) => s.trim()).filter(Boolean);
    const result = checkAgainstDeclared(derived, declared);
    if (!result.ok) {
      if (result.missing.length > 0) {
        console.error(`\nFAIL: story's declared list omits ${result.missing.length} load-bearing reference(s):`);
        for (const f of result.missing) console.error(`  ${f}`);
      }
      if (result.unresolved.length > 0) {
        console.error(`\nFAIL: ${result.unresolved.length} PLAUSIBLE specifier(s) (own text names this target) could not be resolved -- refusing to certify completeness:`);
        for (const u of result.unresolved) console.error(`  ${u.file}  imports '${u.specifier}'`);
      }
      process.exit(1);
    }
    if (result.implausibleUnresolved.length > 0) {
      console.log(`\nOK, with ${result.implausibleUnresolved.length} unrelated unresolved specifier(s) reported but not blocking (see above).`);
    } else {
      console.log("\nOK: declared list covers every load-bearing reference, and every specifier resolved.");
    }
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
