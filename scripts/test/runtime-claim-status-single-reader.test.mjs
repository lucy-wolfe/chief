// A RELEASED runtime-ownership claim is not a placement, and the handoff that
// releases one must claim again.
//
// # The defect this closes
//
// One row, `runtime_owner`, is read by two processes for the same purpose —
// "which socket does this company run on?" — and until 2026-08-19 they asked
// two different questions:
//
//   * the daemon (`chiefd-host/src/gather/reconciler_facts.rs`,
//     `active_runtime_owner_socket`) filtered on `status == "active"`;
//   * the client (`chief-cli/src/company.rs`) read `socketName` off the row
//     and never looked at `status`.
//
// That cost nothing while a claim could only be released by `chief stop`,
// which takes the company down with it. `8ff573ff6` made a stale claim
// recoverable — adopt it, prove it dead, RELEASE it, restart onto the client's
// own preference — and from that moment a live company had a released row.
// Measured 2026-08-18: the daemon on `qa`, the CEO pane and both rails on
// `default`, because `chief actuate` obeyed the released row. `default` is the
// server every bare `tmux` lands on, the one `cb63690a0` exists to keep
// companies off, and the one whose last-session-exit took eleven panes off a
// live company that same day.
//
// The second half of the same finding: the handoff RELEASED and nothing
// re-claimed. A claim is minted only inside
// `runtime_lifecycle::claim_ownership`, which only `launch_runtime` and
// `stop_supervised_runtime` call — the runtime projecting or tearing down a
// session. A post-handoff boot does neither; the people come back from durable
// start intent through the converge loop. So the company ran holding no claim
// at all, which is exactly the state the shadow-fleet guard exists to make
// impossible: a second `chief` in that directory meets no claim to contradict.
//
// # What is checked, and why it is checked HERE
//
// Both halves have ordinary Rust unit tests over the real reader and the real
// verb (`company::tests`). Neither can see the two facts below, which are
// about WHERE the code is:
//
//   1. `socketName` is read out of the runtime-owner row in exactly ONE place
//      in the client, so a second reader cannot re-introduce the divergence by
//      spelling the field again somewhere the status filter is not.
//   2. `reconcile_runtime_claim` — the one path in the product that releases a
//      claim for a company that stays UP — both releases and claims, and
//      claims AFTER it restarts the daemon, because the socket it is claiming
//      is the restarted daemon's own.
//
// Run with `node --test scripts/test/runtime-claim-status-single-reader.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const CLIENT = join(repoRoot, "apps", "chiefd", "crates", "chief-cli", "src");

/** The one accessor in the client that may read the row's socket. */
const READER = "active_runtime_owner_socket";

function read(...parts) {
  return readFileSync(join(CLIENT, ...parts), "utf8");
}

/** Every Rust source file in the operator client. */
function clientSources(dir = CLIENT, collected = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) clientSources(join(dir, entry.name), collected);
    else if (entry.name.endsWith(".rs")) collected.push(join(dir, entry.name));
  }
  return collected;
}

/** A file with its `#[cfg(test)]` module cut off. */
function production(source) {
  const tests = source.indexOf("#[cfg(test)]");
  return tests === -1 ? source : source.slice(0, tests);
}

/** The body of a `fn <name>` / `async fn <name>`, by brace balance. */
function functionBody(source, name) {
  const signature = source.indexOf(`fn ${name}(`);
  assert.notEqual(signature, -1, `no fn ${name} to check`);
  const open = source.indexOf("{", signature);
  assert.notEqual(open, -1, `fn ${name} has no body`);
  let depth = 0;
  for (let at = open; at < source.length; at += 1) {
    if (source[at] === "{") depth += 1;
    else if (source[at] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, at + 1);
    }
  }
  throw new Error(`fn ${name} has an unbalanced body`);
}

test('the client reads "socketName" in exactly one accessor, and that accessor filters on status', () => {
  const company = read("company.rs");
  const readers = [];
  for (const file of clientSources()) {
    // The PRODUCTION half only. A test's stub row is a fixture of the wire
    // shape, not a second reader of it.
    production(readFileSync(file, "utf8"))
      .split("\n")
      .forEach((line, index) => {
        if (line.includes('.get("socketName")')) {
          readers.push({ file: relative(repoRoot, file), at: index + 1 });
        }
      });
  }
  assert.equal(
    readers.length,
    1,
    `the runtime-owner row's socket must be read in exactly one place in the client, so a ` +
      `future reader cannot skip the status filter the way this one did. Found: ` +
      JSON.stringify(readers),
  );

  const body = functionBody(company, READER);
  assert.match(
    body,
    /"status"/,
    `${READER} must ask the same question the daemon's namesake asks: a released claim names ` +
      `no socket. Without the filter a company handed off between sockets is projected onto the ` +
      `socket it was released from.`,
  );
  assert.match(body, /"active"/, `${READER} must accept only an ACTIVE claim`);
});

test("the handoff releases and re-claims, and claims after the restart", () => {
  const body = functionBody(read("attach.rs"), "reconcile_runtime_claim");
  const released = body.indexOf("release_runtime_ownership()");
  const restarted = body.indexOf("daemon::start(");
  const claimed = body.indexOf("claim_runtime_ownership()");

  assert.notEqual(released, -1, "the handoff must release the claim it proved dead");
  assert.notEqual(restarted, -1, "the handoff must restart the daemon onto the new socket");
  assert.notEqual(
    claimed,
    -1,
    "the handoff must CLAIM again. Nothing else re-mints a claim after it: a claim is minted " +
      "only when the runtime projects or tears down a session, and a post-handoff boot does " +
      "neither. Without this the company runs holding no claim and the shadow-fleet guard has " +
      "nothing to contradict.",
  );
  assert.ok(
    released < restarted && restarted < claimed,
    "the order is release, restart, claim: the socket being claimed is the RESTARTED daemon's " +
      "own, so claiming before the restart would record the socket being handed off FROM",
  );
});
