# Conformance fixture format

Normative description of the JSON in `conformance/fixtures/`. This file is the
contract between a fixture and the Rust runner that replays it. Nothing in a
fixture may reference a language, a module, a file path, or a wall-clock
instant.

## Layout

```
conformance/
  FORMAT.md                     this file
  README.md                     what records a fixture, and how to add one
  fixtures/<family>/<name>.json the corpus itself — one directory per store/op family
```

The runners live with the code they replay, in
`apps/chiefd/crates/*/tests/conformance_*.rs`. #1047 deleted the TypeScript
harness (`record-ts.ts`, `run-ts.ts`, `lib/`, `scenarios/`); see README.

`<family>` is the store/op family (`activity`,
`session-maintenance`, `tools`, …). A fixture's `family` field and its `name` field must
agree with its directory and filename; the loader rejects mismatches, so a
fixture cannot be moved without being renamed.

## A fixture

```json
{
  "name": "inv-c1-unfenced-requires-the-explicit-sentinel",
  "family": "activity",
  "description": "Running without the fence takes a deliberate sentinel value that no caller can produce by omission or a dropped key.",
  "invariants": ["inv c-1"],
  "setup": [
    { "op": "company.create", "in": { "template": "northstar" } }
  ],
  "op": "activity.reconcile",
  "in": { "launchIntentPersonIds": "UNFENCED", "requestedPersonIds": ["signal-researcher"] },
  "expect": { "ok": { "...": "..." } },
  "expectState": [
    { "read": "activity.people", "equals": { "...": "..." } }
  ]
}
```

| Field | Meaning |
|---|---|
| `name` | Unique within the family. Invariant numbers belong in the name (`inv14-…`) so a grep for an invariant finds its fixtures. |
| `family` | Store/op family; equals the containing directory. |
| `description` | Prose: what this pins and why it is load-bearing. Required and non-empty. |
| `invariants` | Cross-references into the plan's invariant/flag lists. May be empty. |
| `setup` | Ordered ops applied before the operation under test. **Every setup step must succeed** — a fixture that cannot reach its precondition is broken, not refused, and both runners raise rather than record. Where a *refusal* is the precondition (arming a per-run circuit breaker), the step uses an op that is explicitly defined to succeed only when the call is refused: `tools.call_refused`. |
| `op` | The named operation under test. |
| `caller` | Identity the op is issued under: `personId`, optional `role` (documentation only, never used for authorization). An op that names a caller reads it from here rather than from `in`, so a fixture cannot accidentally present an identity the caller does not hold. |
| `in` | The request payload. |
| `expect` | Exactly one of `{"ok": <response>}` or `{"error": {...}}`. |
| `expectState` | Named durable reads and the exact values they must produce **after** the op. Evaluated in order; at least one is required. |

### `expect.ok`

The operation's bounded response projection — never a whole ledger. Two
reasons: fixtures are read and diffed by humans in review, and plan invariant 27
says responses are bounded anyway. An op that returns nothing records `null`.

### `expect.error`

```json
{ "error": { "type": "Conflict", "code": "fence-mismatch", "message": "Assignment 'validate-signal' is not owned by …" } }
```

- `type` is one of the closed taxonomy (plan §1): `Refused`, `Conflict`, `Busy`,
  `StoreFailure`, `Corrupt`, `Unavailable`. `StoreFailure` is any store error;
  `Corrupt` is the narrow one, reserved for a stored body that did not decode.
- Both store kinds carry a `cause`: the rendered underlying failure (a
  `rusqlite::Error` with its result codes, or the refusal naming the invariant
  that broke). It is **not** corpus-matched — it is a diagnostic sentence, not a
  contract, and pinning it would break on a SQLite or `serde` message change.
  `{type, code}` remains the contract; `cause` is recorded so a fixture shows
  what an operator would actually have been told.
- `code` is the language-neutral refusal code. **This is the field the Rust
  implementation must reproduce.**
