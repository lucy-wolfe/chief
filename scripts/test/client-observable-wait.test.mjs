// A server wait that outlives the client's patience answers nobody.
//
// # Why this file exists
//
// `POST /v1/org/person/bench-lifecycle` commits the bench, then holds its
// answer until the reconciler confirms the pane stopped, and on expiry answers
// `503 bench-convergence-timeout` whose detail begins "bench committed". That
// 503 is not a failure — it is the success, told honestly, and `org_bench`
// translates it back into one so a manager is never told a committed bench
// failed and never invited into the retry that answers `already-benched`.
//
// The whole contract is worth nothing if no caller is still listening when the
// 503 arrives. `@chief/chiefing`'s `FetchTransport` aborts at
// `DEFAULT_TIMEOUT_MS` and the org intercom builds its transport with no
// override, so while the route waited 30s the client always gave up first — 20
// seconds early, EVERY time. Worse, the abort raises `ChiefdUnavailableError`
// with `kind: 'timeout'` and no `status`, so `org_bench`'s own
// `error.status === 503` recovery could never match: the branch written to
// prevent exactly this was dead code from the day it shipped, and a committed
// bench was reported to the manager as an outage.
//
// The route's constant now carries that coupling in its doc comment. **A
// coupling documented in a comment is not enforced.** Two numbers in two files
// that must stay ordered is a second source of truth with extra steps: nothing
// in the repo went red when they crossed, and nothing would have gone red if
// they crossed again. This is the thing that goes red.
//
// # The rule
//
// **The client's patience must exceed every bound chiefd can hold it behind.**
// Not the reverse. How long chiefd is willing to queue a mutation is a backend
// policy about contention; a client that abandons first and then misreports the
// outcome is a client defect.
//
// # What it checks
//
// One list of server bounds, one margin, one assertion. The list is DERIVED,
// never written down, and it has two families:
//
//   1. Every `tokio::time::timeout(NAME, …)` in the docstore HTTP layer's
//      production Rust. A wait there is a wait inside a request handler, which
//      is exactly the class that blocks a response.
//   2. The writer actor's queue deadline, taken from the ONE production site
//      where a number enters the actor — `CompanyDb::open`'s call to
//      `open_with` — and resolved wherever that constant is defined. This is
//      `MUTATION_QUEUE_DEADLINE` today.
//
// Family 2 is here because the first version of this guard left it out, on the
// reasoning that it is "not a wait inside an HTTP handler". That reasoning was
// wrong in the only way that matters: it is a wait a request sits behind, and
// the answer it produces still has to reach somebody. It is worse than the
// bench route, in fact. A mutation that waits longer than the client's patience
// but LESS than the deadline is never reaped — it runs, and commits, while the
// caller has already been told `kind: 'timeout'`, which `isTransientChiefdError`
// treats as non-transient so nothing retries and nothing re-reads. The caller
// believes the write did not happen; the write happened. Two guards answering
// "can the client observe this refusal?" would be the second-source-of-truth
// defect all over again, so this one grew a second family instead.
//
// A third check keeps the number this guard reads honest: no production code
// may supply a `FetchTransport` with a patience of its own. `ChiefdClient`
// carried `private readonly defaultTimeoutMs = 10_000` and passed it into every
// transport it built, so the guard's "one definition" was one definition the
// main client never used — raising `DEFAULT_TIMEOUT_MS` alone would have left
// the defect live for `ChiefdClient` and `apps/web` with this file still green.
//
// Deliberately NOT covered, each for a stated reason:
//
//   * `#[cfg(test)]` modules — a test legitimately writes a short bound out in
//     full; that is what makes it a test.
//   * The docstore engine's 5s SQLite `busy_timeout`, which mints the
//     `store-contended` 429. It is a `pragma_update` literal, not a named
//     bound, and it is an order of magnitude inside the client either way.
//   * `chief-cli`'s per-call Rust budgets (`http.rs`, `listing.rs`). They are a
//     second client with its own patience, in a crate this guard's subject
//     directories do not reach. Named here as a known gap rather than left to
//     be discovered.
//
// Run with `node --test scripts/test/client-observable-wait.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { skipSet } from "../tree-walk-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** The docstore HTTP layer: every route module a request handler lives in. */
export const DOCSTORE_DIR = join(
  "apps",
  "chiefd",
  "crates",
  "chiefd-api",
  "src",
  "docstore"
);

