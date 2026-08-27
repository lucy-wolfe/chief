# `conformance/` — the TS↔Rust golden corpus

The corpus records what the **existing TypeScript implementation** does, as
language-neutral JSON, so the `chiefd` Rust port can be proven equal to it
instead of merely looking right. Fixture format: [`FORMAT.md`](./FORMAT.md).

Milestone M0 covers the first two stores: `activity` and
`session-maintenance` (`assignment` went with the goal feature). Milestone M3 adds `tools` — the ~30 `org_*`
tools `packages/piing/extensions/organization-intercom.ts` registers, which are the whole
model-facing API and the plan's own confessed inventory gap (D12). See
[`FORMAT.md`](./FORMAT.md) for how that family works.

## Rust is the runner (#751/G14, 2026-08-08)

**`fixtures/` is the asset; Rust replays it.** Every family runs on
every `cargo test --workspace`, against the JSON in this directory, comparing the
bounded response projection and every durable read byte for byte:

| Family | Fixtures | Runner |
|---|---|---|
| `activity` | 17 | `apps/chiefd/crates/chiefd-core/tests/conformance_activity.rs` |
| `session-maintenance` | 43 | `apps/chiefd/crates/chiefd-core/tests/conformance_session_maintenance.rs` |
| `tools` | 3 | `apps/chiefd/crates/chiefd-core/tests/conformance_tools.rs`; the 3 reminder fixtures are replayed by `apps/chiefd/crates/chiefd-api/tests/conformance_reminders.rs` |

```
cargo test --manifest-path apps/chiefd/Cargo.toml -p chiefd-core --test conformance_activity
cargo test --manifest-path apps/chiefd/Cargo.toml -p chiefd-core --test conformance_session_maintenance
cargo test --manifest-path apps/chiefd/Cargo.toml -p chiefd-core --test conformance_tools
```

They are ordinary `cargo test` targets rather than a standalone `run-rs`
binary, deliberately: a corpus that only runs when somebody remembers to run it
is not a regression floor. No extra CI wiring is needed — the workspace test job
already runs them.

Each runner asserts the number of fixtures it replayed against the number on
disk, so a fixture added later cannot be silently ignored.

### The `tools` family was 137; the 121 with no Rust subject were deleted

`tools` is not a store, it is the `org_*` tools. For 121 of those fixtures there
was no tool registry and no tool schema in `apps/chiefd` to replay against —
`tools.describe` is registration, schemas and cards, none of which exists here.
They were also unrecordable: **#1047 deleted the TypeScript harness
(`record-ts.ts`, `run-ts.ts`, `lib/`, `scenarios/`) and nothing replaced it**, so
a stale fixture could only ever be repaired by hand.

They were deleted on the operator's ruling. The evidence was measured, not
assumed: a guard written against them found **23 frozen strings across 13 files
that contradicted the tool the product registers** — `schema-org-start-person`
froze "Bring up EXACTLY ONE person" for a tool that takes a list, and
`schema-org-remove-contract` froze a confirmation strictly MORE destructive than
the one the product asks for. **A blocked fixture that contradicts the product is
worse than no fixture, because the diff claims it is covered.**

The 16 that remain all have a live Rust subject: their tools cross an HTTP
boundary a `/v1/*` route owns, so each records something Rust can be held to.

## The session-maintenance family was amended by hand, 2026-08-24

**41 fixtures → 25, and 17 of the survivors were mechanically edited. They are
no longer pristine captures, and this note exists so nobody has to discover that
from a diff.**

The operator ruled `org_maintain_session` out whole — the tool and all three of
its actions. What that did to this family:

- **16 DELETED.** Nine whose OP is gone (`maint.queue_company_action`,
  `maint.complete_native`). Seven more whose op survives but which exercise it
  ON a deleted action — three `maint.interrupt`, three `maint.recover`, one
  `maint.start`, each queueing a `fresh_session` in its own setup and expected
  state.
- **17 AMENDED, by one mechanical rule**: `companyActionId` and
  `companyActionOrder` removed from expected state. Nothing else was touched —
  every one of them was `null` or `[]`, because the field existed when the
  fixture was recorded and the scenario never used it.
- **1 PROSE FIX.** `queue-with-an-invalid-action-is-refused` said "Only compact
  and fresh_session exist"; the refusal it records is unchanged.

### Why they were not re-recorded

**The TypeScript recorder is gone and cannot be pointed at the surviving path**
— #1047 deleted `record-ts.ts`, `run-ts.ts`, `lib/` and `scenarios/`, and the
section below records why recreating it is not the answer: *"a harness that
cannot reach the product is not a recorder that broke; it is a recorder that
never was one."*

### Why the amendment is nonetheless CHECKED rather than asserted

The Rust replay is the recorder for the half chiefd owns, and it **asserts
against the fixture in the same pass that reads it** — so it cannot agree with
itself. If the rule above stripped a field the store still emits, the replay
goes red. The amended state is therefore a claim confirmed against observed
behaviour, not an assertion about what the recording would have said.

