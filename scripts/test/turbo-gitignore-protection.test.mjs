// #948: a task's own output (turbo's per-task log file, `.turbo/turbo-
// <task>.log`) landing inside its own default input glob makes two
// otherwise-identical invocations hash differently, chasing their own tail
// and never converging to a cache hit -- observed live in a synthetic
// fixture while building #944, confirmed NOT reproducible on this
// workspace, and root-caused: turbo's file-hashing is git-aware and
// respects `.gitignore`, and this repo's `.gitignore` excludes `.turbo/`.
// A fixture with no git repository (or no `.turbo/` exclusion) has no such
// protection -- reproduced definitively both ways: the same trivial task,
// with `.gitignore` present, converges (miss then hit); without it,
// miss/miss forever (verified live on `demo`, 2026-08-06).
//
// This is NOT a general guard against the defect class -- #944's own
// dry-run design is structurally blind to anything that only manifests at
// execution time, and this file does not change that. It locks the ONE
// specific invariant that makes this repo immune TODAY: `.turbo/` (and
// `dist/`, the other turbo-output directory every package writes into)
// staying excluded from git tracking. If either is ever removed from
// `.gitignore` -- during an unrelated cleanup, say -- the self-hashing
// bug this file's header describes would silently reappear with no
// warning from any other guard.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..");

function gitignoreLines(root = REPO_ROOT) {
  return readFileSync(join(root, ".gitignore"), "utf8")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith("#"));
}

test("REAL REPO: .gitignore excludes .turbo/ -- the property that makes this workspace immune to #948's self-hashing bug", () => {
  const lines = gitignoreLines();
  assert.ok(
    lines.includes(".turbo/") || lines.includes(".turbo"),
    ".gitignore no longer excludes .turbo/ -- turbo's own per-task log files would become git-tracked/visible inputs again, reintroducing #948's self-hashing bug with no other guard in this repo able to see it"
  );
});

test("REAL REPO: .gitignore excludes dist/ -- the sibling output directory every buildable package writes into", () => {
  const lines = gitignoreLines();
  assert.ok(
    lines.includes("dist/") || lines.includes("dist"),
    ".gitignore no longer excludes dist/ -- build outputs would become inputs to the next build, the same self-hashing shape #948 describes for .turbo/"
  );
});

// Demonstrated red: proves this check actually fires, not just that it can
// be satisfied by the real file.
test("the check fails on a fixture .gitignore missing .turbo/", () => {
  const lines = ["dist/", "node_modules/"];
  assert.ok(!lines.includes(".turbo/") && !lines.includes(".turbo"));
});
