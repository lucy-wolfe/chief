// #933: proves the classifier can report BOTH outcomes -- a classifier
// only ever seen agreeing (always "equivalent") is one nobody has watched
// disagree. Built against a REAL, disposable git repo (never a mocked git
// interface), since the whole risk this tool exists to avoid is a
// classifier that is wrong about real git plumbing.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { auditStrandedBranches, classifyBranch, exclusionLabel, isExcluded, listRemoteBranches } from "../stranded-branch-audit.mjs";

function git(cwd, args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

function write(cwd, path, contents) {
  writeFileSync(join(cwd, path), contents);
}

/** A real disposable repo: canonical has two commits; a branch forks after
 *  the first, changes one file, and its own commit is never merged back
 *  (no --is-ancestor relationship) -- exactly the "linearized landing"
 *  shape #933 describes, where content can land without the branch's own
 *  commits ever becoming ancestors. */
function fixtureRepo() {
  const dir = mkdtempSync(join(tmpdir(), "stranded-branch-audit-"));
  git(dir, ["init", "-q", "-b", "main"]);
  git(dir, ["config", "user.email", "test@example.com"]);
  git(dir, ["config", "user.name", "Test"]);
  write(dir, "a.txt", "base\n");
  git(dir, ["add", "a.txt"]);
  git(dir, ["commit", "-q", "-m", "base"]);
  const mergeBase = git(dir, ["rev-parse", "HEAD"]);

  // Branch: changes a.txt.
  git(dir, ["checkout", "-q", "-b", "topic-branch"]);
  write(dir, "a.txt", "branch-content\n");
  git(dir, ["add", "a.txt"]);
  git(dir, ["commit", "-q", "-m", "branch change"]);
  const branchSha = git(dir, ["rev-parse", "HEAD"]);

  git(dir, ["checkout", "-q", "main"]);
  return { dir, mergeBase, branchSha };
}

test("RED (the falsifier): a branch whose change never landed on canonical is classified UNIQUE, not equivalent", () => {
  const { dir, branchSha } = fixtureRepo();
  try {
    // canonical (main) never applied the branch's change -- a.txt still says "base".
    const result = classifyBranch("main", { ref: "origin/topic-branch", sha: branchSha }, dir);
    assert.equal(result.status, "unique", "a branch whose real content never reached canonical must never classify as equivalent");
    assert.deepEqual(result.mismatches, ["a.txt"]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("GREEN: a branch whose change landed on canonical by a DIFFERENT commit (linearized, not merged) is classified equivalent", () => {
  const { dir, branchSha } = fixtureRepo();
  try {
    // canonical (main) independently lands the SAME file content, via its own new commit --
    // never merging topic-branch, exactly the linearized-landing shape #933 describes.
    write(dir, "a.txt", "branch-content\n");
    git(dir, ["add", "a.txt"]);
    git(dir, ["commit", "-q", "-m", "same content landed a different way"]);

    const result = classifyBranch("main", { ref: "origin/topic-branch", sha: branchSha }, dir);
    assert.equal(result.status, "equivalent");
    assert.equal(result.changedFiles, 1);
    // And prove it is NOT simply because the branch merged -- it is genuinely unmerged ancestry.
    assert.throws(() => execFileSync("git", ["merge-base", "--is-ancestor", branchSha, "main"], { cwd: dir }));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a branch already an ancestor of canonical (conventionally merged) is classified 'merged', out of scope entirely", () => {
  const { dir, mergeBase } = fixtureRepo();
  try {
    // mergeBase IS an ancestor of main (canonical's own history includes it).
    const result = classifyBranch("main", { ref: "origin/already-merged", sha: mergeBase }, dir);
    assert.equal(result.status, "merged");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a branch that touches a file canonical later deleted is UNIQUE, never silently treated as a match against absence", () => {
  const { dir, branchSha } = fixtureRepo();
  try {
    // canonical deletes a.txt entirely rather than converging on the branch's content.
    execFileSync("git", ["rm", "-q", "a.txt"], { cwd: dir });
    git(dir, ["commit", "-q", "-m", "delete a.txt on canonical"]);

    const result = classifyBranch("main", { ref: "origin/topic-branch", sha: branchSha }, dir);
    assert.equal(result.status, "unique");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a file the branch changed and canonical ALSO later deleted (both absent) is vacuously equivalent for that file -- deletion agrees with deletion", () => {
  const dir = mkdtempSync(join(tmpdir(), "stranded-branch-audit-del-"));
  try {
    git(dir, ["init", "-q", "-b", "main"]);
    git(dir, ["config", "user.email", "test@example.com"]);
    git(dir, ["config", "user.name", "Test"]);
    write(dir, "a.txt", "base\n");
    write(dir, "b.txt", "keep\n");
    git(dir, ["add", "."]);
    git(dir, ["commit", "-q", "-m", "base"]);

    git(dir, ["checkout", "-q", "-b", "topic-branch"]);
    execFileSync("git", ["rm", "-q", "a.txt"], { cwd: dir });
    git(dir, ["commit", "-q", "-m", "branch deletes a.txt"]);
    const branchSha = git(dir, ["rev-parse", "HEAD"]);

    git(dir, ["checkout", "-q", "main"]);
    execFileSync("git", ["rm", "-q", "a.txt"], { cwd: dir });
    git(dir, ["commit", "-q", "-m", "canonical independently deletes a.txt too"]);

    const result = classifyBranch("main", { ref: "origin/topic-branch", sha: branchSha }, dir);
    assert.equal(result.status, "equivalent");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("isExcluded always excludes preserve/* regardless of caller-supplied patterns", () => {
  assert.equal(isExcluded("preserve/909-fix-fa8078c10", []), true);
});

test("isExcluded applies caller-supplied patterns, and states its own scope limit: it cannot know 'current fleet' from git alone", () => {
  assert.equal(isExcluded("eng1/933-stranded-branch-audit", [/^eng1\//]), true);
  assert.equal(isExcluded("some-other-branch", [/^eng1\//]), false);
});

test("exclusionLabel names WHICH rule matched, never just true/false, so per-pattern counts are possible", () => {
  assert.equal(exclusionLabel("preserve/909-fix", []), "preserve/*");
  assert.equal(exclusionLabel("eng1/933-x", [{ label: "^eng1/", pattern: /^eng1\// }]), "^eng1/");
  assert.equal(exclusionLabel("unrelated", [{ label: "^eng1/", pattern: /^eng1\// }]), undefined);
});

test("the census reconciles: total === canonicalSkipped + excludedTotal + examined, every time, on a real (small) fixture repo", () => {
  const { dir } = fixtureRepo();
  try {
    // A second branch this run will exclude, to exercise all three buckets at once.
    git(dir, ["branch", "excluded-branch", "main"]);
    const result = auditStrandedBranches("main", {
      remote: "origin", // this fixture has no origin remote configured -- listRemoteBranches finds none,
      cwd: dir,
    });
    // listRemoteBranches only sees refs/remotes/origin/*, which this local-only fixture has none
    // of -- so this exercises the reconciliation arithmetic on an empty set, which must still hold.
    assert.equal(result.total, result.canonicalSkipped + result.excludedTotal + result.examined);
    assert.equal(result.reconciles, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// #1041: this used to call `listRemoteBranches("origin")` against THIS
// checkout and assert `branches.length > 0`. The property it was reaching for
// is real and worth keeping — an implementation that always returns `[]`
// passes every fixture above, since the fixture at line ~148 asserts a
// local-only repo yields none — but the evidence it used was a property of
// the WORKING COPY, not of the code. A checkout made from a `git bundle` has
// no `refs/remotes/origin/*` at all, so the guard reported "expected at least
// one real remote branch" about how the repo was obtained. That is the same
// instrument-cannot-see-its-subject shape as the rest of #1041, and the fix
// is the same: give the test a subject it OWNS. A real `git clone` of a real
// disposable repo has real `refs/remotes/origin/*` on every host, whatever
// the surrounding checkout looks like — so the non-vacuity evidence survives
// and the environmental dependence is gone.
test("REAL CLONE sanity: listRemoteBranches returns real refs and never includes origin/HEAD", () => {
  const { dir } = fixtureRepo();
  const clone = mkdtempSync(join(tmpdir(), "stranded-branch-audit-clone-"));
  try {
    git(clone, ["clone", "-q", dir, "."]);
    // `topic-branch` exists in the source, so the clone carries at least
    // `origin/main` and `origin/topic-branch` -- and an `origin/HEAD` the
    // filter must drop.
    const branches = listRemoteBranches("origin", clone);
    assert.ok(branches.length > 0, "expected at least one real remote branch in a real clone");
    assert.ok(
      branches.some((b) => b.ref === "origin/topic-branch"),
      "a real remote-tracking ref from the source repo must be reported"
    );
    assert.ok(!branches.some((b) => b.ref === "origin/HEAD"), "origin/HEAD must be filtered out");
    assert.ok(branches.every((b) => /^[0-9a-f]{40}$/.test(b.sha)));
  } finally {
    rmSync(clone, { recursive: true, force: true });
    rmSync(dir, { recursive: true, force: true });
  }
});
