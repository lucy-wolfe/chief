// The settle window is ONE number, written down once in Rust, and the footer's
// copy of it is held to that by machine rather than by a comment.
//
// # Why this file exists
//
// The operator asked, repeatedly, for one thing: at most two minutes from the
// moment an agent settles to the moment its pane is gone. He was shown
// `shutting down in 3m 47s`. Nothing lied — the footer really did count to the
// instant of the kill — but three durations had each been chosen on their own
// merits and simply stacked:
//
//   * `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS` — idle to park admission;
//   * `HANDOFF_GRACE_MS`, added when the park was minted — admission to
//     `Overdue`;
//   * `ORGANIZATION_AUTOMATIC_PARK_OVERDUE_LEASE_MS` — `Overdue` to `Forced`.
//
// 120s + 120s + 120s = six minutes against a stated cap of two. The last two
// are now DELETED, not shortened and not zeroed: a routine idle park is minted
// already terminal, so the quiet lease is the entire window. This guard exists
// to keep it that way, and to catch the thing no compiler can see.
//
// # The thing no compiler can see
//
// `packages/piing/extensions/team-ui.ts` carries a HAND COPY of the window,
// because a Pi extension is copied verbatim into each person's pi-home and
// cannot import launcher `src/`. That copy had already drifted and drifted
// SILENTLY, under a comment swearing it had not: it read `60_000` while the
// Rust authority said `120 * 1_000`, cited a TypeScript module that no longer
// exists, and told the reader to keep the two in sync by hand. Half the real
// value, and with the grace gone that copy is the WHOLE behaviour rather than
// one term in a sum.
//
// Same disease and same shape as `kill-probe-single-definition.test.mjs` (four
// `kill(pid, 0)` probes, two of them inverted) and
// `beacond-port-single-definition.test.mjs`.
//
// # What it checks
//
//   1. The Rust window IS the operator's two minutes.
//   2. The footer's copy equals the Rust window, by value.
//   3. The deleted grace has not come back — in Rust as a constant, or in the
//      footer as anything added to a transition deadline.
//
// Every expected value is READ from its definition rather than retyped, except
// the two minutes itself, which is the requirement and therefore the one thing
// this file is entitled to state.
//
// Run with `node --test scripts/test/settle-budget-single-definition.test.mjs`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

const ACTIVITY_RS = "apps/chiefd/crates/chiefd-core/src/store/activity.rs";
const TEAM_UI_TS = "packages/piing/extensions/team-ui.ts";

/** The operator's requirement, in milliseconds. The one literal this file owns.
 *
 * 2026-08-24: *"lets bump the 2mins to a 5mins."* It was 2 minutes from the
 * 2026-08-10 ruling; the number moves only when the operator moves it, which is
 * the whole reason this literal lives here rather than being read from the
 * code it checks. */
const OPERATOR_CAP_MS = 5 * 60 * 1000;

function read(relativePath) {
  return readFileSync(join(repoRoot, relativePath), "utf8");
}

/**
 * `NAME: i64 = <a> * <b>;` or `= <n>;`, evaluated.
 *
 * Deliberately narrow: this repo writes these as `120 * 1_000`, and accepting
 * an arbitrary expression would mean reimplementing const evaluation inside a
 * guard. A definition this cannot parse FAILS rather than being skipped — a
 * silently-skipped assertion is the exact failure mode this file exists to
 * prevent.
 */
function constMs(source, declaration, where) {
  const match = source.match(new RegExp(`${declaration}\\s*=\\s*([0-9_]+)(?:\\s*\\*\\s*([0-9_]+))?;`));
  assert.ok(match, `${where} must define ${declaration} as a plain millisecond constant`);
  const left = Number(match[1].replaceAll("_", ""));
  const right = match[2] === undefined ? 1 : Number(match[2].replaceAll("_", ""));
  return left * right;
}

/** Production Rust: `#[cfg(test)]` modules removed, per this repo's convention
 * that the test module is the last item in a file (the same assumption
 * `beacond-port-single-definition.test.mjs` makes and states). */
function productionRust(text) {
  return text.split(/^#\[cfg\(test\)\]/m)[0];
}

test("the settle window is the operator's five minutes", () => {
  const rust = productionRust(read(ACTIVITY_RS));
  assert.equal(
    constMs(rust, "pub const ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS: i64", ACTIVITY_RS),
    OPERATOR_CAP_MS,
    "idle -> pane-down is one duration and it must be 5 minutes: 'lets bump the 2mins to a 5mins'",
  );
});

test("the footer's settle countdown mirrors the Rust window by value", () => {
  const rust = productionRust(read(ACTIVITY_RS));
  const ts = read(TEAM_UI_TS);
  assert.equal(
    constMs(ts, "const SETTLE_QUIET_LEASE_MS", TEAM_UI_TS),
    constMs(rust, "pub const ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS: i64", ACTIVITY_RS),
    "team-ui's SETTLE_QUIET_LEASE_MS must equal chiefd's settle window — this is the copy " +
      "that sat at half the real value under a comment asserting it had not drifted, and " +
      "with the grace deleted it is the whole countdown the operator reads",
  );
});

test("the deleted settle grace has not come back", () => {
  const rust = productionRust(read(ACTIVITY_RS));
  for (const name of [
    "IDLE_AUTO_PARK_HANDOFF_GRACE_MS",
    "ORGANIZATION_AUTOMATIC_PARK_OVERDUE_LEASE_MS",
  ]) {
    assert.ok(
      !new RegExp(`const ${name}\\s*:`).test(rust),
      `${name} is deleted, not zeroed: a routine idle park is minted already terminal, so ` +
        "there is no interval between admitting it and the pane going away. A constant at " +
        "zero would be a fallback in disguise and the next reader would restore it to 2min.",
    );
  }

  const ts = read(TEAM_UI_TS);
  // Comments may NAME the deleted constants — the tombstones in both files
  // deliberately do, and a guard that forbade naming them in prose would erase
  // the only record of why this rule exists. Only a DEFINITION or a live
  // addition to a deadline is forbidden.
  for (const name of ["SETTLE_HANDOFF_GRACE_MS", "SETTLE_FORCE_KILL_GRACE_MS"]) {
    assert.ok(
      !new RegExp(`^\\s*const ${name}\\s*=`, "m").test(ts),
      `${name} is deleted along with the window it measured; the footer renders one ` +
        "countdown, straight off idleSince, and adds nothing to it",
    );
  }
  const live = ts
    .split("\n")
    .filter((line) => !/^\s*(\/\/|\*|\/\*)/.test(line))
    .join("\n");
  assert.ok(
    !/handoffDeadlineAt/.test(live),
    "the footer must not read a transition's handoffDeadlineAt at all: adding a grace to it " +
      "is what put 'shutting down in 3m 47s' on the operator's screen",
  );
});
