// A conformance fixture may not claim a transport nothing executes.
//
// # Why this file exists
//
// The corpus's whole value is that a fixture can be refuted by the product. It
// has failed that twice at scale, the same way both times:
//
//   * 74 fixtures asserted `tools.launcher_calls` — an argv list whose producer
//     `#751/G9` had deleted. 38 pinned a non-empty argv that was actively false;
//     the rest asserted the empty list and were true for the wrong reason. All
//     74 were green for months (#1044).
//   * the three reminder fixtures pinned the deleted `org reminder arm|list|stop`
//     argv transport and a `createdByPersonId` request field that had left the
//     wire. Also green, because nothing executed them (#1043).
//
// Neither was a bad assertion. Both were assertions with NO SUBJECT — nothing
// anywhere ran them, so nothing could disagree. `conformance_tools.rs` counts
// how many fixtures are blocked, which is a true and useful number, but a
// blocked fixture is allowed to say anything it likes. That is the hole.
//
// # The rule
//
// A fixture that records `tools.chiefd_calls` is claiming that a tool posts a
// specific request to a route chiefd owns. That claim MUST have a subject: the
// fixture's own name must appear in a Rust conformance runner, which replays
// the recorded request at the real router. A new transport claim with no runner
// fails here, by name, on the commit that adds it.
//
// This is deliberately narrower than "every fixture must be replayed". Most of
// the `tools` family projects the extension host — registration, schemas, card
// rendering — which is TypeScript permanently and is covered by
// `packages/piing/test/toolcontract/OrganizationToolContract.test.ts`. The
// corpus machine-checks the CHIEFD half only, and a `tools.chiefd_calls` read
// is exactly the shape that claims something chiefd can answer for. Those, and
// only those, are required to have a runner.
//
// # What this guard is NOT
//
// It does not count blocked fixtures, restate the blocked/replayed split, or
// read `REPLAYED_IN_RUST`. That accounting is
// `apps/chiefd/crates/chiefd-core/tests/conformance_tools.rs`'s and has one
// owner. This asks a different question, from the fixture side: does anything
// run what this fixture claims?
//
// Run with `node --test scripts/test/conformance-fixture-subject.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** Every family of the corpus lives here, one directory each. */
export const FIXTURE_ROOT = join("conformance", "fixtures");

/** The crates whose `tests/` hold the conformance replay runners. */
export const RUNNER_DIRS = [
  join("apps", "chiefd", "crates", "chiefd-api", "tests"),
  join("apps", "chiefd", "crates", "chiefd-core", "tests"),
];

/**
 * The `tools` family's ACCOUNTING file, excluded from the runner set.
 *
 * It is not a replay: it shape-checks all 137 fixtures and records which of
 * them have no Rust subject, so it names every fixture in the family ON
 * PURPOSE. Counting it as a subject would make this guard vacuous — the first
 * version of this file did exactly that and stayed green while an orphaned
 * claim was restored underneath it, which is the failure mode it exists to
 * catch, wearing a green tick. The exclusion is checked rather than trusted:
 * see the test below.
 */
export const ACCOUNTING_FILE = "conformance_tools.rs";

/**
 * What makes the excluded file ACCOUNTING rather than a replay: it holds the
 * blocked-op vocabulary, the list of fixtures it has decided NOT to run.
 *
 * A replay names the fixtures it executes; this one names the fixtures nobody
 * executes, which is why its mentions must not count as subjects. The two
 * store-level runners (`activity`, `session-maintenance`) replay
 * against Rust stores rather than the router, so "drives the router" would have
 * been the wrong discriminator — it would have thrown them out too.
 */
export const ACCOUNTING_MARKER = "REPLAYED_IN_RUST";

/** The read whose presence makes a fixture a transport claim. */
export const TRANSPORT_CLAIM = "tools.chiefd_calls";

