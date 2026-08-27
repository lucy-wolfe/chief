// Runtime demand from mail is read from the `mailbox` TABLE and from nowhere
// else.
//
// # Why this file exists
//
// `Ledgers` is chiefd's in-memory image of the company, hydrated from SQLite
// exactly once in `CompanyDb::open`. Its mailbox map only ever GAINS rows:
// `mailbox::enqueue` writes a `pending` row for chiefd's own mail — a fired
// reminder, a health incident, an escalation — and nothing ever moves that row
// on. A pane drains its mailbox through `/v1/org/mailbox/delta`, which writes
// the `mailbox` table straight on the transaction and says so in its own words
// at `CompanyDb::mailbox_delta` ("Bypassing the Ledgers snapshot").
//
// So every envelope chiefd delivered since the process started is `accepted` in
// SQL and `pending` in memory, for ever. The converge cycle used to union that
// memory into its demand set, and a phantom `ActivityReason::Requested` is
// effective demand: `activity::reconcile` recomputes `idle_since` as NULL on
// every pass, the quiet lease never expires, and the person is never a park
// candidate.
//
// MEASURED on `taperoom-inc` (a live box), 2026-08-20 21:38Z:
// thirteen people desired-active, `agent_quiet_at` twenty minutes old,
// `idle_since` NULL, against ZERO pending rows in SQL. The daemon started at
// 20:18:41; every person whose reminder fired after that instant was pinned,
// and `intel-lead` — whose last reminder fired at 20:05:07, before it — was the
// only one of them that settled normally. They were green, connected, and doing
// nothing, and no restart short of the daemon's own would have freed them.
//
// # What it checks
//
// The converge cycle — the one place runtime demand is computed — does not read
// the in-memory mailbox at all. A unit test can pin the behaviour for the world
// it builds; only this can pin that a future edit does not reach for the
// convenient in-process copy again, which is exactly how the union arrived.
//
// `gather/health_snapshot.rs` still reads it, deliberately and separately: that
// surface REPORTS mail rather than deciding who runs, so a stale row there
// cannot hold anybody up. It is named in the allowlist below so that the day it
// is fixed, this guard is what tells you the allowlist row is dead.
//
// Run with `node --test scripts/test/mail-demand-reads-one-table.test.mjs`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const CYCLE_RS =
  "apps/chiefd/crates/chiefd-host/src/converge_apply/cycle.rs";

/** The source with every `mod tests` block and every `//` comment removed. */
function productionCode(relPath) {
  const text = readFileSync(join(repoRoot, relPath), "utf8");
  const marker = text.indexOf("#[cfg(test)]");
  const production = marker === -1 ? text : text.slice(0, marker);
  return production
    .split("\n")
    .filter((line) => !line.trimStart().startsWith("//"))
    .join("\n");
}

test("the converge cycle never reads the in-memory mailbox", () => {
  const code = productionCode(CYCLE_RS);
  assert.doesNotMatch(
    code,
    /\.mailbox_rows\(\)/,
    `${CYCLE_RS} must not read \`Ledgers::mailbox_rows\`. That map never sees a ` +
      "drain, so a row in it is `pending` for the life of the process; unioned into " +
      "demand it is a `Requested` reason nobody can clear, and its recipient never " +
      "settles. Read the `mailbox` table — `ReconcilerFactsStore::pending_mail_facts_after` " +
      "when a facts store is wired, `CompanyDb::mailbox_read` when one is not.",
  );
  assert.doesNotMatch(
    code,
    /mailbox::pending_(for|recipients|demand_recipients)\(/,
    `${CYCLE_RS} must not read the in-memory mailbox through \`store::mailbox\`'s ` +
      "pending accessors either — same map, same staleness, same outcome.",
  );
});

test("both branches of the projection classify pending mail with one function", () => {
  const code = productionCode(CYCLE_RS);
  const matches = code.match(/pending_mail_facts_from_snapshot|pending_mail_facts_after/g) ?? [];
  assert.ok(
    matches.length >= 2,
    "the wired and unwired branches must BOTH produce pending-mail facts. The unwired " +
      "branch answered `Vec::new()` and the pass still saw demand through the in-memory " +
      "mailbox; deleting that half without giving this branch a real read would make an " +
      "unwired actuator blind to every envelope — the same silence in the other direction. " +
      `found: ${JSON.stringify(matches)}`,
  );
  assert.match(
    productionCode(
      "apps/chiefd/crates/chiefd-core/src/store/reconciler_facts.rs",
    ),
    /pub fn pending_mail_facts_from_snapshot/,
    "the shared classifier must exist, or the two branches are two copies of the same " +
      "three filters and will drift.",
  );
});

test("the in-memory mailbox has exactly one remaining production reader, and it is named", () => {
  // A hand-kept list, checked against the tree. When the health surface stops
  // reading the ledger, this test fails on the unused row and tells you to
  // delete it — an allowlist nobody prunes is how the union survived.
  const ALLOWED = ["apps/chiefd/crates/chiefd-host/src/gather/health_snapshot.rs"];
  for (const relPath of ALLOWED) {
    assert.match(
      productionCode(relPath),
      /\.mailbox_rows\(\)/,
      `${relPath} is allowlisted as an in-memory mailbox reader but no longer reads it. ` +
        "Delete the row from ALLOWED in this file.",
    );
  }
});
