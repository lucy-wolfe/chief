// A withdrawn launch intent always names the decision that withdrew it.
//
// # Why this file exists
//
// A launch intent is a person the operator asked for. Withdrawing one is the
// product deciding they do not get that person, and until 2026-08-20 most of
// those decisions were invisible.
//
// Measured on `taperoom-inc` (a live box), 2026-08-20: of 597
// `launch-intent` deletes in `org_events` that day, 310 had no matching
// `launch intent withdrawn (...)` line anywhere in the log. One of the silent
// ones was `research-promoter`, granted at `20:34:00.543Z` by the operator's own
// Wake Up (actor `service`) and deleted 2.165 seconds later with actor `''`. The
// pass that deleted her row still printed `launching: ..., research-promoter,
// ...`. Reading the log, nothing had gone wrong; watching the rail, she never
// came up and there was no next question to ask.
//
// The withdrawals in `converge_apply::cycle` have named themselves since #1170
// (`settled` / `not-operational` / `no-demand`). The ones that did not were the
// ROW writers, and a row writer is exactly where a future silent path would
// appear — a new atomic op composing the fence accessor, or a new caller of the
// whole-document publish.
//
// # What it checks
//
//   1. `delete_person_fence` takes a `reason` parameter, so the COMPILER
//      refuses a fence drop that does not name its verb. This is the property
//      that makes the invariant hold for callers this guard has never seen.
//   2. Every `DELETE FROM launch_intent` statement in production Rust sits in a
//      function that also emits a `launch-intent.withdrawn` tracing event. That
//      is the whole set of ways a fence row can die: `launch_intent_rows` is
//      the only module allowed to name the table in SQL (M7 containment), and
//      this pins that its two deleting functions both speak.
//   3. `publish` — the whole-document republish, the silent majority above —
//      names the person it withdrew AND the person it RETAINED under an
//      operator's wake lease. Deliberately at INFO: `remove`, the converge
//      shrink half, commits through this same document path, so this line fires
//      on every ordinary settle, and `cycle.rs` already draws the distinction —
//      a line that fires when the product is working correctly must not be a
//      fault. The value is that the answer to "why is this person gone" exists
//      and is greppable by person, not that it is alarming.
//
// A compiler cannot check (2) or (3), and nothing else in the tree does.
//
// Run with `node --test scripts/test/launch-intent-withdrawal-is-never-silent.test.mjs`.

import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const CRATES = join(repoRoot, "apps", "chiefd", "crates");
const ROWS_RS = "apps/chiefd/crates/chiefd-core/src/store/launch_intent_rows.rs";

/** Every `.rs` file under the chiefd crates, excluding build output. */
function rustSources(dir) {
  const found = [];
  for (const entry of readdirSync(dir)) {
    // `tests/` holds a crate's INTEGRATION tests, and `test_support.rs` its
    // fixtures. A fixture may write whatever SQL it likes; the subject here is
    // the product.
    if (entry === "target" || entry === "node_modules" || entry === "tests") continue;
    if (entry === "test_support.rs") continue;
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) found.push(...rustSources(path));
    else if (entry.endsWith(".rs")) found.push(path);
  }
  return found;
}

/**
 * The source of `path` with every `#[cfg(test)] mod tests { ... }` block cut
 * out. A fixture may delete a fence row without announcing it; the subject here
 * is the product.
 */
function productionSource(path) {
  const text = readFileSync(path, "utf8");
  const marker = text.indexOf("#[cfg(test)]\nmod tests");
  return marker === -1 ? text : text.slice(0, marker);
}

test("delete_person_fence cannot be called without naming the verb that withdrew the fence", () => {
  const rows = readFileSync(join(repoRoot, ROWS_RS), "utf8");
  const signature = rows.match(
    /pub fn delete_person_fence\(([\s\S]*?)\) -> rusqlite::Result/,
  );
  assert.ok(signature, `${ROWS_RS} must declare delete_person_fence`);
  assert.match(
    signature[1],
    /reason:\s*&'static str/,
    "delete_person_fence must take `reason: &'static str`, so dropping a launch intent " +
      "without naming the decision is a COMPILE error rather than a convention. A silent " +
      "withdrawal leaves an operator watching a rail row that never comes up with no next " +
      "question to ask.",
  );
});

test("every DELETE FROM launch_intent sits beside a launch-intent.withdrawn event", () => {
  const offenders = [];
  for (const path of rustSources(CRATES)) {
    const source = productionSource(path);
    if (!/DELETE FROM launch_intent/.test(source)) continue;
    const rel = relative(repoRoot, path);
    // Split on `pub fn` / `fn` boundaries and check each deleting function.
    const functions = source.split(/\n(?=(?:pub(?:\(crate\))? )?fn )/);
    for (const body of functions) {
      if (!/DELETE FROM launch_intent/.test(body)) continue;
      if (/event = "launch-intent\.withdrawn"/.test(body)) continue;
      const name = body.match(/fn (\w+)/)?.[1] ?? "<unknown>";
      offenders.push(`${rel}: ${name}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "these functions delete a launch-intent row without emitting a " +
      '`launch-intent.withdrawn` tracing event. A withdrawn launch intent is a person the ' +
      "operator asked for and is not getting; on `taperoom-inc`, 2026-08-20, 310 of 597 " +
      "fence deletes said nothing at all, and one of them was a wake the operator had made " +
      "2.165 seconds earlier. Name the decision, or do not withdraw.",
  );
});

test("a whole-document republish names both what it withdrew and what it retained", () => {
  const rows = readFileSync(join(repoRoot, ROWS_RS), "utf8");
  const publish = rows.slice(
    rows.indexOf("pub fn publish("),
    rows.indexOf("pub fn clear("),
  );
  assert.ok(publish.length > 0, "launch_intent_rows must declare publish before clear");
  assert.match(
    publish,
    /tracing::info!\(\s*event = "launch-intent\.withdrawn"/,
    "publish's set difference deletes every committed row the incoming document omits, and " +
      "each one must name the person. INFO rather than WARN on purpose: the converge shrink " +
      "half commits through this path too, so this fires on every ordinary settle, and a line " +
      "that fires when the product is working correctly must not be logged as a fault.",
  );
  assert.match(
    publish,
    /event = "launch-intent\.wake-lease-held"/,
    "and it must say when it RETAINS a row instead: a person inside the quiet lease an " +
      "operator's wake bought them is not withdrawn by a republish, and a retention nobody " +
      "can see is the same defect in the other direction.",
  );
});