/** Non-vacuity floors. A guard that lost its subject looks exactly like one that passed. */
// Lowered 90 -> 79 when the task family's eleven fixtures went with the task
// feature, and again for the acknowledge/renew-generation and generation
// fixtures the same change deleted. The floor tracks a corpus that genuinely
// shrank; a fixture disappearing without this number moving is still the
// failure this guard exists to catch.
//
// Lowered 79 -> 64 when the goal feature was deleted: the whole 15-fixture
// `assignment` family (assign/result/fence — the delegation half of goals) and
// the one `org_clear_goals` transport claim in `tools` went with it.
//
// Lowered 64 -> 62 when provider/model management was deleted: `set_model` and
// `set_thinking` are no longer `MaintenanceAction` variants, so the two
// session-maintenance fixtures that queued them describe a verb the product
// does not have.
//
// Lowered 62 -> 61 when the daemon-side CEO-only boot was deleted: the daemon
// boots no pane, so `prepare-ceo-only-response.json` froze the response shape
// of a route that no longer exists. A fixture for a deleted verb is not
// coverage; it is a claim about a door nobody can knock on.
//
// Lowered 61 -> 42 on 2026-08-24, when `org_maintain_session` was deleted whole
// on the operator's ruling and took 16 session-maintenance fixtures with it —
// nine whose OP is gone, seven that exercised a surviving op ON a deleted
// action. Same principle as the line above, at sixteen times the size:
// `conformance/README.md` records the deletions, the seventeen mechanical
// amendments and why the seven were not rewritten to use `compact`.
//
// THIS FLOOR IS A MEASUREMENT, NOT A TARGET. It is the count as it stands, so
// the guard still refuses a corpus that has silently lost its subject; it is
// not a claim that 42 is the right number of fixtures.
export const MIN_FIXTURES = 42;
// Lowered 4 -> 3 for the same deletion: `conformance_assignment.rs` replayed
// the family above and had nothing left to replay.
export const MIN_RUNNERS = 3;

/** Every fixture on disk as `{ family, name, fixture }`, derived from the tree. */
export function loadFixtures(root) {
  const found = [];
  for (const family of readdirSync(root, { withFileTypes: true })) {
    if (!family.isDirectory()) continue;
    const familyDir = join(root, family.name);
    for (const entry of readdirSync(familyDir).sort()) {
      if (!entry.endsWith(".json")) continue;
      const text = readFileSync(join(familyDir, entry), "utf8");
      found.push({
        family: family.name,
        name: entry.slice(0, -".json".length),
        fixture: JSON.parse(text),
      });
    }
  }
  return found;
}

/** Does this fixture claim a transport? */
export function claimsTransport(fixture, read = TRANSPORT_CLAIM) {
  const states = fixture.expectState;
  if (!Array.isArray(states)) return false;
  return states.some((state) => state?.read === read);
}

/** The source of every `conformance_*.rs` runner, concatenated with its path. */
export function runnerSources(root, dirs) {
  const sources = [];
  for (const dir of dirs) {
    let entries;
    try {
      entries = readdirSync(join(root, dir));
    } catch {
      continue;
    }
    for (const entry of entries.sort()) {
      if (!entry.startsWith("conformance_") || !entry.endsWith(".rs")) continue;
      if (entry === ACCOUNTING_FILE) continue;
      sources.push({
        path: join(dir, entry),
        text: readFileSync(join(root, dir, entry), "utf8"),
      });
    }
  }
  return sources;
}

/**
 * Which runners name this fixture.
 *
 * The needle is the fixture's own name, so a runner cannot satisfy this by
 * merely existing — the same coupling `conformance_tools.rs`'s tripwire uses
 * across a crate boundary it cannot link.
 */
export function runnersNaming(name, sources) {
  return sources.filter((source) => source.text.includes(name)).map((source) => source.path);
}

test("the corpus and the runners are both where this guard expects them", () => {
  const fixtures = loadFixtures(join(repoRoot, FIXTURE_ROOT));
  assert.ok(
    fixtures.length >= MIN_FIXTURES,
    `only ${fixtures.length} fixtures found under ${FIXTURE_ROOT}; this guard has lost its subject`
  );
  const families = new Set(fixtures.map((entry) => entry.family));
  for (const family of ["activity", "session-maintenance", "tools"]) {
    assert.ok(families.has(family), `the '${family}' family is missing from ${FIXTURE_ROOT}`);
  }
  const sources = runnerSources(repoRoot, RUNNER_DIRS);
  assert.ok(
    sources.length >= MIN_RUNNERS,
    `only ${sources.length} conformance runners found; expected at least ${MIN_RUNNERS}`
  );
});

