// #939: turbo's default envMode is STRICT -- any env var a task's own code
// reads from its inherited environment but does not declare in that task's
// `env`/`passThroughEnv` is silently STRIPPED before the child process even
// starts. No warning, no log line: the value is just absent. Two real,
// independent incidents on the same day (a segfault chasing a missing
// `CARGO_TARGET_DIR`, unrelated-looking `@chief/testing` noise from a
// different seat) turned out to share this one cause.
//
// A hand-written env list is exactly the failure mode this file exists to
// replace: #939's own fix started as "declare CARGO_TARGET_DIR" and grew to
// three more real, previously-invisible gaps (PI_SOURCE_AGENT_DIR, TMPDIR,
// and a package-specific task override that doesn't inherit the generic
// task's declarations at all) the moment the derivation was done from
// source instead of from the one incident that surfaced it. This script IS
// that derivation, run permanently rather than once: for `test:unit` (and
// any package-specific `<pkg>#test:unit` override, which REPLACES the
// generic entry rather than merging with it -- turbo's own semantics, not
// this file's), it greps every in-scope package's `src/**/*.ts` (never
// `test/**` -- test files routinely construct their own scoped environment
// objects rather than reading the inherited one, which would make this
// check noisy with false positives it cannot resolve statically) for
// `process.env.X` and asserts every distinct `X` found is declared for the
// effective task key that governs that package.
//
// Deliberately scoped to `test:unit`: it is the only task whose CHILD
// PROCESS actually EXECUTES application source (vitest imports and runs
// it). `build`/`lint`/`format` invoke tsc/eslint/prettier, which parse
// source as text and never import or execute it -- a `process.env.X` read
// inside `src/` is inert to those tasks regardless of whether the var
// reaches the child process. `dev`/`start` DO execute source (their env
// arrays are empty, `[]`, by original design pending real needs) but are
// out of scope for this pass -- flagged, not silently declared covered.
//
// KNOWN LIMITATION, disclosed rather than silently claimed covered: this
// scanner resolves `process.env.X` / `process.env["X"]` literally, resolves
// a handful of hand-traced dynamic sites via KNOWN_DYNAMIC_READS, and (#943)
// resolves the LOCAL alias class -- a parameter default
// (`environment: T = process.env`) or a same-scope `const environment =
// options.environment ?? process.env` fallback -- by finding every `.PROP`
// read off that alias name within its own enclosing function. That closed
// the exact site #939 found and traced by hand (`ORG_LAUNCHER_ROOT`,
// `attach-wiring.ts`'s `chiefdLauncherRoot`), plus every other
// same-function instance of the same shape, without a second parsing
// strategy or any cross-file value tracing.
//
// STILL NOT CLOSED, NAMED SEPARATELY: `process.env` passed as a plain CALL
// ARGUMENT to a function defined elsewhere that reads `.PROP` off its own
// parameter (e.g. `defaultChiefdBinaryPath(process.env)`, where that
// function's parameter is read internally, in a different file, with no
// textual `= process.env` tying the two together at the read site) is a
// real points-to problem, not an AST-adjacent one -- #939's own header
// already rejected a partial version of that as worse than none, because it
// would imply coverage it lacks. This file does not attempt it.
//
// A short, deliberately explicit allowlist covers vars turbo passes through
// by default regardless of a task's own declarations, confirmed
// EMPIRICALLY (a probe test logging each var under `turbo run test:unit`),
// not assumed from turbo's docs, which do not enumerate this list
// precisely for the installed version. `TMPDIR` was probed and is NOT on
// this list, despite being an OS-standard variable -- this is exactly the
// kind of assumption this file exists to never make from a name.

import { readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import ts from "typescript";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, "..");

