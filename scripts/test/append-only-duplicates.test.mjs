// `CHANGELOG.md` and `DECISIONS.md` gain DUPLICATE entries, and nothing else
// in this repository can see it.
//
// # The mechanism, first, because it is not what anybody assumes
//
// This is NOT a mistake somebody commits. It is the defined behaviour of git
// on the file shape we chose:
//
//   In an append-only file, the same lines added at different OFFSETS on two
//   branches are, to a three-way merge, two INDEPENDENT ADDITIONS. Git keeps
//   both. No conflict is raised, because nothing conflicts — both sides only
//   added.
//
// So it cannot be trained away and no amount of care prevents it. Every merge
// of `main` into a branch that has touched either file can silently duplicate
// whatever both sides appended. A rebase that appends a second time is the
// same defect by a narrower path; the merge path is the common one, and this
// repository hit it three times in a single afternoon (the A3 actuator entry,
// the 2026-07-24 e2e-harness entry, and two mailbox entries in #1115).
//
// # Why this cannot be folded into the append-only check
//
// The standing check is `git diff origin/main -- <file> | grep -c '^-[^-]'`,
// asserting zero deletions. Against this defect that check is not weak, it is
// STRUCTURALLY INCAPABLE: it asks "was anything destroyed?" and duplication
// destroys nothing. It reads 0 before the duplication and 0 after it. The two
// checks answer different questions and both are needed.
//
// # Why the key is a PREFIX, and why the fuzziness is the feature
//
// Entries are keyed on their first `KEY_LENGTH` characters, not on the whole
// line. That can in principle match two genuinely distinct entries sharing a
// long opening clause, and the allowlist below absorbs that cost.
//
// A normalised full-line hash would be exact and would have caught NEITHER
// real instance — both the A3 pair and the e2e-harness pair had DIVERGENT
// TAILS. And the divergent-tail case is the dangerous one, because that is
// where the older copy asserts the OPPOSITE of what shipped: the deleted A3
// draft described an acquisition failure as one undifferentiated "warn and
// proceed", which is the reverse of the `KeyAbsent`/`KeyTooPermissive` split
// that actually landed. A guard tuned to identical lines is a guard against a
// defect we have never had, blind to the one we have had three times.
//
// So the prefix is the guard's PURPOSE and not an implementation shortcut.
//
// # The trap you are about to hit, stated here because this guard sends you to it
//
// FIXING A DUPLICATE IS ITSELF A MARKDOWN-ONLY PUSH. `ci.yml`'s docs-only
// detection will therefore skip the entire matrix on your fix, and the run
// will report `conclusion: success` having executed ONE job of eighteen —
// `ok=1, skip=17`. So the remedy for the defect this guard finds reliably
// triggers a different one: a green run that verified nothing.
//
// The pull-request-wide docs guard now sees earlier code changes when the last
// commit changes only Markdown. A pull request that contains only a Markdown
// repair still skips the matrix by policy, so run this guard before the push.
//
// Run with `node --test scripts/test/append-only-duplicates.test.mjs`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** Long enough to survive a shared opening clause, short enough to match two
 *  drafts of one decision whose tails diverged. */
const KEY_LENGTH = 120;

/**
 * The files, and what an ENTRY looks like in each.
 *
 * # These patterns were BLIND, and the blind half is where entries land
 *
 * They began as a dashed-date pattern and a leading-bold pattern, each
 * describing one of the shapes its file actually holds:
 *
 *   DECISIONS.md   2286 `- <date> …`   |  676 bare `<date> …`  |  9 `# <date> …`
 *   CHANGELOG.md   1706 `- **…**`      |  545 `- <type> (#n): …`
 *
 * So the guard could not see 685 of 2971 decision entries (23%) or 545 of 2251
 * changelog entries (24%) — and the bare-date shape is the one every CURRENT
 * decision is written in. It reported green over the region new entries land
 * in, which is worse than no guard, because a guard people trust is a guard
 * people stop re-deriving. Two byte-identical `2026-08-07` decision pairs sat
 * inside the unseen region from the day the guard shipped, and were adjudicated
 * with this widening.
 *
 * The fix is ONE pattern per file describing what an entry IS, not a list of
 * the spellings seen so far — a list of special cases rots back into this bug
 * the next time somebody writes an entry a little differently:
 *
 *   DECISIONS.md   a top-level line that STARTS WITH A DATE, with an optional
 *                  `- ` or `# ` marker in front of it.
 *   CHANGELOG.md   a top-level list item. Every entry is one, whatever its
 *                  first word is; continuation lines are indented.
 *
 * The `# <date>` arm is not a special case for tidiness: a legacy block near
 * DECISIONS.md:4375 has real entries mangled into markdown headings. They are
 * historical record and are not rewritten, so the guard reads them where they
 * are.
 *
 * `entry-coverage` below is what keeps this honest going forward.
 */
