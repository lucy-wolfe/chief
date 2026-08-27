// A repo tree-walk never descends into another checkout of this repo.
//
// # The failure this pins
//
// Agent worktrees live under `.claude/worktrees/<name>/`, and each is a FULL
// second copy of the repo. Five guards walked into them and judged somebody
// else's in-progress branch as if it were this checkout's code, so every seat
// with a live worktree — most of the time — saw the same five reds, each naming
// a path the reader did not recognise and could not fix. Two did not merely
// report a wrong finding, they DIED on `ENOENT` from a dangling path inside a
// nested checkout.
//
// CI has no worktrees, so CI never saw any of it. That is the dangerous half:
// `CLAUDE.md`'s standing rule is that a correct guard nobody can run before
// pushing produces exactly the same outcome as a broken one, and five
// unactionable reds is how a suite stops being read and a real red rides
// through with them.
//
// # Why the rule, and not the instances
//
// Each of those guards used to hand-maintain its own near-identical skip set,
// which is the duplicated-predicate shape that let one guard get a fix while
// the next — written from an older template — did not. The sets are one
// definition now (`scripts/tree-walk-lib.mjs`), and these tests pin the RULE so
// the next tree-walking guard written from an old template is caught here
// rather than by an operator wondering why their board is red.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { skipSet } from "../tree-walk-lib.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

test("the shared set excludes the directory agent worktrees live under", () => {
  assert.ok(
    skipSet().has(".claude"),
    "`.claude/worktrees/<name>/` is a full second checkout of this repo; a walk that " +
      "enters it reports findings about another agent's branch"
  );
});

test("the shared set still excludes build output and the git directory", () => {
  // The exclusion `.claude` was ADDED to; none of the original members may be
  // lost in the consolidation, or a guard starts walking `node_modules`.
  for (const name of [".git", "node_modules", "target", "dist", ".turbo", ".next", "coverage"]) {
    assert.ok(skipSet().has(name), `${name} must stay excluded`);
  }
});

test("a guard's own additions do not replace the shared members", () => {
  const set = skipSet(["patches"]);
  assert.ok(set.has("patches"), "the caller's own addition is honoured");
  assert.ok(set.has(".claude"), "and it does not drop the shared exclusions");
  assert.ok(set.has("node_modules"));
});

test("one guard cannot widen the exclusion for every other guard", () => {
  // NOT a formality. The first draft of `tree-walk-lib` exported the shared
  // value directly as `Object.freeze(new Set([...]))`, which is not frozen at
  // all - a Set's entries live in internal slots rather than in properties, so
  // `.add()` still worked and any caller could have silently widened the
  // exclusion for the whole suite. This test failed on that draft. The shared
  // value is module-private now and `skipSet` copies it per call, so the hazard
  // is removed rather than documented.
  const mine = skipSet();
  mine.add("apps");
  assert.ok(!skipSet().has("apps"), "the next caller gets a clean set");
});

test("no guard re-spells the skip set this consolidation replaced", () => {
  // The regression this closes is a CLASS, so the assertion is over the class
  // rather than over the five files that happened to break.
  //
  // The signal is precise on purpose: a `new Set([...])` literal naming BOTH
  // `node_modules` and `.git` IS the duplicated tree-walk predicate, and every
  // guard that broke had one. It does not fire on a guard that merely mentions
  // `node_modules` in prose or matches it in a path, so it will not nag files
  // that are not walkers.
  //
  // Why this and not "does the walk start at the repo root": that question
  // cannot be answered honestly by reading source. A heuristic broad enough to
  // catch the real root-walkers also caught twenty guards that walk a fixed
  // subdirectory and can never reach `.claude` - and demanding a change from
  // those would imply a hazard that is not there, which is exactly what this
  // packet was told not to do.
  // Both directories, because walkers live in both: the guards are
  // `scripts/test/*.test.mjs`, and the scanners they drive (`orphaned-fake-
  // detector.mjs`, `coverage-scope-gap.mjs`) are `scripts/*.mjs`. One of the
  // five that broke was a scanner, so checking only the guards would have left
  // half the class open.
  const offenders = [];
  const self = basename(fileURLToPath(import.meta.url));
  const scripts = join(repoRoot, "scripts");
  const candidates = [
    ...readdirSync(scripts)
      .filter((name) => name.endsWith(".mjs"))
      .map((name) => ({ label: `scripts/${name}`, path: join(scripts, name) })),
    ...readdirSync(join(scripts, "test"))
      .filter((name) => name.endsWith(".test.mjs") && name !== self)
      .map((name) => ({ label: `scripts/test/${name}`, path: join(scripts, "test", name) })),
  ];

  for (const candidate of candidates) {
    const source = readFileSync(candidate.path, "utf8");
    for (const match of source.matchAll(/new Set\(\[(.*?)\]\)/gs)) {
      const body = match[1];
      if (body.includes("node_modules") && body.includes(".git")) {
        offenders.push(candidate.label);
        break;
      }
    }
  }
  offenders.sort();

  assert.deepEqual(
    offenders,
    [],
    "these guards spell their own tree-walk skip set. Import `skipSet` from " +
      "`scripts/tree-walk-lib.mjs` instead - a hand-maintained copy is how one guard " +
      "gets a fix and the next, written from an older template, walks into " +
      "`.claude/worktrees/<name>/` and judges another agent's checkout"
  );
});

test("a walk driven by the shared set does not descend into a nested checkout", () => {
  // The behavioural half, on a real directory tree rather than on the set's
  // membership: build the exact shape that broke — a repo with a second
  // checkout under `.claude/worktrees/<name>/` — and prove the walk never
  // reaches the file inside it.
  const root = mkdtempSync(join(tmpdir(), "tree-walk-nested-"));
  try {
    mkdirSync(join(root, "packages", "chiefing", "src"), { recursive: true });
    writeFileSync(join(root, "packages", "chiefing", "src", "Client.ts"), "export const a = 1\n");
    const nested = join(root, ".claude", "worktrees", "agent-abc", "packages", "chiefing", "src");
    mkdirSync(nested, { recursive: true });
    writeFileSync(join(nested, "Client.ts"), "export const a = 2\n");

    const skip = skipSet();
    const found = [];
    const walk = (dir) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        if (skip.has(entry.name)) continue;
        const full = join(dir, entry.name);
        if (entry.isDirectory()) walk(full);
        else found.push(full.slice(root.length + 1));
      }
    };
    walk(root);

    assert.deepEqual(found, ["packages/chiefing/src/Client.ts"]);
    assert.ok(
      !found.some((file) => file.includes(".claude")),
      "the second checkout is invisible to the walk"
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