// #945: this file used to hold a `DEFAULT_PASSTHROUGH_ENV` set of
// {CI, HOME, PATH, LANG}, exempted from declaration on the strength of a
// probe test logging each var with nothing declared and observing the
// value still present in the child process. THAT PROBE PROVED VISIBILITY.
// It said nothing about whether turbo's cache key hashes the value -- and
// it does not. Demonstrated live (#944/#945): `CI` unset and `CI=1` against
// an unchanged tree produced the SAME cache entry -- a run computed with
// tests skipped (`CI` unset -> `chiefdBinaryTestGate` skips rather than
// throws) was replayed as the result for `CI=1`, and the formerly-skipped
// tests never executed a second time despite the flag that should force
// them. `HOME` and `PATH` were re-tested the same way once `CI` failed:
// changing either, undeclared, also produced a cache HIT -- identical
// blindness, just never exercised by an incident yet. `LANG` was checked
// for a different reason (locale-sensitive formatting via `toLocaleString`/
// `Intl` appears in several files this program's tests import) rather than
// a hashing probe, since no code reads `process.env.LANG` literally.
//
// The fix is not a corrected exemption list -- it is that NO EXEMPTION SET
// EXISTS ANYMORE. Every var this scanner finds must be declared in `env` or
// `passThroughEnv`, with no third option that skips the question. `CI` and
// `HOME` are now declared in turbo.json (real literal reads); `PATH` and
// `LANG` are declared too, on the "ambiguous -> the safe bucket" rule from
// the same asymmetry this file's header describes elsewhere -- a real
// literal read is not required to justify caution when a var can influence
// child-process resolution (`PATH`) or locale-formatted output (`LANG`).
// A future var visible-but-unhashed by turbo's own internals is exactly
// the shape that must fail this scanner, not be quietly exempted from it a
// second time.

export function readTurboJson(turboJsonPath = join(REPO_ROOT, "turbo.json")) {
  const { config, error } = ts.readConfigFile(resolve(turboJsonPath), ts.sys.readFile);
  if (error) {
    throw new Error(`cannot read ${turboJsonPath}: ${ts.flattenDiagnosticMessageText(error.messageText, " ")}`);
  }
  return config ?? {};
}

// Every workspace member with a package.json, derived from disk -- not
// transcribed from `package.json`'s `workspaces` glob, which only says
// where to look, not what actually exists there today.
export function discoverPackages(root = REPO_ROOT) {
  const packages = [];
  for (const group of ["apps", "packages"]) {
    const groupDir = join(root, group);
    let entries;
    try {
      entries = readdirSync(groupDir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const dir = join(groupDir, entry.name);
      let pkgJson;
      try {
        pkgJson = JSON.parse(ts.sys.readFile(join(dir, "package.json")) ?? "");
      } catch {
        continue;
      }
      if (pkgJson.name) packages.push({ name: pkgJson.name, dir });
    }
  }
  return packages;
}

function walkTsFiles(dir, out = []) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    if (entry.name === "node_modules" || entry.name === "dist" || entry.name === ".git") continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      walkTsFiles(path, out);
    } else if (entry.name.endsWith(".ts") || entry.name.endsWith(".tsx")) {
      out.push(path);
    }
  }
  return out;
}

// #939: a text/regex scan cannot tell a real `process.env.X` READ from the
// identical text inside a `//` or `/** */` comment -- the first version of
// this file matched `CHIEFD_RUN_STATE_DIR` and `ORG_LAUNCHER_ROOT` purely
// because a JSDoc comment happened to say `process.env.CHIEFD_RUN_STATE_DIR`
// while explaining why the code does NOT read it that way. Both were false
// positives: real vars in the declared list that no code anywhere reads.
// This is now real AST parsing (`ts.createSourceFile` + a node walk), which
// tokenizes comments as trivia rather than code -- the same reason
// `readTsconfig` above defers to the compiler instead of a hand-rolled
// JSONC strip.
function isFunctionLikeNode(node) {
  return (
    ts.isFunctionDeclaration(node) ||
    ts.isFunctionExpression(node) ||
    ts.isArrowFunction(node) ||
    ts.isMethodDeclaration(node) ||
    ts.isConstructorDeclaration(node)
  );
}

