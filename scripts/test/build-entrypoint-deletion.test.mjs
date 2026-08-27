// #1041: a `build` script may not delete the directory its own package
// entrypoints live in.
//
// THE INCIDENT. `@chief/chiefing`'s `ContractSuiteResidual.test.ts` failed
// with "Failed to resolve entry for package @chief/testing" -- twice, for two
// different agents, under parallel gate load. It reads like flakiness and it
// is not. `@chief/testing`'s build script began `rimraf dist && rimraf
// tsconfig.build.tsbuildinfo && tsc -p ... && tsc-alias -p ...`, so a
// rebuild DELETED the package's only entrypoint and then took a full `tsc`
// compile to put it back. Measured on the build host, sampling every 10ms
// through one real `turbo run build --filter=@chief/testing --force`:
// `packages/testing/dist/index.js` did not exist for 98 consecutive samples,
// about 1.3 SECONDS. Any process resolving `@chief/testing` in that window
// gets exactly that error message.
//
// THE EDGE WAS NEVER MISSING. `packages/chiefing/package.json` declares
// `@chief/testing: workspace:*`, and `turbo.json`'s
// `@chief/chiefing#test:unit` declares `dependsOn: ["build", "^build"]`, so
// within ONE `turbo run` the ordering is correct and honoured. The defect is
// that the window is visible to every OTHER process in the tree: `bun run
// typecheck` runs its own `turbo run build --filter='./packages/*'`,
// `postinstall` runs another, and the standing pre-push checklist is six
// gates that an engineer under time pressure runs at the same time. Turbo's
// task graph orders tasks inside one invocation; it cannot order a task
// against a process it does not own. A build that is destructive before it
// is productive exports that ordering problem to everyone.
//
// WHY THE FIX WAS DELETION, NOT AN ATOMIC SWAP OR A RETRY. A retry was
// rejected outright: it hides an ordering bug and teaches the fleet to re-run
// reds instead of reading them. What `rimraf dist` bought was removal of
// orphaned output whose source had been deleted -- and it did not actually
// buy that, which is the finding that settled it. Demonstrated on the build
// host: dropping a junk `dist/ORPHAN.js` into `packages/testing` and running
// `turbo run build` produced `>>> FULL TURBO`, a cache hit, and the orphan
// survived it untouched. Turbo does not prune outputs before restoring them,
// so "a clean dist" was never a property this build actually had; every
// cache hit could already reintroduce the exact class `rimraf` existed to
// prevent. Weighed against a reproducible 1.3-second entryless window on
// every cache MISS, the destructive step was paying a real cost for a
// property it did not deliver. `rimraf tsconfig.build.tsbuildinfo` STAYS: it
// forces a full re-emit, so every live output is rewritten in place, and
// `dist` now goes from complete to complete without passing through absent.
//
// WHAT THIS GUARD ASSERTS, derived rather than listed: for every workspace
// member with a `build` script, the destructive commands in that script may
// not name any directory its own `package.json` entrypoints (`exports` /
// `main`) resolve under. Keyed on the entrypoint declaration, not on a list
// of package names or a ban on `rimraf`: deleting a build stamp is fine,
// deleting the thing dependents resolve is not, and the difference is
// exactly what the manifest already says.

import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..");

/** Every path an `exports` map (or a bare `main`) points at -- the same walk
 *  `scripts/test/assert-workspace-built.mjs` uses to decide whether a package
 *  is built. Deliberately the same source of truth: the paths that guard
 *  checks for EXISTENCE are precisely the paths a build must not remove. */
function entryPointsOf(manifest) {
  const entries = [];
  const walkNode = (node) => {
    if (typeof node === "string") {
      if (node.startsWith("./")) entries.push(node);
      return;
    }
    if (node !== null && typeof node === "object") {
      for (const value of Object.values(node)) walkNode(value);
    }
  };
  walkNode(manifest.exports ?? null);
  if (typeof manifest.main === "string") entries.push(manifest.main);
  return [...new Set(entries)];
}

/** The top directory each entrypoint lives under (`./dist/index.js` ->
 *  `dist`). An entrypoint committed at the package root (`./index.js`)
 *  contributes nothing: there is no directory to delete. */