- `message` is the sentence the caller sees, recorded so drift is visible.
  `{type, code}` is the contract; a runner asserts `message` only where the
  sentence is a function of what chiefd answered — `conformance_reminders.rs`
  REBUILDS the refusal prose from the route's own `code` and `detail` and
  compares, which is what catches a fixture quoting a refusal chiefd does not
  produce. Card copy is not asserted here at all (see "What a fixture may
  assert").

The refusal taxonomy is a closed five-member set, and mapping a product refusal
onto it is a design decision. It lived in `conformance/lib/taxonomy.ts`, which
is deleted with the harness; the mapping the corpus was recorded under is
preserved in `conformance_session_maintenance.rs`'s own ordered table, and
`code: "unclassified"` is refused by `conformance_tools.rs` — an unclassified
refusal cannot guide anything, so it must not enter the corpus silently.

### `expectState`

```json
{ "read": "maint.request", "args": { "requestId": "session-maintenance:1:ceo:fresh_session" }, "equals": { "...": "..." } }
```

`read` names a read in the read registry; `args` are its parameters; `equals` is
the exact value. State expectations are how a fixture proves a refusal changed
**nothing** — the half of a refusal test that is usually skipped and is exactly
where the historical bugs lived.

## The `tools` family — the agent-facing surface

`tools` is not a store. It is the ~30 `org_*` tools
`packages/piing/extensions/organization-intercom.ts` registers: the entire model-facing API of
the company, and the one part of the system the plan admits was surveyed rather
than mined (DESIGN-NOTES scope note, D12). Its fixtures come in three shapes:

| Op | What it pins |
|---|---|
| `tools.surface` | Which tools exist for a given caller. Today registration *is* the authorization surface (manager-only install; runtime-fenced install); plan §3 item 2 reverses that, so these fixtures are the deliberate before-picture. |
| `tools.describe` | One tool's provider-visible schema, label, description, and execution mode. One fixture per tool — this is the per-field D12 gap, closed field by field, and the input to the M2 wire-type freeze. |
| `tools.call` | Behaviour: the tool's bounded response, or its refusal. |

Two harness facts make these fixtures mean something:

1. **The boundary a tool crosses is HTTP, and `tools.chiefd_calls` records it.**
   Each entry is the `{path, body}` the tool posted. **That request is the
   contract**: on a success fixture it says what the tool asked chiefd to do,
   and on a refusal fixture the empty list proves nothing was attempted.

   **`tools.launcher_calls` is deleted (#1044).** It recorded the argv a tool
   spawned `apps/cli` with. #751/G9 deleted the launcher-subprocess transport
   from `organization-intercom.ts` outright — no `runner:` option, no
   `LauncherRunner`, no spawn — and the same change deleted the scripted host in
   `lib/tool-host.ts` that answered it, leaving `ToolSurface.calls` as an array
   that was declared and never pushed to. The read could only ever answer `[]`,
   so all 74 fixtures that carried it were asserting a constant: 38 pinned a
   non-empty argv and were false, and 36 asserted the `[]` and could not fail.

   The fixtures without a Rust subject kept nothing invented in the field's
   place, and every fixture that survives in this family now has one.
   Re-recording anything else needs a Rust subject per fixture the way the
   reminder three have one; a bulk rewrite would only produce fresh unverified
   assertions, and with `lib/durable.ts` unable to parse there is no recorder to
   produce a real one either.

   A `tools.chiefd_calls` fixture whose route exists in Rust can be **replayed**
   rather than merely recorded — see `conformance_reminders.rs`, which drives
   the recorded bodies through the real router and compares the answer against
   `expect.ok.details`. That is the only thing in this family that can currently
   fail on a product change, and it is the shape the rest should grow into. It
   is also the only recording mechanism the corpus has left: the reminder
   fixtures' `expect` payloads were taken off the live route by
   `conformance_reminders.rs`, not composed by hand.
2. **The process clock is frozen during a tool call**, because the tool layer
   reads `new Date()` directly rather than the injected clock in roughly a dozen
   places. Redacting those stamps would also destroy the deliberate ones (a
   fixture's own future deadline is a timestamp, and it is load-bearing), so the
   harness pins `Date` instead. That the override is *needed* is a finding, not
   a convenience: chiefd must take `now` from the caller.

The one thing still redacted is `randomUUID()` in generated spec-file paths
(`<UUID>`), for the same reason: an id the caller did not choose cannot appear
in a fixture.

## Determinism rules

A fixture must mean the same thing on every machine, in every order, forever.

1. **Frozen clock.** Every fixture starts at `2026-07-15T12:00:00.000Z`. Time
   moves only through the explicit `clock.advance` op. No op may read wall time.
2. **Fresh, empty world.** Each fixture runs in its own temp data root, created
   and destroyed by the harness. State comes only from `setup`.
3. **Named company templates.** A fixture never carries a company spec; it names
   a template (`company.create` with `{"template": "northstar"}`) and the harness
   owns it, so person and department ids are derived, not invented.
4. **No randomness, no environment.** No uuids, no PATH, no env vars in inputs or
   outputs.
5. **Path redaction.** Any occurrence of the temp root in a recorded string is
   replaced by `<ROOT>`.
6. **Sorted object keys** in every recorded value, so a fixture diff shows
   behaviour changes rather than iteration-order changes.

## Ops and reads

Ops are named `<family>.<verb>`; reads are named `<family>.<projection>`. The
registries lived in `conformance/lib/ops.ts` and are deleted with it; the closed
vocabulary each family actually uses is now stated by its runner —
`conformance_tools.rs`'s `BLOCKED_OPS`/`BLOCKED_READS` is the exhaustive list
for `tools`, and an op or read name it does not account for fails by name rather
than being skipped. A fixture the Rust runner cannot execute is a missing chiefd
verb, and it is loud.

## What records a fixture, and what a fixture may assert

`expect` and `expectState.equals` are still never somebody's belief about the
system — but the thing that writes them down has changed, and so has the set of
questions a fixture is allowed to ask.

**The recorder is the replay runner.** `bun conformance/record-ts.ts` is gone
(see README). A fixture's chiefd half is (re-)recorded by running its Rust
runner with `cargo test -- --nocapture`: each runner posts the recorded request
at the REAL router and prints `SERVED <fixture> <answer>`. The value comes off
the live route, never out of anyone's head, and unlike the old recorder it
cannot agree with itself — it asserts against the fixture in the same pass.

**The corpus machine-checks the CHIEFD half only.** A tool call crosses one
boundary chiefd owns — an HTTP request and its answer — and that is what a
fixture may assert:

* `tools.chiefd_calls`: the ordered `{path, body}` a tool posted. This is the
  fixture's transport claim.
* `expect.ok.details` where the tool passes chiefd's answer through, and
  `expect.error` where the refusal is chiefd's.

**The TypeScript half is out of scope, deliberately.** Argument
canonicalization, card rendering and the exact `message` sentence belong to
`packages/piing/extensions/organization-intercom.ts`, and
`packages/piing/test/toolcontract/OrganizationToolContract.test.ts` already
drives all 47 registered tools against a real daemon and asserts them. A fixture
restating those strings would be a second answer to a question that already has
an answer-holder, and the two would drift — which is exactly how this corpus
came to pin a transport the product had deleted. A runner therefore checks
`expect.ok.message` only for the facts it must carry FROM the served answer (the
id it quotes, the count it announces), never for its prose.

**A transport claim must have a subject.** A fixture that records
`tools.chiefd_calls` must be named by a Rust runner that replays it.
`scripts/test/conformance-fixture-subject.test.mjs` enforces this and fails, by
name, on the commit that adds an orphan. The rule exists because the corpus has
twice filled up with assertions nothing executed: 74 `tools.launcher_calls` argv
lists with no producer (#1044), and three reminder fixtures pinning a deleted
transport (#1043). Neither was a wrong assertion — both were assertions with no
subject, and a claim nothing runs cannot be wrong. If what a fixture wants to
claim has no possible Rust subject — "the tool refused before making any call"
is one, since that decision never leaves TypeScript — the read is dropped and
the tool-contract suite owns it.

## Deliberate divergences

Where the plan intends chiefd to behave differently from today's TypeScript, the
fixture still records **today's** behaviour and says so, prefixed `PLAN-DELTA-`
with the plan section in `invariants`. The corpus is the floor; a divergence that
is a decision must be visible as one rather than discovered mid-port. Today there
is one: `session-maintenance/PLAN-DELTA-start-replay-of-the-same-claim-returns-null-today`
(plan §2.5 [Δ]).
