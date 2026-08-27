// #970: guard against the gap re-derived by scripts/coverage-scope-gap.mjs
// growing silently. A ONE-TIME report ("here are 64 files") would leave
// the fleet exactly where it is the next time someone adds a directory --
// this is the check, not the census.
//
// KNOWN_GAP below is the reviewed baseline as of this story: every file
// scripts/coverage-scope-gap.mjs currently finds referenced-but-uncovered,
// each with a stated category. A NEW file appearing in the live derivation
// that is NOT in this list fails the guard by name -- exactly the #892/
// #937/#960/#962/#970 shape (a real interface, checked by nothing) is
// caught the moment it appears, rather than two days later by someone
// executing the thing.
//
// This does not fix the gap (per #970's own instruction: "print the scope
// of your own answer... report the distinction rather than a raw list").
// It converts "nobody is watching this" into "a name is on record and any
// ADDITION to the list is a loud, reviewed event."

import { test } from "node:test";
import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { deriveCoverageGap } from "../coverage-scope-gap.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(__dirname, "..", "..");

// Reviewed baseline, categorized. Every entry here was actually produced by
// a run of scripts/coverage-scope-gap.mjs against this exact tree -- not
// hand-imagined. Growing this list requires a human to add a line, which is
// the point: an addition is reviewed, not silent.
const KNOWN_GAP = new Set([
  // conformance/: a standalone scenario-replay tool with no package.json or
  // tsconfig of its own -- genuinely outside every instrument, real code
  // (imported by conformance/run-ts.ts's own scenarios), tracked here
  // rather than fixed by this story.
  // "conformance/scenarios/index.ts" removed: it is no longer REFERENCED,
  // because the graph that reached it is broken.
  //
  // #1044 CORRECTION: this comment used to say the graph broke because
  // `conformance/lib/durable.ts` imports four modules the Rust port deleted. The
  // imports are real, but they are the SECOND fault, and naming only them sends
  // the next reader to a repair that would not work. The first fault is that
  // `conformance/lib/durable.ts` DOES NOT PARSE: merge `b887b9a9c` (2026-08-07)
  // resolved a conflict by keeping BOTH branches' `healthy()`, so the `async`
  // one at line 38 is never closed, `conformanceChiefdUrl` and
  // `ensureDurableStore` are `export` declarations nested inside a function
  // body, and the second `healthy()` `await`s in a non-`async` function.
  // Repairing the imports would not make the file load. The deleted modules are
  // `org-company-endpoint`, `org-durable-store`, `org-sync-transport` and
  // `tests/helpers/observed-founder`. Both faults sit outside every tsc leg and
  // every CI lane, so nothing has ever said so — see the #751 gap audit, and
  // note that the same break takes `bun conformance/record-ts.ts` down with it,
  // so the corpus has no recorder either. And a third fault under both:
  // `lib/tool-host.ts` installs the intercom with no chiefd endpoint and no
  // beacond wiring, so the harness cannot resolve a company or make a real route
  // call — nothing on that path could ever disagree with a fixture.
  //
  // #751/G14 (2026-08-08): the CORPUS is no longer dark. Rust replays three of
  // the four families against `conformance/fixtures/` byte for byte — activity
  // (19), assignment (29) and session-maintenance (44) — under plain
  // `cargo test --workspace`, and 15 of the 137 `tools` fixtures are replayed by
  // chiefd-api's `conformance_reminders.rs` and `conformance_tasks.rs`. What
  // stays here is the TypeScript half only. It was deliberately NOT deleted:
  // `scenarios/tools.ts` and `lib/tool-host.ts` are the recorder for the
  // `tools` family, whose subject
  // (`packages/piing/extensions/organization-intercom.ts`) is live TypeScript
  // that no Rust replaces, so deleting them would destroy the only description
  // of how those fixtures are produced. Repointing `durable.ts` is also not the
  // fix — that would rebuild a TypeScript store path the port deleted.
  // scripts/*.ts rows removed here: the reason they carried -- "run directly
  // via `bun`/`node --experimental-*` outside both tsc legs and vitest" --
  // stopped being true when tsconfig.scripts.json became the fifth leg of
  // scripts/typecheck.sh. All six `scripts/**/*.ts` are inside a real
  // typecheck scope now, so the three rows that used to sit here
  // (reactive-allowlist.ts, reactive-scan.ts, release-chiefd.ts) went stale in
  // the direction a register wants to go stale, and this file's own
  // stale-entry arm is what said so. Re-verified against the live derivation
  // immediately before removal.
  // shim/ entries removed here: the whole 1,796-line directory is DELETED
  // (#751 G8, Mandate 0). Its only importer anywhere was `tests/shim.test.ts`,
  // deleted with it. The row's old reason — "imported by its own generated/
  // consumers" — had stopped being true: nothing imports it. The half of that
  // test worth keeping (drift between the Rust wire types and
  // packages/chiefing/generated/chiefd-request-schemas.json) is guarded on the
  // AUTHORITATIVE side by chiefd-api's `shim_schema_export` test, which
  // regenerates and diffs that exact file; a TypeScript copy of that check
  // could only ever agree or be wrong. Re-verified against the live tree
  // immediately before removal.
  // tests/e2e/** entries removed here: all 15 (4 under fresh-org/, 11 under
  // harness/) deleted along with tests/e2e/ itself, user order, E2E
  // retirement. Re-verified each against the live tree immediately before
  // removal, not carried over from a prior report -- a KNOWN_GAP row naming
  // a file that cannot exist is wrong regardless of what deriveCoverageGap
  // returns, so removing them cannot make this guard less correct.
  // tests/helpers/**: fixture/support code for the same parked corpus,
  // same reasoning as tests/e2e/** above.
  //
  //
  // #1035: thirteen tests/ rows and one scripts/ row left this baseline with
  // the files they named. The parked bun:test corpus that imported them is
  // deleted (it tested `apps/cli/src/legacy/`, which #751 removed), and a
  // support file whose last importer went with it is not a coverage gap —
  // it is not code. Removed: helpers/chiefd-binary-path.ts,
  // helpers/durable.ts, helpers/fake-beacond.ts, helpers/fixture-namespace.ts,
  // helpers/isolated-home.ts, helpers/memory-review.ts,
  // helpers/observed-department.ts, helpers/observed-founder.ts,
  // helpers/observed-hire.ts, helpers/operator-pane.ts,
  // helpers/zipbox-provider-fixture.ts, setup-durable-store.ts,
  // unimplemented-durable-backend.ts, and
  // scripts/unit-shard-parent-death-watchdog.ts (the bun:test sharder's
  // helper, deleted with scripts/ci-shard.ts).
  //
  // The two that STAY are the two still genuinely imported by a surviving
  // file: tmux-socket.ts (by tests/tmux-socket-teardown.test.ts) and
  // setup-workspace-build-preflight.ts (by tests/setup-conditional-preload.ts,
  // the bunfig preload). They are in the baseline rather than pulled into a
  // scope for the same reason as every sibling here: the whole tests/ corpus
  // sits outside every tsc leg by ruling D15, and moving one file in would
  // not change that.
  "tests/helpers/tmux-socket.ts",
  "tests/setup-workspace-build-preflight.ts"
]);