const SURFACES = [
  { file: "DECISIONS.md", entry: /^(?:[-#] )?\d{4}-\d{2}-\d{2}/ },
  { file: "CHANGELOG.md", entry: /^- / },
];

/** A line nobody indented: an entry always starts one, a continuation never does. */
const TOP_LEVEL = /^\S/;

/**
 * How much of each file the entry pattern must reach.
 *
 * These files are lists. Almost every top-level line IS an entry; the rest is a
 * one-line preamble, a heading, and a few stray prose paragraphs. The files
 * measure 98.5% and 98.7% today, so a 95% floor is slack for more prose and
 * still far below what an unseen entry SHAPE costs: under the narrow patterns
 * this replaced the same measurement reads 75.9% and 74.8%.
 */
const MIN_COVERAGE = 0.95;

/**
 * Duplicates this guard tolerates. It is EMPTY, and that is the finished state.
 *
 * It shipped holding eight pre-existing `CHANGELOG.md` keys, frozen so the
 * guard was green the day it landed rather than red on arrival for reasons its
 * author could not fix. All eight have since been adjudicated — the losing
 * draft deleted, the surviving entry kept — so every row came out with them.
 *
 * DO NOT ADD TO THIS LIST. A new duplicate is the defect this guard exists to
 * catch. The assertion is EXACTLY-EQUALS, so a stale row fails just as loudly
 * as a new duplicate; a subset check would have let this list rot into
 * permanent exemptions nobody revisits.
 */
const KNOWN_DUPLICATES = [];

/** Every duplicated entry in one file, with both lines and their numbers. */
function duplicateEntries({ file, entry }) {
  const lines = readFileSync(join(repoRoot, file), "utf8").split("\n");
  const firstSeenAt = new Map();
  const duplicates = [];
  lines.forEach((line, index) => {
    if (!entry.test(line)) return;
    const key = line.slice(0, KEY_LENGTH);
    const earlier = firstSeenAt.get(key);
    if (earlier === undefined) {
      firstSeenAt.set(key, index);
      return;
    }
    duplicates.push({
      file,
      key,
      first: { number: earlier + 1, text: lines[earlier] },
      second: { number: index + 1, text: line },
    });
  });
  return duplicates;
}

/**
 * The report a reader needs, which is BOTH LINES IN FULL.
 *
 * When this fires the first question is "which of these two is right?", and
 * answering it requires seeing the divergent tails side by side. Printing only
 * the shared key would announce a duplicate and hide the one piece of
 * information that resolves it.
 */
function report(duplicate) {
  return [
    "",
    `${duplicate.file}: the same entry appears twice.`,
    "",
    `  line ${duplicate.first.number}: ${duplicate.first.text}`,
    "",
    `  line ${duplicate.second.number}: ${duplicate.second.text}`,
    "",
    "Keep the entry that describes what SHIPPED; the other is a superseded",
    "draft and may assert the opposite. Delete the loser from the markdown.",
    "",
    "This is not a mistake you made: a three-way merge treats the same lines",
    "added at different offsets on two branches as two independent additions",
    "and keeps both, without a conflict. Run this guard after every merge of",
    "main into a branch that touches these files.",
    "",
    "AND THE NEXT TRAP, because your fix walks straight into it: deleting the",
    "loser is a MARKDOWN-ONLY push, so CI skips the whole matrix and reports",
    "success having run one job. Use `gh pr close && gh pr reopen` to schedule",
    "a real one — it changes no tree.",
    "",
  ].join("\n");
}

test("no NEW duplicate entry in the append-only files", () => {
  const found = SURFACES.flatMap(duplicateEntries);
  const unknown = found.filter(
    (duplicate) => !KNOWN_DUPLICATES.some((known) => duplicate.key.startsWith(known)),
  );
  assert.deepEqual(
    unknown.map(report),
    [],
    "a duplicated entry was added to an append-only file",
  );
});

test("the frozen list has no stale rows: every known duplicate is still present", () => {
  const keys = SURFACES.flatMap(duplicateEntries).map((duplicate) => duplicate.key);
  const stale = KNOWN_DUPLICATES.filter((known) => !keys.some((key) => key.startsWith(known)));
  assert.deepEqual(
    stale,
    [],
    "these rows no longer name a real duplicate — somebody adjudicated one and did not " +
      "strike it here. The assertion is exactly-equals rather than subset precisely so " +
      "this fails: a tolerated allowlist rots into permanent exemptions nobody revisits.",
  );
});

test("the guard is non-vacuous: it can see the entries it is keyed on", () => {
  // A regex that stopped matching would make every assertion above pass by
  // finding nothing at all — the failure mode this repo's other guards call
  // out by name.
  //
  // THIS USED TO BE A SIZE FLOOR (`> 100` matched lines), and the size was
  // standing in for "this is really the file". That proxy died with the
  // 2026-08-25 public-ledger reset: both files legitimately start at a
  // handful of entries, so the floor failed a correct pattern reading a
  // correct file. A count floor was always the weaker instrument anyway —
  // "many" and "all" are different questions, which is exactly the lesson the
  // coverage test below was added to record.
  //
  // The replacement asks the same question without knowing the file's size.
  // A drifted pattern shows up as matching NOTHING, or as failing on the
  // file's very FIRST entry — the one line every reader sees, and the one a
  // pattern cannot miss while still being the right pattern. The "all, not
  // many" half is the coverage test's job, and it does not need a size to do
  // it.
  for (const surface of SURFACES) {
    const lines = readFileSync(join(repoRoot, surface.file), "utf8").split("\n");
    const entries = lines.filter((line) => surface.entry.test(line));
    assert.ok(
      entries.length > 0,
      `${surface.file}: the entry pattern matched NOTHING — it has drifted and the guard is ` +
        "checking nothing at all",
    );
    const firstTopLevel = lines.find((line) => TOP_LEVEL.test(line));
    assert.ok(
      firstTopLevel !== undefined && surface.entry.test(firstTopLevel),
      `${surface.file}: the entry pattern does not match the file's FIRST top-level line ` +
        `(${JSON.stringify((firstTopLevel ?? "").slice(0, 100))}). Either the pattern drifted or ` +
        "the file's own opening stopped being an entry; both need a deliberate answer.",
    );
  }
});

test("entry coverage: the pattern reaches every region, not just the one it was written for", () => {
  // The defect a line count CANNOT see. Both patterns matched thousands of
  // lines and both were blind to a whole entry shape — the non-vacuity check
  // above was green throughout, because "many" and "all" are different
  // questions and only the second one is the guard's promise.
  //
  // Measuring matched lines against TOP-LEVEL lines asks the second question
  // without naming any spelling, so a new entry shape shows up here as a
  // falling number rather than as silence.
  for (const surface of SURFACES) {
    const lines = readFileSync(join(repoRoot, surface.file), "utf8").split("\n");
    const topLevel = lines.filter((line) => TOP_LEVEL.test(line));
    const missed = topLevel.filter((line) => !surface.entry.test(line));
    const coverage = (topLevel.length - missed.length) / topLevel.length;
    assert.ok(
      coverage >= MIN_COVERAGE,
      `${surface.file}: the entry pattern reaches ${(coverage * 100).toFixed(1)}% of the ` +
        `${topLevel.length} top-level lines, below the ${MIN_COVERAGE * 100}% floor. An entry ` +
        "SHAPE is invisible to this guard, so it reports green over a region it cannot read. " +
        "Widen the pattern to describe what an entry IS — do not add the new spelling as a " +
        "second alternative, which is how the original blindness was built.\n\n" +
        `First unreached lines:\n${missed
          .slice(0, 5)
          .map((line) => `  ${line.slice(0, 100)}`)
          .join("\n")}`,
    );
  }
});
