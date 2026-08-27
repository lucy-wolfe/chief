// A test file CI excludes from a shard must be reachable by a documented
// local command, or nobody can run it before pushing.
//
// # The defect this closes
//
// `.github/workflows/ci.yml` runs the piing unit shards with
// `--exclude='test/toolcontract/OrganizationToolContract.test.ts'` and two
// siblings, then runs those three in four dedicated `toolcontract` lanes. The
// effect is that `bun run test` covers every piing test EXCEPT those three,
// and nothing in the standing pre-push list covers them either. A change can
// be green on every check a human runs and red in CI, which is what happened:
// three stacked breakages (a genesis spec key, a moved `pi-home` path, a
// bearer) sat in those suites across three stages and were invisible locally.
//
// AGENTS.md already states the rule this violates, about guards:
//
//   "A correct, CI-wired guard nobody runs before pushing produces exactly
//    the same outcome as a broken guard."
//
// The same is true of a test suite. Exclusion is legitimate — these boot a
// real tmux host and take minutes, so keeping them out of the fast shards is
// right. What is not legitimate is excluding them and leaving no documented
// way to run them.
//
// # What this asserts, and what it deliberately does not
//
// It does NOT demand that every excluded file run in the default `bun run
// test`. That would undo the exclusion. It demands two things instead:
//
//   1. every path CI excludes from a shard is named by at least one lane in
//      the same workflow — an exclusion with no lane is a file nothing runs
//      anywhere, which is strictly worse than not having the test;
//   2. every excluded path is named in `CLAUDE.md`'s standing-check section,
//      so a person reading the list learns the suite exists and how to run
//      it.
//
// Run with `node --test scripts/test/excluded-suites-are-runnable.test.mjs`.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const WORKFLOW = join(repoRoot, ".github", "workflows", "ci.yml");
const STANDING_LIST = join(repoRoot, "CLAUDE.md");

/** Every `--exclude='<path>'` CI passes to a test runner. */
export function excludedPaths(workflow) {
  const found = new Set();
  for (const match of workflow.matchAll(/--exclude=['"]([^'"]+)['"]/g)) {
    found.add(match[1]);
  }
  return [...found].sort();
}

/**
 * Whether `path` is named somewhere in the workflow OTHER than its own
 * `--exclude=` occurrences — i.e. some lane actually runs it.
 */
export function isNamedByALane(workflow, path) {
  const mentions = workflow.split(path).length - 1;
  const exclusions = workflow.split(`--exclude='${path}'`).length - 1;
  const doubleQuoted = workflow.split(`--exclude="${path}"`).length - 1;
  return mentions > exclusions + doubleQuoted;
}

test("every path CI excludes from a shard is still run by some lane", () => {
  const workflow = readFileSync(WORKFLOW, "utf8");
  const excluded = excludedPaths(workflow);

  // Non-vacuity: this guard is about a real exclusion set. If the workflow
  // stops excluding anything, the assertions below pass trivially and this
  // file should be deleted rather than left as decoration.
  assert.ok(
    excluded.length > 0,
    "ci.yml excludes nothing; delete this guard rather than leaving it green over no subject",
  );

  for (const path of excluded) {
    assert.ok(
      existsSync(join(repoRoot, "packages", "piing", path)) ||
        existsSync(join(repoRoot, path)),
      `ci.yml excludes ${path}, which does not exist — a stale exclusion silently widens a shard`,
    );
    assert.ok(
      isNamedByALane(workflow, path),
      `${path} is excluded from a shard and named by no lane, so NOTHING runs it — ` +
        "not CI, and not any local command",
    );
  }
});

test("every excluded suite is named in the standing check list, so a person can run it", () => {
  const workflow = readFileSync(WORKFLOW, "utf8");
  const standing = readFileSync(STANDING_LIST, "utf8");

  for (const path of excludedPaths(workflow)) {
    // The basename, because the standing list names suites the way a person
    // types them, not by workflow-relative path.
    const suite = path.split("/").pop().replace(/\.test\.ts$/, "");
    assert.ok(
      standing.includes(suite),
      `${suite} is excluded from the ordinary shards, so \`bun run test\` does not cover it. ` +
        "CLAUDE.md's standing-check section must name it and say how to run it, or it is " +
        "CI's alone to discover — the same outcome as having no test at all.",
    );
  }
});