test("#970: the coverage-scope gap (referenced-but-uncovered files) matches the reviewed KNOWN_GAP baseline exactly -- a NEW entry fails by name", () => {
  const { referenced } = deriveCoverageGap(repoRoot);
  const referencedSet = new Set(referenced);

  const newlyUncovered = referenced.filter((f) => !KNOWN_GAP.has(f));
  assert.deepEqual(
    newlyUncovered,
    [],
    `${newlyUncovered.length} file(s) are referenced by something in the tree but outside every typecheck/test ` +
      `scope, and are NOT in the reviewed KNOWN_GAP baseline -- this is the #970 class (a real interface, checked ` +
      `by nothing) recurring. Review each, then either bring it into a scope or add it to KNOWN_GAP with a stated ` +
      `reason:\n${newlyUncovered.join("\n")}`
  );

  const noLongerInGap = [...KNOWN_GAP].filter((f) => !referencedSet.has(f));
  assert.deepEqual(
    noLongerInGap,
    [],
    `${noLongerInGap.length} KNOWN_GAP entries no longer appear in the live derivation -- they were either brought ` +
      `into a real scope (good -- remove them from KNOWN_GAP) or deleted/moved (update the path). A stale entry ` +
      `here is the same "manifest tracks reality" failure #877's own guard-wiring check exists to catch:\n${noLongerInGap.join("\n")}`
  );
});

test("#970 ARM: the derivation actually finds something -- a vacuity check, not just a shape check", () => {
  const { referenced } = deriveCoverageGap(repoRoot);
  assert.ok(referenced.length > 0, "expected a non-empty coverage gap against the real repo -- a zero result here is itself suspicious given the known org-world.ts case");
});

// #970 CONTROL, repointed: org-world.ts (the original target) was deleted
// with tests/e2e/ -- a control asserting a deleted file's presence can
// never pass again, which makes it a control that no longer proves
// anything, not a stale row (a KNOWN_GAP entry just names a fact; a
// CONTROL's whole job is to fail the way the guard's real defect would).
//
// Repointed AGAIN by #1035, and for exactly the reason stated above: the
// previous target, tests/helpers/isolated-home.ts, was itself deleted when
// the parked bun:test corpus went. Its three cited call sites
// (tests/org-cli.test.ts, tests/company-session-actions.test.ts,
// tests/org-provider-admission-compat.test.ts) were three of the 135 files
// removed, which left it with zero importers -- so it would have dropped
// out of `referenced` and this control would have started failing for a
// reason that is not the defect it watches for.
//
// Now pointed at tests/helpers/tmux-socket.ts. Why this file discriminates:
// deriveCoverageGap's TEXT-SPECIFIER match (see this file's own header --
// "does any file anywhere in the tree contain an import/require specifier
// whose final path segment equals this file's own basename") only puts a
// path in `referenced` if something in the tree actually names it in an
// import. tmux-socket.ts has a real, verified call site that SURVIVES this
// deletion: tests/tmux-socket-teardown.test.ts:17's `import { killTmuxServer,
// tmuxSocketPath } from "./helpers/tmux-socket"` -- enumerated directly for
// this repoint, not assumed. So if deriveCoverageGap returned everything (a
// broken derivation that never actually reads import specifiers) OR nothing
// (a broken derivation that never actually resolves a file), this file's
// presence in the SPECIFIC shape "referenced by a real import, present in
// neither TYPECHECKED nor TEST-COVERED" would not follow from either failure
// mode by coincidence -- it requires the derivation to have actually walked
// that import specifier AND actually excluded tests/** from both scopes,
// which is the real property #970 exists to check.
test("#970 CONTROL: tests/helpers/tmux-socket.ts -- a live, genuinely-imported file outside every scope -- is present in the derived gap", () => {
  const { referenced } = deriveCoverageGap(repoRoot);
  assert.ok(
    referenced.includes("tests/helpers/tmux-socket.ts"),
    "expected tmux-socket.ts (imported by tests/tmux-socket-teardown.test.ts, covered by neither TYPECHECKED nor TEST-COVERED) to appear in the derived gap"
  );
});