function entryRoots(manifest) {
  const roots = new Set();
  for (const entry of entryPointsOf(manifest)) {
    const segments = entry.replace(/^\.\//, "").split("/");
    if (segments.length > 1 && segments[0] !== "") roots.add(segments[0]);
  }
  return roots;
}

/** Every path a destructive command in `script` names. Covers the two forms
 *  this repo's build scripts use or could plausibly grow (`rimraf <path>`,
 *  `rm -rf <path>`); a form it cannot see reports nothing, which the
 *  non-vacuity arm below is what protects against. */
function deletedPaths(script) {
  const paths = [];
  for (const command of script.split(/&&|;|\|\|/)) {
    const rimraf = /(?:^|\s)rimraf\s+(.+)$/.exec(command.trim());
    if (rimraf) {
      for (const arg of rimraf[1].trim().split(/\s+/)) paths.push(arg);
      continue;
    }
    const rm = /(?:^|\s)rm\s+(?:-[a-zA-Z]+\s+)*(.+)$/.exec(command.trim());
    if (rm) {
      for (const arg of rm[1].trim().split(/\s+/)) {
        if (!arg.startsWith("-")) paths.push(arg);
      }
    }
  }
  return paths.map((path) => path.replace(/^\.\//, "").replace(/\/+$/, ""));
}

/** `{ member, script, deleted, root }` for every build script that deletes a
 *  directory its own entrypoints resolve under. Empty is the passing answer. */
function entrypointDeletions(root) {
  const findings = [];
  const rootManifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  for (const glob of rootManifest.workspaces ?? []) {
    const base = String(glob).replace(/\/\*$/, "");
    const dir = join(root, base);
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir)) {
      const manifestPath = join(dir, name, "package.json");
      if (!existsSync(manifestPath)) continue;
      const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
      const script = manifest.scripts?.build;
      if (typeof script !== "string") continue;
      const roots = entryRoots(manifest);
      for (const deleted of deletedPaths(script)) {
        const top = deleted.split("/")[0];
        if (roots.has(top)) {
          findings.push({ member: `${base}/${name}`, script, deleted, root: top });
        }
      }
    }
  }
  return findings;
}

/** The population this scan actually reached, so an empty findings list from
 *  a scan that walked nothing cannot be read as good news. */
function scannedBuildScripts(root) {
  let count = 0;
  const rootManifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  for (const glob of rootManifest.workspaces ?? []) {
    const base = String(glob).replace(/\/\*$/, "");
    const dir = join(root, base);
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir)) {
      const manifestPath = join(dir, name, "package.json");
      if (!existsSync(manifestPath)) continue;
      const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
      if (typeof manifest.scripts?.build === "string" && entryRoots(manifest).size > 0) count += 1;
    }
  }
  return count;
}

test("no workspace package's build script deletes the directory its own entrypoints live in", () => {
  const findings = entrypointDeletions(REPO_ROOT);
  assert.deepEqual(
    findings.map((f) => `${f.member}: deletes '${f.deleted}', which its own package.json exports resolve under`),
    [],
    "a build script removes the entrypoint dependents resolve, and then takes a full compile to put it " +
      "back. Every process outside the turbo invocation that owns the task -- `bun run typecheck`'s own " +
      "`turbo run build`, `postinstall`, a second gate running in parallel -- sees the package as " +
      "unresolvable for the whole window (measured: ~1.3s for @chief/testing). Turbo orders tasks inside " +
      "one run; it cannot order a task against a process it does not own.",
  );
});

test("NON-VACUITY: the scan actually reached real build scripts with real entrypoints", () => {
  const scanned = scannedBuildScripts(REPO_ROOT);
  assert.ok(
    scanned >= 3,
    `only ${scanned} workspace package(s) with both a build script and a declared entrypoint were found -- ` +
      `the workspaces walk or the manifest parse has collapsed, and an empty findings list from a scan that ` +
      `read nothing is the silent green this arm refuses`,
  );
});

test("DETECTION, both directions: the exact incident shape is flagged, and deleting a build stamp is not", () => {
  // Reconstructs @chief/testing's manifest as it was when the failure was
  // reported, and as it is now -- same package, same script minus one
  // command, so nothing but the destructive step differs between the arms.
  const exports = { ".": { types: "./dist/index.d.ts", import: "./dist/index.js" } };
  const tail = "rimraf tsconfig.build.tsbuildinfo && tsc -p tsconfig.build.json && tsc-alias -p tsconfig.build.json --resolve-full-paths";

  const before = { exports, scripts: { build: `rimraf dist && ${tail}` } };
  const beforeRoots = entryRoots(before);
  const beforeHits = deletedPaths(before.scripts.build).filter((p) => beforeRoots.has(p.split("/")[0]));
  assert.deepEqual(
    beforeHits,
    ["dist"],
    "the pre-fix @chief/testing build script must be caught -- if it is not, this guard could not have " +
      "seen the defect it was written for",
  );

  const after = { exports, scripts: { build: tail } };
  const afterRoots = entryRoots(after);
  const afterHits = deletedPaths(after.scripts.build).filter((p) => afterRoots.has(p.split("/")[0]));
  assert.deepEqual(
    afterHits,
    [],
    "the current script still deletes `tsconfig.build.tsbuildinfo`, which is deliberate (it forces a full " +
      "re-emit so every live output is rewritten in place). A guard that flagged every `rimraf` would fail " +
      "here, and would be a ban on a command rather than a rule about entrypoints",
  );

  // A package whose entrypoint is committed at its own root has no directory
  // to delete, and must not be reported for deleting an unrelated one.
  const rootEntry = { main: "./index.js", scripts: { build: "rimraf dist && tsc" } };
  assert.deepEqual(
    deletedPaths(rootEntry.scripts.build).filter((p) => entryRoots(rootEntry).has(p.split("/")[0])),
    [],
    "a package that publishes ./index.js from its own root declares no entrypoint DIRECTORY, so deleting " +
      "`dist` cannot remove one -- reporting it would be this guard flagging the command rather than the harm",
  );
});