/** The client's patience, read from its one definition rather than retyped. */
export const TRANSPORT_FILE = join(
  "packages",
  "chiefing",
  "src",
  "transport",
  "FetchTransport.ts"
);

/**
 * The writer actor: where the queue deadline is spent and where it is defined.
 * `writer.rs` holds the one production site that hands the actor its deadline;
 * `mod.rs` holds the constant. The whole directory is read, so a move between
 * the two files (or into a third) does not blind the guard.
 */
export const ACTOR_DIR = join(
  "apps",
  "chiefd",
  "crates",
  "chiefd-core",
  "src",
  "actor"
);

/** The file that opens a company writer in production. */
export const WRITER_FILE = "writer.rs";

const TRANSPORT_TIMEOUT = /^const DEFAULT_TIMEOUT_MS = ([\d_]+)$/m;

/**
 * The margin a server wait must leave the client. It covers connect, the JSON
 * round trip and the reconcile wake — everything that happens on the wire
 * around the wait itself. A bound that merely squeaks under the abort is one
 * slow handshake away from the defect this guard exists for.
 */
export const REQUIRED_MARGIN_MS = 2_000;

/** `10_000` / `250` — the Rust and TS spellings of an integer literal. */
function parseIntegerLiteral(text) {
  if (!/^[\d_]+$/.test(text)) return undefined;
  const value = Number(text.replaceAll("_", ""));
  return Number.isFinite(value) ? value : undefined;
}

/**
 * `DEFAULT_TIMEOUT_MS`, in milliseconds, or `undefined` when the definition is
 * not the shape this guard can trust. An unreadable definition FAILS: a guard
 * that silently skips a subject it cannot parse is worse than no guard.
 */
export function parseTransportTimeoutMs(source) {
  const match = source.match(TRANSPORT_TIMEOUT);
  return match ? parseIntegerLiteral(match[1]) : undefined;
}

/**
 * Production source: every `#[cfg(test)]` module cut out by brace matching.
 *
 * NOT a split on the first `#[cfg(test)]`, which is what the sibling guards do.
 * `docstore/router.rs` interleaves five test modules through 8k lines, and the
 * wait this guard exists for sits AFTER the first of them — a naive split
 * dropped the subject and left the guard passing over nothing. The technique is
 * the one `router.rs`'s own in-tree helper already uses on itself.
 *
 * Brace counting is textual, so a `{` inside a string literal or comment inside
 * a test module could mis-span. That is the same limit `rust-ts-shape-drift`
 * states for its regex parsing, and the failure mode is a subject wrongly
 * dropped — which the non-vacuity floor below turns into a red, not a silent
 * pass.
 */