**The limit of that confirmation, stated so it is not read as more:** it proves
the amended fixture matches what the Rust store does TODAY. It cannot prove the
TypeScript would have emitted the same in June. For these two fields that gap is
immaterial — both are deleted from the product entirely, so no divergence could
manifest as behaviour. **If an amendment ever touches a field that SURVIVES,
that gap is the whole question and this precedent does not cover it.**

### And why the surviving 7 were deleted rather than rewritten

Rewriting them to use `compact` would have preserved the count. It would also
have been **inventing a capture of a scenario that was never run** — and the
replay would have confirmed it, because the replay confirms whatever the store
does today. A rewritten fixture would look exactly as verified as a
field-stripped one while being a completely different kind of claim.

That is the precise reason it is worse than deletion: it would **manufacture
provenance**. A deleted fixture is visibly absent and its absence is recorded
here; a rewritten one presents as a capture of something that never happened,
and nothing downstream can tell the difference.

**A verifier that cannot agree with itself still cannot tell you whether the
question was worth asking.**

## The TypeScript harness is deleted (#1047)

`run-ts.ts`, `record-ts.ts`, `lib/` and `scenarios/` are gone. `fixtures/`,
this file and `FORMAT.md` are what remain, which is what `#751/G14` ruled in the
first place: *"the fixtures are the asset, the runner was only ever the thing
that read them."*

**Why the reversal is reasoned rather than forgotten.** `#1035` overturned that
deletion on one ground — "it cannot run" is not sufficient cause to delete an
artifact that explains a corpus which IS replayed. `#1046` retired that ground
in two moves. It ruled that the corpus machine-checks the **chiefd half only**,
so how the TypeScript half was produced stopped being a fact the corpus depends
on; and it named the recorder that already existed, so the explanation is live
code instead of dead code. With both of those true the harness explained
nothing the corpus asserts, and #751/G14's original instruction stands.

**What was actually wrong with it, recorded because the shape recurs.** Three
independent faults, and the order matters. `lib/durable.ts` **did not parse** —
merge `b887b9a9c` kept BOTH branches' `healthy()`, so the `async` one was never
closed and `conformanceChiefdUrl` and `ensureDurableStore` were `export`
declarations inside a function body. It ALSO imported four modules the Rust port
deleted, which is the fault everyone named and the one that would not have fixed
it. And `lib/tool-host.ts` installed `installOrganizationIntercom` with **no
beacond wiring at all**, so it could not resolve a company and could not make a
real route call: `ToolSurface.chiefdCalls` had exactly one producer, an injected
`goalClear` stub answering with a canned result. A harness that cannot reach the
product is not a recorder that broke; it is a recorder that never was one.

## What records a fixture

**The replay runner.** For the half chiefd owns, the Rust replay IS the
recorder: it posts the recorded calls at the real router and prints the served
record, so an `expect.ok.details` is observed rather than composed.

```
cargo test -p chiefd-api --test conformance_reminders -- --nocapture   # SERVED <fixture> <json>
```

Unlike a recorder that writes the file, it cannot agree with itself — it asserts
against the fixture in the same pass. The frozen instant every fixture is
recorded against is `chiefd_core::test_support::CONFORMANCE_EPOCH`, which is now
its one definition; it used to be five copies across two languages with nothing
comparing them.

The half TypeScript owns — argument canonicalization, the card, the exact
`message` string — is **out of the corpus** by ruling, and belongs to
`packages/piing/test/toolcontract/OrganizationToolContract.test.ts`, which drives
all 47 registered tools against a real daemon. See `FORMAT.md`.

## Adding a fixture

1. Write the JSON. `FORMAT.md` is the contract; assert only what chiefd owns.
2. Add the Rust side in the family's runner, and run it with `--nocapture` — the
   served record is what the fixture must say.
3. If it records `tools.chiefd_calls`, it MUST be named by a runner:
   `scripts/test/conformance-fixture-subject.test.mjs` refuses a transport claim
   nothing replays, which is what 74 `launcher_calls` assertions taught.
4. A fixture that replays nowhere is a fixture nothing tests.

Per TESTING.md §8 the corpus is append-only: any invariant discovered while
implementing gets a numbered fixture the same day it is coded.

## Not yet here

- **A live-traffic tap.** M3 as written in the plan asks for "one week of real
  Cobalt intercom request/response pairs, redacted via `redact()`, frozen as
  fixtures". What is here instead is the code pass plus recordings driven
  through the real registered `execute` functions. That is the stronger artifact
  for a port — live traffic samples only the paths agents happened to take that
  week, and by construction contains almost no refusals, which is where every
  historical bug lived — but it is *not* a substitute for one thing live traffic
  would give: evidence of which fields real providers actually send, including
  the historical aliases the §3.5 shim must keep accepting. If a tap is added
  later, its value is alias/field-frequency data, not new behaviour.
- `CRITIQUE-MAP.md` — the adversarial-review-finding → fixture mapping table.
  Deliberately not stubbed: it should be written from the review documents, not
  guessed at.