// #943: closes the LOCAL half of the alias class #939 disclosed rather than
// traced -- a parameter default (`environment: T = process.env`) or a local
// variable fallback (`const environment = options.environment ?? process.env`)
// binds `process.env` to a new name WITHIN THE SAME FUNCTION, and every
// `PROP` read off that name in that same scope is exactly as real an
// ambient dependency as `process.env.PROP` written literally -- it just
// needs one extra AST step (find the alias's declaration, confirm its
// value IS `process.env`, then scan its own enclosing function for reads
// off that name) rather than a second parsing strategy. This is NOT a
// general points-to analysis: the alias's SOURCE (a default value or a
// `??`/`||` fallback) and its READ SITE are both resolved within one
// scope, with no value threaded through an intermediate variable, a
// re-export, or a call argument -- sound by construction, not a heuristic
// that could silently under- or over-count.
//
// STILL NOT CLOSED, AND STATED SEPARATELY (never implied covered by this
// fix): `process.env` passed as a plain CALL ARGUMENT to a function
// defined elsewhere that reads `.PROP` off its own parameter (e.g.
// `defaultChiefdBinaryPath(process.env)` where that function's own
// parameter is read internally) is a cross-file/cross-call-site case this
// still cannot see -- resolving THAT soundly needs the real points-to
// analysis #939's own header already named and rejected as
// disproportionate ("a partial version... is worse than none, because it
// would imply coverage it lacks"). This fix closes the shape #939's
// `chiefdLauncherRoot` example actually was (a same-function default
// parameter) without claiming the harder shape too.
function collectAliasPropertyReads(scopeNode, aliasName, out) {
  function visit(node) {
    if (ts.isPropertyAccessExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === aliasName) {
      out.push(node.name.text);
    } else if (ts.isElementAccessExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === aliasName) {
      if (ts.isStringLiteralLike(node.argumentExpression)) out.push(node.argumentExpression.text);
    }
    ts.forEachChild(node, visit);
  }
  visit(scopeNode);
}

function collectProcessEnvAccesses(sourceFile) {
  const literalReads = [];
  const dynamicSites = [];
  function isProcessEnv(node) {
    return (
      ts.isPropertyAccessExpression(node) &&
      ts.isIdentifier(node.expression) &&
      node.expression.text === "process" &&
      node.name.text === "env"
    );
  }
  function visit(node) {
    if (ts.isPropertyAccessExpression(node) && isProcessEnv(node.expression)) {
      literalReads.push(node.name.text);
    } else if (ts.isElementAccessExpression(node) && isProcessEnv(node.expression)) {
      if (ts.isStringLiteralLike(node.argumentExpression)) {
        literalReads.push(node.argumentExpression.text);
      } else {
        dynamicSites.push(node.getStart(sourceFile));
      }
    } else if (ts.isParameter(node) && node.initializer && isProcessEnv(node.initializer) && ts.isIdentifier(node.name)) {
      // `environment: T = process.env` -- when the default fires, `environment`
      // IS process.env for the rest of this function's body.
      const scope = ts.findAncestor(node, isFunctionLikeNode) ?? sourceFile;
      collectAliasPropertyReads(scope, node.name.text, literalReads);
    } else if (
      ts.isVariableDeclaration(node) &&
      node.initializer &&
      ts.isBinaryExpression(node.initializer) &&
      (node.initializer.operatorToken.kind === ts.SyntaxKind.QuestionQuestionToken ||
        node.initializer.operatorToken.kind === ts.SyntaxKind.BarBarToken) &&
      isProcessEnv(node.initializer.right) &&
      ts.isIdentifier(node.name)
    ) {
      // `const environment = options.environment ?? process.env` -- when the
      // fallback fires, `environment` IS process.env for the rest of this
      // function's body. The LEFT side (`options.environment`) is a real,
      // separately-caller-suppliable value this scanner cannot trace (that
      // IS the cross-call-site case named above) -- only the right side's
      // guaranteed `process.env` identity is being credited here.
      const scope = ts.findAncestor(node, isFunctionLikeNode) ?? sourceFile;
      collectAliasPropertyReads(scope, node.name.text, literalReads);
    }
    ts.forEachChild(node, visit);
  }
  visit(sourceFile);
  return { literalReads, dynamicSites };
}

// Every `process.env[...]` site in `src/**` found and hand-resolved to its
// literal name(s) during #939's derivation, keyed by `file:line` (1-indexed,
// relative to repo root) so a moved or edited line re-triggers review rather
// than silently keeping a stale resolution. Both directions are checked:
// a dynamic-read site with no entry here fails loud (new, unresolved); an
// entry whose file:line no longer contains a dynamic read fails loud too
// (stale -- the resolution has drifted from the code it was resolved
// against). This is a resolution record, not an allowlist: it does not
// exempt anything from declaration, it only lets the scanner credit a
// dynamic read with the name(s) a human already traced it to.
/**
 * Annotated rather than left to inference. The record was EMPTY for a while,
 * so its inferred type was `{}` and every caller could pass any shape; the
 * first real row made the inferred type demand that exact key of every
 * argument, and the guards typecheck leg went red on this file's own tests.
 * The record's contract is `file:line -> the literal names it resolves to`,
 * and stating it keeps that true whether it holds nought rows or nine.
 *
 * @type {Record<string, string[]>}
 */