test("the excluded accounting file is still accounting, not a replay", () => {
  // A stale exclusion is worse than no exclusion. If `conformance_tools.rs`
  // ever starts driving the router, it becomes a real subject and dropping it
  // here would hide a fixture that genuinely has none.
  const path = join(repoRoot, RUNNER_DIRS[1], ACCOUNTING_FILE);
  const text = readFileSync(path, "utf8");
  assert.ok(
    text.includes(ACCOUNTING_MARKER),
    `${ACCOUNTING_FILE} no longer holds '${ACCOUNTING_MARKER}', so it may have stopped being the ` +
      `family's accounting file — re-decide whether excluding it from the subject set is still right`
  );
  // And the exclusion must still be doing work: this file names fixtures, which
  // is the whole reason counting it as a subject would be vacuous.
  const claims = loadFixtures(join(repoRoot, FIXTURE_ROOT)).filter((entry) =>
    claimsTransport(entry.fixture)
  );
  assert.ok(
    claims.some((entry) => text.includes(entry.name)),
    `${ACCOUNTING_FILE} names no fixture that claims a transport, so excluding it is pointless — ` +
      `check whether this guard is still asking the question it thinks it is`
  );
});

test("every transport claim in the corpus has a Rust runner that executes it", () => {
  const fixtures = loadFixtures(join(repoRoot, FIXTURE_ROOT));
  const sources = runnerSources(repoRoot, RUNNER_DIRS);
  const claims = fixtures.filter((entry) => claimsTransport(entry.fixture));

  // Non-vacuity again, and a different one: the rule is about transport claims,
  // so zero of them would make this pass over nothing.
  assert.ok(
    claims.length > 0,
    `no fixture declares a '${TRANSPORT_CLAIM}' read — either the corpus lost them or the read was renamed`
  );

  const orphans = claims
    .filter((entry) => runnersNaming(entry.name, sources).length === 0)
    .map((entry) => `${entry.family}/${entry.name}`);

  assert.deepEqual(
    orphans,
    [],
    `these fixtures claim a request the product makes, and no Rust runner replays it — so the ` +
      `claim cannot be wrong, which is how 74 launcher_calls assertions survived:\n  ` +
      `${orphans.join("\n  ")}\n` +
      `Give each one a runner under ${RUNNER_DIRS.join(" or ")}, or drop the ` +
      `'${TRANSPORT_CLAIM}' read if what it claims is a TypeScript-side fact the corpus ` +
      `must not assert.`
  );
});

// --- demonstrated red ---------------------------------------------------
//
// The checks above pass today. These prove they can fail, and for the right
// reason — a guard whose red has never been seen is a guard nobody has tested.

const CLAIM_FIXTURE = {
  expectState: [{ read: TRANSPORT_CLAIM, equals: [{ path: "/v1/x", body: {} }] }],
};
const PLAIN_FIXTURE = { expectState: [{ read: "tools.registered", equals: [] }] };

test("RED: a transport claim no runner names is an orphan", () => {
  const sources = [{ path: "conformance_reminders.rs", text: "names org-something-else" }];
  assert.equal(claimsTransport(CLAIM_FIXTURE), true);
  assert.deepEqual(runnersNaming("org-brand-new-claim", sources), []);
});

test("GREEN: a runner that names the fixture is its subject", () => {
  const sources = [
    { path: "conformance_goals.rs", text: 'const REPLAYED = &["org-brand-new-claim"];' },
  ];
  assert.deepEqual(runnersNaming("org-brand-new-claim", sources), ["conformance_goals.rs"]);
});

test("a fixture with no transport claim is not asked for a runner", () => {
  assert.equal(claimsTransport(PLAIN_FIXTURE), false);
  assert.equal(claimsTransport({}), false);
  assert.equal(claimsTransport({ expectState: "not an array" }), false);
});

test("a runner that merely exists does not satisfy the rule", () => {
  // The needle is the fixture's own name. An empty runner file is not a subject.
  const sources = [{ path: "conformance_goals.rs", text: "// TODO" }];
  assert.deepEqual(runnersNaming("org-brand-new-claim", sources), []);
});
