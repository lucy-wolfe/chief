// #963's class, applied to the last hand-maintained exemption list in this
// repo that had no stale-row check of its own.
//
// Three `packages/eslinter` rules take an `allowedPaths` option -- a list of
// path substrings where the thing the rule bans is sanctioned
// (`no-process-env`, `no-raw-null-check`, `no-json-stringify`). Every entry is
// written by hand into a package's `eslint.config.mjs`, and nothing has ever
// checked that the path it names is still there. An entry orphaned by a file
// move does not fail: it simply stops matching, which means the config now
// states a false fact about the tree AND re-arms silently the day a file
// appears at that path again. That is #963 verbatim -- a stale allowlist row a
// file move orphaned, invisible until batch assembly and then misattributed to
// an unrelated pin.
//
// WHY ONLY EXISTENCE, and not "the exempted file really does the banned
// thing". Answering the stronger question means running ESLint with the rule
// disabled and diffing, which costs a full lint of the workspace and makes
// this guard depend on the thing it polices. Existence is the check that
// catches the failure that has actually happened here, at the cost of a
// `statSync` per entry. Stated rather than quietly claimed.
//
// The other exemption registers in this repo already have this property and
// are deliberately NOT rewritten: `scripts/reactive-allowlist.ts` and
// `scripts/orphanable-spawner-lib.mjs` both fail on an entry that matches
// no live site (see `compareSitesToAllowlist`'s `stale` arm), and
// `scripts/test/typecheck-scope-gap.test.mjs` and `scripts/cargo-test-derive.mjs`
// each carry their own stale-row check. `apps/web/test/MandateFence.test.ts`
// got the property in the same change as this file, in its own package, where
// its rules live.

import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..");

/** Every `eslint.config.mjs` in the tree: the root one plus each workspace
 *  member's own. Derived from the root manifest's `workspaces` globs, never a
 *  path list -- a config list that has to be edited when a package is added is
 *  the same maintained-by-hand shape this guard exists to police. */
function eslintConfigPaths(root) {
  const found = [];
  const rootConfig = join(root, "eslint.config.mjs");
  if (existsSync(rootConfig)) found.push(rootConfig);
  const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  for (const glob of manifest.workspaces ?? []) {
    const base = String(glob).replace(/\/\*$/, "");
    const dir = join(root, base);
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir).sort()) {
      const config = join(dir, name, "eslint.config.mjs");
      if (existsSync(config)) found.push(config);
    }
  }
  return found;
}

/** Every `allowedPaths` entry across every eslint config, as
 *  `{ config, entry }`. A deliberate text scan: every `allowedPaths` in this
 *  repo is a literal array of string literals, and a scan that cannot see a
 *  computed one reports nothing for it rather than guessing -- which the
 *  non-vacuity arm below is what protects against. */
export function eslintAllowedPathEntries(root = REPO_ROOT) {
  const entries = [];
  for (const config of eslintConfigPaths(root)) {
    const text = readFileSync(config, "utf8");
    for (const array of text.matchAll(/allowedPaths:\s*\[([^\]]*)\]/g)) {
      for (const quoted of array[1].matchAll(/['"]([^'"]+)['"]/g)) {
        entries.push({ config: config.slice(root.length + 1), entry: quoted[1] });
      }
    }
  }
  return entries;
}

/** The entries naming nothing on disk. An entry is written repo-relative,
 *  sometimes with a leading slash (it is matched against a full file path at
 *  lint time), so the leading slash is stripped before resolving. */
export function staleAllowedPathEntries(root = REPO_ROOT) {
  return eslintAllowedPathEntries(root)
    .filter(({ entry }) => !statSync(join(root, entry.replace(/^\/+/, "")), { throwIfNoEntry: false }))
    .map(({ config, entry }) => `${config} -> ${entry}`);
}

test("every eslint allowedPaths entry still names something that exists", () => {
  assert.deepEqual(
    staleAllowedPathEntries(),
    [],
    "an `allowedPaths` entry names a path that is not in the tree. The rule it exempts is not being " +
      "relaxed for anything, so the config states a false fact about the code -- and it will silently " +
      "re-arm the day a file appears at that path again. Delete the entry, or fix the path if the file " +
      "moved.",
  );
});

test("NON-VACUITY: the scan actually found eslint configs and real allowedPaths entries", () => {
  const configs = eslintConfigPaths(REPO_ROOT);
  assert.ok(
    configs.length >= 4,
    `only ${configs.length} eslint.config.mjs file(s) found -- the workspaces walk has collapsed, and an ` +
      `empty stale list from a scan that read nothing is not evidence about the configs`,
  );
  const entries = eslintAllowedPathEntries();
  assert.ok(
    entries.length >= 3,
    `only ${entries.length} allowedPaths entr(ies) parsed out of ${configs.length} config(s) -- the ` +
      `option's spelling or formatting has changed and this scan can no longer see its subject`,
  );
});

test("DETECTION, both directions: a moved path is named, a live one is not", () => {
  // Against the REAL tree, not a fixture: a checker that cannot resolve a real
  // repo path would pass the stale arm by reporting everything, and one that
  // resolves nothing would pass the live arm by reporting nothing.
  const live = { config: "probe", entry: "/packages/eslinter/rules/no-process-env.js" };
  const moved = { config: "probe", entry: "/packages/eslinter/rules/a-rule-that-was-moved.js" };
  const resolveEntry = ({ entry }) =>
    Boolean(statSync(join(REPO_ROOT, entry.replace(/^\/+/, "")), { throwIfNoEntry: false }));

  assert.equal(resolveEntry(live), true, "a real repo path must resolve, or this guard reports every entry stale");
  assert.equal(resolveEntry(moved), false, "a path that is not there must not resolve, or this guard reports nothing ever");
});