export const KNOWN_DYNAMIC_READS = {
  // #945-followup: relocated by #817's cli.ts rewrite (was 1024/1163;
  // same LAUNCHER_PANE_IDENTITY_ENV_KEYS array, same two call sites,
  // verified by reading the code at the new lines, not by arithmetic on
  // the old ones -- a merge can move a resolved site without changing
  // what it resolves to, and the symmetric staleness check is what
  // caught the old line numbers no longer matching real code.
  // #751/P0: DELETED, not re-lined. `apps/cli/src/legacy` is gone in full,
  // so both sites and the `LAUNCHER_PANE_IDENTITY_ENV_KEYS` array they read
  // no longer exist. A recorded resolution whose file cannot be opened is a
  // claim about code that is not there — Mandate 0 removes it rather than
  // repointing it at some surviving caller that never had these reads.
  // #751/E4: the `org-log.ts:139` (ORG_LOG_MAX_BYTES/ORG_LOG_HEARTBEAT_MS)
  // and `org-sse-rollout.ts:58` (ORG_SSE_DISABLED) resolutions are DELETED,
  // not re-lined. Both files are gone — `apps/cli/src/legacy/organization/`
  // now holds only company-files.ts and managed-pane-observation.ts, the
  // rest having been ported into chiefd — so there is no dynamic read left
  // to resolve and nowhere to point the record. This is the shrink direction
  // the symmetric staleness check above exists to force: a resolution record
  // that only ever grows is exactly the rot #925's stale exception was.
  // #983: DELETED, not re-lined. `DocstoreGlobalSetup.ts` used to
  // save-then-restore the vars it exported the daemon URL under, defaulting
  // to the retired pane env stamp. It exports nothing now -- the URL travels
  // through vitest's `provide()` -- so there is no dynamic read at that line
  // or any other in the file. All FOUR records go together -- the export
  // loop and the restore loop were one mechanism, and leaving any of them
  // would be the stale row this check exists to catch.
  //
  // #1052's `apps/web/src/common/Env.ts` `credentialVariable` dereference is
  // DELETED too, and for the same reason: chief is out of the provider/model
  // business, so nothing in that package resolves a registry-named `$VAR` any
  // more and the record describes no code.
};

// Only `src/**` -- see the file header for why `test/**` is deliberately
// excluded (it manufactures its own scoped env objects; a static scan
// cannot tell "reads the inherited var" from "sets a local one of the same
// name to hand to a spawned child").
export function deriveEnvReadsForPackage(pkgDir, root = REPO_ROOT) {
  const srcDir = join(pkgDir, "src");
  const reads = new Set();
  const unresolvedDynamicSites = [];
  const seenKnownSites = new Set();
  for (const file of walkTsFiles(srcDir)) {
    const text = ts.sys.readFile(file) ?? "";
    const sourceFile = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
    const { literalReads, dynamicSites } = collectProcessEnvAccesses(sourceFile);
    for (const name of literalReads) reads.add(name);
    const relativePath = file.slice(root.length + 1);
    for (const position of dynamicSites) {
      const { line } = sourceFile.getLineAndCharacterOfPosition(position);
      const site = `${relativePath}:${line + 1}`;
      const resolved = KNOWN_DYNAMIC_READS[site];
      if (resolved) {
        seenKnownSites.add(site);
        for (const name of resolved) reads.add(name);
      } else {
        unresolvedDynamicSites.push(site);
      }
    }
  }
  return { reads, unresolvedDynamicSites, seenKnownSites };
}