export function productionSource(text) {
  let out = text;
  for (;;) {
    const start = out.indexOf("#[cfg(test)]");
    if (start === -1) return out;
    const open = out.indexOf("{", start);
    if (open === -1) return out.slice(0, start);
    let depth = 0;
    let index = open;
    for (; index < out.length; index += 1) {
      if (out[index] === "{") depth += 1;
      else if (out[index] === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    out = out.slice(0, start) + out.slice(index + 1);
  }
}

/** Every `tokio::time::timeout(NAME, …)` bound named by a constant. */
export function namedWaitBounds(source) {
  const names = new Set();
  for (const match of productionSource(source).matchAll(
    /tokio::time::timeout\(\s*([A-Z][A-Z0-9_]*)\s*,/g
  )) {
    names.add(match[1]);
  }
  return [...names].sort();
}

/**
 * `const NAME: Duration = Duration::from_secs(6);` -> 6000. Returns `undefined`
 * for a definition this guard cannot read, which its caller turns into a
 * failure rather than a skip.
 */
export function parseDurationConstMs(source, name) {
  const pattern = new RegExp(
    `const\\s+${name}\\s*:\\s*Duration\\s*=\\s*Duration::from_(secs|millis)\\(([\\d_]+)\\)\\s*;`
  );
  const match = productionSource(source).match(pattern);
  if (!match) return undefined;
  const value = parseIntegerLiteral(match[2]);
  if (value === undefined) return undefined;
  return match[1] === "secs" ? value * 1000 : value;
}

/** Every `.rs` file directly under a route/actor directory. */
export function rustFilesIn(dir) {
  return readdirSync(dir)
    .filter((name) => name.endsWith(".rs"))
    .sort();
}

/**
 * Every named wait in the docstore HTTP layer, resolved to milliseconds.
 * `{ file, name, ms }`, with `ms: undefined` for a definition that could not be
 * read in the file that uses it.
 */
export function collectHandlerWaits(rootDir) {
  const waits = [];
  for (const file of rustFilesIn(rootDir)) {
    const source = readFileSync(join(rootDir, file), "utf8");
    for (const name of namedWaitBounds(source)) {
      waits.push({ file, name, ms: parseDurationConstMs(source, name) });
    }
  }
  return waits;
}

/**
 * The constant a production `CompanyDb::open` hands the writer as its queue
 * deadline, by name.
 *
 * DERIVED from the one place a number enters the actor rather than looked up by
 * name: `Self::open_with(label, path, clock, AgingPolicy::default(), NAME)`.
 * `open_with` itself takes the deadline as its last argument precisely so the
 * scheduler tests can drive a short one, and every test that does lives inside
 * a `#[cfg(test)]` module this parser has already removed — so the only site
 * left is the production one.
 *
 * Returns `undefined` when no such site can be read: an inlined
 * `Duration::from_secs(45)` at the call, a reordered argument list, or a
 * renamed constructor all land here, and the caller turns every one of them
 * into a failure. A guard that cannot find its subject has lost it.
 */
export function actorQueueDeadlineName(writerSource) {
  const matches = [
    ...productionSource(writerSource).matchAll(
      /Self::open_with\([^;]*?,\s*([A-Z][A-Z0-9_]*)\s*\)/g
    )
  ];
  if (matches.length !== 1) return undefined;
  return matches[0][1];
}

/**
 * The writer actor's queue deadline as `[{ file, name, ms }]` — the same shape
 * as a handler wait, so both families share one assertion.
 *
 * The constant is resolved against every production `.rs` in the actor
 * directory, not just the file that spends it: it is declared in `mod.rs` and
 * used in `writer.rs` today, and a move between siblings is a refactor, not a
 * defect.
 */
export function collectActorQueueBounds(actorDir) {
  const name = actorQueueDeadlineName(
    readFileSync(join(actorDir, WRITER_FILE), "utf8")
  );
  if (name === undefined) return [];
  let ms;
  for (const file of rustFilesIn(actorDir)) {
    const resolved = parseDurationConstMs(
      readFileSync(join(actorDir, file), "utf8"),
      name
    );
    if (resolved !== undefined) ms = resolved;
  }
  return [{ file: WRITER_FILE, name, ms }];
}

/**
 * Every bound chiefd can hold a request behind, both families, one list.
 * `{ where, file, name, ms }`.
 */
export function collectServerBounds(root) {
  return [
    ...collectHandlerWaits(join(root, DOCSTORE_DIR)).map((wait) => ({
      where: DOCSTORE_DIR,
      ...wait
    })),
    ...collectActorQueueBounds(join(root, ACTOR_DIR)).map((wait) => ({
      where: ACTOR_DIR,
      ...wait
    }))
  ];
}

/**
 * Directories that hold no production TypeScript.
 *
 * The shared members - build output, and the checkouts that are not this one -
 * come from `tree-walk-lib`, which is where the `.claude/worktrees/<name>/`
 * exclusion this guard needed first now lives for every walking guard. The
 * additions here are this guard's own subject: test trees are not production.
 */
const NOT_PRODUCTION = skipSet(["test", "tests", "__tests__"]);

/** Every production `.ts` file in the repo, tests and build output excluded. */
export function productionTsFiles(root, prefix = "") {
  const out = [];
  for (const entry of readdirSync(join(root, prefix), { withFileTypes: true })) {
    const rel = prefix === "" ? entry.name : join(prefix, entry.name);
    if (entry.isDirectory()) {
      if (NOT_PRODUCTION.has(entry.name)) continue;
      out.push(...productionTsFiles(root, rel));
    } else if (entry.name.endsWith(".ts") && !entry.name.endsWith(".test.ts")) {
      out.push(rel);
    }
  }
  return out.sort();
}

/**
 * The argument list of every `new FetchTransport(...)` in `source`, split at
 * top-level commas. Nesting is tracked so `() => manager.authHeader()` stays
 * one argument.
 */
export function transportConstructionArgs(source) {
  const calls = [];
  const marker = "new FetchTransport(";
  for (let at = source.indexOf(marker); at !== -1; at = source.indexOf(marker, at + 1)) {
    let depth = 0;
    let current = "";
    const args = [];
    for (let index = at + marker.length - 1; index < source.length; index += 1) {
      const ch = source[index];
      if (ch === "(" || ch === "[" || ch === "{") {
        depth += 1;
        if (depth === 1) continue;
      } else if (ch === ")" || ch === "]" || ch === "}") {
        depth -= 1;
        if (depth === 0) {
          if (current.trim() !== "") args.push(current.trim());
          break;
        }
      } else if (ch === "," && depth === 1) {
        args.push(current.trim());
        current = "";
        continue;
      }
      current += ch;
    }
    calls.push(args);
  }
  return calls;
}

test("the transport's own patience is readable from its one definition", () => {
  const ms = parseTransportTimeoutMs(
    readFileSync(join(repoRoot, TRANSPORT_FILE), "utf8")
  );
  assert.equal(
    typeof ms,
    "number",
    `${TRANSPORT_FILE} must define DEFAULT_TIMEOUT_MS as a plain integer literal`
  );
  assert.ok(ms > 0, "the client abort must be a positive duration");
});

test("every bound chiefd can hold a request behind expires inside the client's abort", () => {
  const clientMs = parseTransportTimeoutMs(
    readFileSync(join(repoRoot, TRANSPORT_FILE), "utf8")
  );
  const bounds = collectServerBounds(repoRoot);

  // Non-vacuity, per family. A refactor that renamed the call, moved the
  // directory or dropped the wait would otherwise leave this guard passing over
  // nothing — the exact failure mode the guard exists to catch, wearing a green
  // tick. The first version of the docstore half was green over nothing for
  // precisely this reason and this floor is what caught it.
  assert.ok(
    bounds.some((bound) => bound.where === DOCSTORE_DIR),
    `no named wait found under ${DOCSTORE_DIR}: this guard has lost its subject`
  );
  assert.ok(
    bounds.some((bound) => bound.name === "BENCH_COMPLETION_TIMEOUT"),
    "BENCH_COMPLETION_TIMEOUT is the wait this guard was written for; it must still be found"
  );
  assert.ok(
    bounds.some((bound) => bound.where === ACTOR_DIR),
    `no queue deadline derived from ${join(ACTOR_DIR, WRITER_FILE)}: ` +
      "the writer's production open_with site is this guard's second subject"
  );
  assert.ok(
    bounds.some((bound) => bound.name === "MUTATION_QUEUE_DEADLINE"),
    "MUTATION_QUEUE_DEADLINE is the writer actor's single bounded wait; it must still be found"
  );

  for (const { where, file, name, ms } of bounds) {
    assert.equal(
      typeof ms,
      "number",
      `${join(where, file)} awaits ${name}, whose Duration definition this guard cannot read`
    );
    assert.ok(
      ms + REQUIRED_MARGIN_MS <= clientMs,
      `${name} (${ms}ms) must expire at least ${REQUIRED_MARGIN_MS}ms before the client aborts ` +
        `at DEFAULT_TIMEOUT_MS (${clientMs}ms), or its response reaches nobody — ` +
        `see ${join(where, file)} and ${TRANSPORT_FILE}`
    );
  }
});

test("no production code supplies a chiefd transport with a patience of its own", () => {
  const offenders = [];
  let sites = 0;
  const files = productionTsFiles(repoRoot);
  for (const file of files) {
    const source = readFileSync(join(repoRoot, file), "utf8");
    if (!source.includes("new FetchTransport(")) continue;
    for (const args of transportConstructionArgs(source)) {
      sites += 1;
      if (args.length >= 2 && args[1] !== "undefined") {
        offenders.push(`${file}: new FetchTransport(…, ${args[1]}, …)`);
      }
    }
  }

  // Non-vacuity: the walk must actually reach the production construction
  // sites. A skipped directory or a renamed class would otherwise make this
  // pass over an empty set.
  assert.ok(
    sites >= 3,
    `only ${sites} production FetchTransport construction site(s) found: this check has lost its subject`
  );
  assert.ok(
    files.includes(join("packages", "chiefing", "src", "ChiefdClient.ts")),
    "ChiefdClient.ts is the site that carried a second patience; the walk must reach it"
  );

  assert.deepEqual(
    offenders,
    [],
    "the client's patience has exactly one definition — DEFAULT_TIMEOUT_MS in " +
      `${TRANSPORT_FILE} — and it is the number this guard holds against chiefd's ` +
      "bounds. A production caller that passes its own keeps the guard green while " +
      `the effective patience drifts: ${offenders.join("; ")}`
  );
});

// --- demonstrated red, both directions --------------------------------------
//
// The checks above pass today. These prove they can fail, and fail for the
// right reason: a guard whose red has never been seen is a guard nobody has
// tested.

const RED_ROUTER = `
use std::time::Duration;
const BENCH_COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);
async fn handler() {
    let _ = tokio::time::timeout(BENCH_COMPLETION_TIMEOUT, completion).await;
}
#[cfg(test)]
mod tests {
    const NOT_A_SUBJECT: Duration = Duration::from_secs(600);
}
`;

const GREEN_ROUTER = RED_ROUTER.replace("from_secs(30)", "from_secs(6)");

test("RED: the server wait drifting past the client abort is caught", () => {
  const ms = parseDurationConstMs(RED_ROUTER, "BENCH_COMPLETION_TIMEOUT");
  assert.equal(ms, 30_000, "the pre-fix value is read as 30s");
  assert.ok(
    !(ms + REQUIRED_MARGIN_MS <= 10_000),
    "30s must NOT fit inside a 10s client abort — this is the defect"
  );
});

test("GREEN: the landed value fits, with the margin", () => {
  const ms = parseDurationConstMs(GREEN_ROUTER, "BENCH_COMPLETION_TIMEOUT");
  assert.equal(ms, 6_000);
  assert.ok(ms + REQUIRED_MARGIN_MS <= 10_000);
});

test("RED: shrinking the CLIENT timeout crosses the same line", () => {
  // The coupling has two ends. A transport change is the direction nobody
  // watches, because it looks like a client-only edit.
  const clientMs = parseTransportTimeoutMs("const DEFAULT_TIMEOUT_MS = 5_000\n");
  assert.equal(clientMs, 5_000);
  const serverMs = parseDurationConstMs(GREEN_ROUTER, "BENCH_COMPLETION_TIMEOUT");
  assert.ok(
    !(serverMs + REQUIRED_MARGIN_MS <= clientMs),
    "a 5s client abort must NOT admit a 6s server wait"
  );
});

test("a wait whose bound cannot be parsed is a failure, never a skip", () => {
  assert.equal(parseDurationConstMs("const X: Duration = FOREVER;", "X"), undefined);
  assert.equal(parseDurationConstMs(RED_ROUTER, "ABSENT"), undefined);
  assert.equal(parseTransportTimeoutMs("const DEFAULT_TIMEOUT_MS = someOther\n"), undefined);
});

test("a bound defined only inside #[cfg(test)] is not a production subject", () => {
  assert.deepEqual(namedWaitBounds(RED_ROUTER), ["BENCH_COMPLETION_TIMEOUT"]);
  assert.equal(parseDurationConstMs(RED_ROUTER, "NOT_A_SUBJECT"), undefined);
});

test("a test module EARLIER in the file does not hide the production wait after it", () => {
  // `docstore/router.rs`'s shape, minimised. A split on the first
  // `#[cfg(test)]` — what the sibling guards do, because their subjects put the
  // test module last — silently drops everything below it, and this guard's one
  // subject lives down there.
  const interleaved = `
#[cfg(test)]
mod early_tests {
    fn helper() { let _ = 1; }
}
const BENCH_COMPLETION_TIMEOUT: Duration = Duration::from_secs(6);
async fn handler() {
    let _ = tokio::time::timeout(BENCH_COMPLETION_TIMEOUT, completion).await;
}
`;
  assert.deepEqual(namedWaitBounds(interleaved), ["BENCH_COMPLETION_TIMEOUT"]);
  assert.equal(parseDurationConstMs(interleaved, "BENCH_COMPLETION_TIMEOUT"), 6_000);
});

// --- the writer actor's queue deadline, red in both directions ---------------

/** `writer.rs`'s shape, minimised: the production `open` site, plus a test
 *  module that opens the same writer with a deliberately short deadline. Both
 *  spellings are real — `open_with` exists so scheduler tests can drive one. */
const RED_WRITER = `
impl CompanyDb {
    pub fn open(label: &str, path: &Path, clock: Arc<dyn Clock>) -> Result<Self, OpenError> {
        Self::open_with(label, path, clock, AgingPolicy::default(), MUTATION_QUEUE_DEADLINE)
    }
}
#[cfg(test)]
mod scheduler_tests {
    fn harness() -> CompanyDb {
        CompanyDb::open_with(label, path, clock, AgingPolicy::default(), TEST_DEADLINE).unwrap()
    }
}
`;

const ACTOR_MOD = `
pub const MUTATION_QUEUE_DEADLINE: Duration = Duration::from_secs(30);
pub const AGING_INTERVAL: Duration = Duration::from_secs(2);
`;

test("the queue deadline is derived from the writer's production site, never named", () => {
  assert.equal(actorQueueDeadlineName(RED_WRITER), "MUTATION_QUEUE_DEADLINE");

  // Follow a rename rather than assert one. A guard that greps for the name it
  // was written against reports "subject missing" on a rename; this one reports
  // the new bound.
  const renamed = RED_WRITER.replaceAll("MUTATION_QUEUE_DEADLINE", "WRITER_QUEUE_BOUND");
  assert.equal(actorQueueDeadlineName(renamed), "WRITER_QUEUE_BOUND");

  // The test module's own short deadline is not a subject, and its presence
  // does not make the production site ambiguous.
  assert.equal(parseDurationConstMs(RED_WRITER, "TEST_DEADLINE"), undefined);
});

test("RED: the writer's queue deadline outliving the client abort is caught", () => {
  const name = actorQueueDeadlineName(RED_WRITER);
  const ms = parseDurationConstMs(ACTOR_MOD, name);
  assert.equal(ms, 30_000, "the pre-fix value is read as 30s");
  // The exact defect: a mutation queued behind deep work is answered — a commit
  // or the reaped 429 — after the client already gave up and called it a
  // non-transient timeout.
  assert.ok(
    !(ms + REQUIRED_MARGIN_MS <= 10_000),
    "a 30s queue deadline must NOT fit inside a 10s client abort"
  );
  // …and the landed client patience admits it.
  assert.ok(ms + REQUIRED_MARGIN_MS <= 35_000);
});

test("RED: growing the SERVER bound past the landed client abort is caught", () => {
  const grown = ACTOR_MOD.replace("from_secs(30)", "from_secs(60)");
  const ms = parseDurationConstMs(grown, actorQueueDeadlineName(RED_WRITER));
  assert.equal(ms, 60_000);
  assert.ok(
    !(ms + REQUIRED_MARGIN_MS <= 35_000),
    "a 60s queue deadline must NOT fit inside a 35s client abort"
  );
});

test("RED: shrinking the CLIENT below the queue deadline crosses the same line", () => {
  // The direction nobody watches, because it looks like a client-only edit —
  // and the one that produced this defect in the first place.
  const clientMs = parseTransportTimeoutMs("const DEFAULT_TIMEOUT_MS = 10_000\n");
  assert.equal(clientMs, 10_000);
  const ms = parseDurationConstMs(ACTOR_MOD, actorQueueDeadlineName(RED_WRITER));
  assert.ok(
    !(ms + REQUIRED_MARGIN_MS <= clientMs),
    "a 10s client abort must NOT admit a 30s queue deadline"
  );
});

test("a queue deadline this guard cannot read is a failure, never a skip", () => {
  // An inlined duration at the call site: nothing to resolve, nothing to
  // compare, so the subject is reported missing rather than assumed fine.
  assert.equal(
    actorQueueDeadlineName(
      RED_WRITER.replace("MUTATION_QUEUE_DEADLINE", "Duration::from_secs(45)")
    ),
    undefined
  );
  // Two production sites is two policies; the guard refuses to pick one.
  assert.equal(actorQueueDeadlineName(RED_WRITER + RED_WRITER), undefined);
  // A resolvable name whose definition is absent still resolves to undefined,
  // which the bound loop turns into a failure.
  assert.equal(parseDurationConstMs(ACTOR_MOD, "ABSENT_DEADLINE"), undefined);
});

// --- one definition of the client's patience, red in both directions ---------

test("RED: a production transport carrying its own patience is caught", () => {
  // `ChiefdClient.ts` as it was: a second `defaultTimeoutMs = 10_000` that
  // silently overrode the constant this guard reads, for every caller of the
  // main client.
  const [args] = transportConstructionArgs(
    "new FetchTransport(options.url, options.timeoutMs ?? this.defaultTimeoutMs, a, b)"
  );
  assert.equal(args.length, 4);
  assert.notEqual(args[1], "undefined");

  // A bare literal is the same defect, spelled shorter.
  const [literal] = transportConstructionArgs("new FetchTransport(url, 10_000)");
  assert.equal(literal[1], "10_000");
});

test("GREEN: every production spelling that inherits the one definition", () => {
  assert.deepEqual(transportConstructionArgs("new FetchTransport(endpoint.url)"), [
    ["endpoint.url"]
  ]);
  // Nesting must not split an argument: the auth hooks are arrow functions with
  // their own parentheses and commas.
  assert.deepEqual(
    transportConstructionArgs(
      "new FetchTransport(endpoint.url, undefined, () => manager.authHeader(), () => manager.invalidate())"
    ),
    [
      [
        "endpoint.url",
        "undefined",
        "() => manager.authHeader()",
        "() => manager.invalidate()"
      ]
    ]
  );
});

test("the production TypeScript walk excludes tests and build output", () => {
  const files = productionTsFiles(repoRoot);
  assert.ok(files.length >= 50, "the walk must reach the workspace's production TypeScript");
  assert.ok(
    !files.some((file) => file.includes(`${sep}dist${sep}`) || file.endsWith(".test.ts")),
    "built output and test files are not production construction sites"
  );
});