// The effective declared set for a package's `test:unit` execution.
// `<pkg>#test:unit` REPLACES `test:unit` for that package if present --
// turbo's own resolution rule, not something this file merges on its
// behalf; merging here would hide exactly the class of gap #939 found in
// `@chief/chiefing#test:unit`.
export function effectiveDeclaredEnv(turboConfig, packageName) {
  const scoped = turboConfig.tasks?.[`${packageName}#test:unit`];
  const task = scoped ?? turboConfig.tasks?.["test:unit"];
  const declared = new Set([...(task?.env ?? []), ...(task?.passThroughEnv ?? [])]);
  return { declared, usedScopedOverride: Boolean(scoped), task };
}

// Returns one entry per package with an undeclared, actually-read env var.
// Empty array = every var each package's `src/` reads during `test:unit`
// is either declared for the task key that actually governs that package,
// or covered by turbo's confirmed default passthrough.
export function findUndeclaredTestUnitEnv(
  turboConfig = readTurboJson(),
  root = REPO_ROOT,
  // #983: the resolution record reached EMPTY for the first time, and that
  // exposed a real weakness in the self-test that proves this staleness check
  // works: it drove the proof from the LIVE record, so an empty record made
  // the proof vacuous exactly when there was nothing left to catch. The record
  // is now injectable so the mechanism can be exercised against a synthetic
  // one. Production still passes nothing and gets the live record.
  knownDynamicReads = KNOWN_DYNAMIC_READS
) {
  const packages = discoverPackages(root);
  if (packages.length === 0) {
    throw new Error("discoverPackages resolved zero workspace members -- apps/*/package.json or packages/*/package.json probably moved (#939 vacuity)");
  }
  const problems = [];
  const allUnresolvedDynamicSites = [];
  const allSeenKnownSites = new Set();
  for (const { name, dir } of packages) {
    const { reads, unresolvedDynamicSites, seenKnownSites } = deriveEnvReadsForPackage(dir, root);
    allUnresolvedDynamicSites.push(...unresolvedDynamicSites);
    for (const site of seenKnownSites) allSeenKnownSites.add(site);
    if (reads.size === 0) continue;
    const { declared, usedScopedOverride } = effectiveDeclaredEnv(turboConfig, name);
    const undeclared = [...reads].filter((name_) => !declared.has(name_));
    if (undeclared.length > 0) {
      problems.push({ package: name, undeclared: undeclared.sort(), usedScopedOverride });
    }
  }
  // Symmetric check on the resolution record itself: a `KNOWN_DYNAMIC_READS`
  // entry whose file:line no longer contains a dynamic read is stale --
  // the code moved or was rewritten since the resolution was made, and the
  // record no longer describes anything real. A resolution record that only
  // ever grows, never shrinks, is the same rot #925's stale exception was.
  const staleKnownSites = Object.keys(knownDynamicReads).filter((site) => !allSeenKnownSites.has(site));
  return { problems, unresolvedDynamicSites: allUnresolvedDynamicSites.sort(), staleKnownSites: staleKnownSites.sort() };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { problems, unresolvedDynamicSites, staleKnownSites } = findUndeclaredTestUnitEnv();
  let refuse = false;
  if (unresolvedDynamicSites.length > 0) {
    refuse = true;
    console.error("[turbo-env-audit] REFUSING TO RUN — unresolved process.env[dynamic] read(s), not a zero:");
    for (const site of unresolvedDynamicSites) console.error(`  - ${site}: resolve the key by hand and add it to KNOWN_DYNAMIC_READS`);
  }
  if (staleKnownSites.length > 0) {
    refuse = true;
    console.error("[turbo-env-audit] REFUSING TO RUN — KNOWN_DYNAMIC_READS entry no longer matches a real dynamic read:");
    for (const site of staleKnownSites) console.error(`  - ${site}: the code moved or changed; re-resolve and update the record`);
  }
  if (problems.length > 0) {
    refuse = true;
    console.error("[turbo-env-audit] REFUSING TO RUN — test:unit would silently strip real env reads:");
    for (const problem of problems) {
      const via = problem.usedScopedOverride ? `${problem.package}#test:unit` : "test:unit";
      console.error(`  - ${problem.package} (governed by '${via}'): ${problem.undeclared.join(", ")}`);
    }
  }
  if (refuse) process.exit(1);
  console.log("[turbo-env-audit] every process.env read (literal and resolved-dynamic) under a package's src/ is declared for the task key that governs its test:unit — not vacuous");
}
