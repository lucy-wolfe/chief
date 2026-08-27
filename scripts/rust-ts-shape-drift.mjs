#!/usr/bin/env node
// #875: the bounded enumeration of every Rust store struct <-> chiefing
// TS-type "join" pair, plus the CLI entrypoint that reads BOTH real sources
// and fails loudly on drift. `bun run check:rust-ts-shape-drift` (wired in
// root package.json) runs this against the live tree.
//
// Scope (stated explicitly, per #875's "enumerate every struct/type pair in
// the join, including the ones that agree" — a bounded set is the
// deliverable, negatives included):
//
//  - IN SCOPE: every TOP-LEVEL document/row struct under
//    `chiefd-core/src/store/**` that #844's audit already enumerated (the
//    26 `#[serde(flatten)] extra`-bearing structs) PLUS every named
//    sub-struct that has its own matching NAMED chiefing TS interface
//    (e.g. `AckReceipt` <-> `AckReceiptDoc`) — 20 pairs below.
//  - OUT OF SCOPE, documented (not silently dropped):
//    - `HealthMonitorIncident`/`HealthMonitorObservation`/`HealthLogCursor`/
//      `TerminalHealthIncidentResolution` — nested inside `HealthMonitorState`
//      /`HealthMonitorDoc` as Rust named structs but TS ANONYMOUS inline
//      object-literal types (no named interface to point a pair at). A
//      recursive Rust<->TS structural-type comparison would be needed to
//      cover these; the top-level `HealthMonitorState`/`HealthMonitorDoc`
//      pair below still catches a field added/removed at the DOCUMENT level
//      (the #875 example: "a new field on the struct"), which is the
//      concrete risk the issue names. Filed as a known gap, not silently
//      skipped — see the DECISIONS.md entry.
//    - `GoalIntent` (inside `GoalIntents`/`GoalIntentsDoc`) — TS types the
//      map value as `Record<string, unknown>`, same untyped-by-design shape.
//    - Router-level ad-hoc result envelopes with no backing named Rust
//      struct (`StartPersonResult`,
//      `SemanticQueueInsertResult`, `ClearedResult`,
//      `InsertEventOnceMarkerResult`, `PruneEventOnceMarkersResult`) — these
//      are constructed as `serde_json::json!({...})` literals in
//      `chiefd-api/src/docstore/router.rs` handlers, not derived from a
//      `#[derive(Serialize)]` struct this script's Rust-struct extractor can
//      read. Small, stable, low field-count; out of scope for the same
//      reason a router-JSON-literal parser was judged not worth building
//      for #816's set-actuation-config verb.
//      `PrepareCeoOnlyResult` once LEFT this exclusion, when its route's body
//      became a real `#[derive(Serialize)] pub struct
//      OrgPrepareCeoOnlyResponse` rather than a `json!` literal, and it was a
//      PAIR below. Both sides are deleted with the daemon-side CEO boot
//      (chief-home-is-cwd §4c), so the pair is gone rather than back here.
//    - `EventOnceMarker`'s `event: Map<String, Value>` and
//      `EventOnceMarkerDoc.event: Record<string, unknown>` — deliberately
//      untyped passthrough, both sides agree it is opaque.
//
// "Which direction does drift fail?" (per #875's own question): the two
// halves of `diffShapes`'s result answer this directly — `rustOnly` is the
// DATA-LOSS direction (chiefd sends a field, chiefing's type never declares
// it, so a caller reading through the type silently drops the value on any
// round-trip); `tsOnly` is the THROWS-AT-BOUNDARY direction (the TS type
// promises a field chiefd's struct never actually sends, so strict code
// assuming its presence meets `undefined` at runtime instead of a value).
//
// KNOWN LIMIT OF THE RUST-SIDE EXTRACTOR (say what it can't parse, so the
// next person doesn't discover the boundary by being wrong): field/attribute
// extraction is REGEX-based, not a real Rust parser (no `syn`). It already
// broke once while this file was being written — a doc comment's own prose
// (a bracketed range in prose) corrupted the shared bracket-depth counter
// used to split fields on top-level commas, misreading a field's own generic
// type as a phantom second field (fixed: comment lines are now stripped
// before depth-counting; regression-tested in
// scripts/test/rust-ts-shape-drift.test.mjs). It clears #873's bar today —
// zero false positives across all 31 real pairs, tamper-proven independently
// by the merger against the live tree. But it has NOT been tried against
// every Rust syntax shape this codebase could grow (multi-line generic
// bounds, macro-generated fields, cfg-gated fields, a field whose own type
// spans a doc-comment-interrupted multi-line declaration). If the regex
// accumulates special cases as new struct shapes are added to `PAIRS`, the
// durable fix is a real `syn`-based extractor (parse the file as a token
// stream, not text) rather than one more pattern bolted onto this one.

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { diffShapes, rustWireFields, tsInterfaceFields, tsNestedInterfaceFields } from './rust-ts-shape-drift-lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..')

const STORE = 'apps/chiefd/crates/chiefd-core/src/store'
const API_DOCSTORE = 'apps/chiefd/crates/chiefd-api/src/docstore'
const ROW_DOCS_TS = 'packages/chiefing/src/types/RowDocs.ts'
const ORG_SLICE_TS = 'packages/chiefing/src/types/OrgSlice.ts'
const PERSON_CONTRACTS_TS = 'packages/chiefing/src/types/PersonContracts.ts'

/** Every checked join pair: `{ label, rustFile, rustStruct, tsFile, tsInterface }`. */
export const PAIRS = [
  { label: 'session-epoch', rustFile: `${STORE}/session_epoch_rows.rs`, rustStruct: 'SessionEpoch', tsFile: ROW_DOCS_TS, tsInterface: 'SessionEpochDoc' },
  { label: 'goal-delivery-quiesce', rustFile: `${STORE}/goal_delivery_quiesce_rows.rs`, rustStruct: 'GoalDeliveryQuiesce', tsFile: ROW_DOCS_TS, tsInterface: 'GoalDeliveryQuiesceDoc' },
  { label: 'operator-escalation-push', rustFile: `${STORE}/operator_escalation_push_rows.rs`, rustStruct: 'OperatorEscalationPush', tsFile: ROW_DOCS_TS, tsInterface: 'OperatorEscalationPushDoc' },
  { label: 'operator-escalation-push:pending-doorbell', rustFile: `${STORE}/operator_escalation_push_rows.rs`, rustStruct: 'PendingDoorbell', tsFile: ROW_DOCS_TS, tsInterface: 'OperatorEscalationPushDoc', tsNestedType: 'pending' },
  { label: 'runtime-owner', rustFile: `${STORE}/runtime_owner_rows.rs`, rustStruct: 'RuntimeOwner', tsFile: ROW_DOCS_TS, tsInterface: 'RuntimeOwnerDoc' },
  { label: 'launch-intent', rustFile: `${STORE}/launch_intent_rows.rs`, rustStruct: 'LaunchIntent', tsFile: ROW_DOCS_TS, tsInterface: 'LaunchIntentDoc' },
  { label: 'launch-intent:start-attribution', rustFile: `${STORE}/launch_intent_rows.rs`, rustStruct: 'StartAttribution', tsFile: ROW_DOCS_TS, tsInterface: 'LaunchIntentAttribution' },
  { label: 'mutation-journal-record', rustFile: `${STORE}/mutation_journal_rows.rs`, rustStruct: 'MutationRecord', tsFile: ROW_DOCS_TS, tsInterface: 'MutationJournalRecordDoc' },
  { label: 'mutation-journal', rustFile: `${STORE}/mutation_journal_rows.rs`, rustStruct: 'MutationJournal', tsFile: ROW_DOCS_TS, tsInterface: 'MutationJournalDoc' },
  { label: 'event-once-marker', rustFile: `${STORE}/event_journal_rows.rs`, rustStruct: 'EventOnceMarker', tsFile: ROW_DOCS_TS, tsInterface: 'EventOnceMarkerDoc' },
  { label: 'converge-safety-state', rustFile: `${STORE}/converge_safety.rs`, rustStruct: 'ConvergeSafetyState', tsFile: ROW_DOCS_TS, tsInterface: 'ConvergeSafetyDoc' },
  { label: 'converge-safety:refusal-record', rustFile: `${STORE}/converge_safety.rs`, rustStruct: 'RefusalRecord', tsFile: ROW_DOCS_TS, tsInterface: 'ConvergeSafetyDoc', tsNestedType: 'lastRefusal' },
  { label: 'health-monitor-state', rustFile: `${STORE}/health_monitor_rows.rs`, rustStruct: 'HealthMonitorState', tsFile: ROW_DOCS_TS, tsInterface: 'HealthMonitorDoc' },
  { label: 'operator-escalation-intent', rustFile: `${STORE}/operator_escalation_intents_rows.rs`, rustStruct: 'OperatorEscalationIntent', tsFile: ROW_DOCS_TS, tsInterface: 'OperatorEscalationIntentDoc' },
  { label: 'operator-escalation-intents', rustFile: `${STORE}/operator_escalation_intents_rows.rs`, rustStruct: 'OperatorEscalationIntents', tsFile: ROW_DOCS_TS, tsInterface: 'OperatorEscalationIntentsDoc' },
  { label: 'person-contract-entry', rustFile: `${STORE}/person_contracts/rows.rs`, rustStruct: 'PersonContractEntry', tsFile: PERSON_CONTRACTS_TS, tsInterface: 'PersonContractEntry' },
  { label: 'organization-person-contracts', rustFile: `${STORE}/person_contracts/rows.rs`, rustStruct: 'OrganizationPersonContracts', tsFile: PERSON_CONTRACTS_TS, tsInterface: 'OrganizationPersonContractsDocument' },
  // `/v1/org/tree/structured` — all three structs of one projection, because a
  // response shape is only as checked as its least-checked level. This is the
  // only roster the browser is given, and it drifted in exactly the direction
  // this guard names DATA-LOSS: chiefd's `PersonRecord` carries
  // `employment_state`, the projection dropped it, and a departed person
  // rendered identically to an active one for as long as the route existed.
  // Nothing here was declared, so nothing compared it.
  { label: 'company-tree-person', rustFile: `${API_DOCSTORE}/company_tree.rs`, rustStruct: 'TreePerson', tsFile: ORG_SLICE_TS, tsInterface: 'CompanyTreePerson' },
  { label: 'company-tree-department', rustFile: `${API_DOCSTORE}/company_tree.rs`, rustStruct: 'DepartmentNode', tsFile: ORG_SLICE_TS, tsInterface: 'CompanyTreeDepartment' },
  { label: 'company-tree', rustFile: `${API_DOCSTORE}/company_tree.rs`, rustStruct: 'CompanyTree', tsFile: ORG_SLICE_TS, tsInterface: 'CompanyTreeResult' },
]

/** Run one pair's check against real file content. Returns `{ pair, error }`
 * on a hard failure (struct/interface/property not found — a stale pair
 * entry), or `{ pair, result }` with the diff otherwise. `tsNestedType`
 * pairs (a Rust sub-struct checked against a TS interface's OWN property's
 * inline object-literal type, not the interface itself — `PendingDoorbell`
 * against `OperatorEscalationPushDoc.pending`) resolve via
 * `tsNestedInterfaceFields`, which throws rather than silently falling back
 * to the outer interface's unrelated key set if the property or its
 * type-literal shape cannot be found. */
export function checkPair(pair) {
  const rustSource = readFileSync(join(repoRoot, pair.rustFile), 'utf8')
  const tsSource = readFileSync(join(repoRoot, pair.tsFile), 'utf8')
  const rustFields = rustWireFields(rustSource, pair.rustStruct)
  const tsFields = pair.tsNestedType
    ? tsNestedInterfaceFields(tsSource, pair.tsInterface, pair.tsNestedType)
    : tsInterfaceFields(tsSource, pair.tsInterface)
  assertNonVacuous(pair, rustFields, tsFields)
  const result = diffShapes(rustFields, tsFields)
  return { pair, result }
}

/**
 * Refuse a pair that compares nothing.
 *
 * A side that parses to NOTHING diffs clean against anything, so a vacuous pair
 * reports the same "no drift" as a pair that compares correctly — and reads, in
 * the summary line, as one more checked pair. That is strictly worse than not
 * declaring the pair at all: an undeclared gap at least looks like a gap.
 *
 * Not hypothetical. `parseRustFields` accepted only bare `pub`, so any struct
 * with `pub(crate)` fields parsed empty, and the first such pair added would
 * have passed vacuously forever. The parser reads those now; this floor is what
 * makes the NEXT shape it cannot read a red instead of a lie.
 *
 * Separate and exported so it can be tested directly: proving a floor works by
 * finding real source the parser fails on is a test that deletes itself the
 * moment the parser improves.
 */
export function assertNonVacuous(pair, rustFields, tsFields) {
  if (rustFields.length > 0 && tsFields.length > 0) return
  const tsName = `${pair.tsInterface}${pair.tsNestedType ? `.${pair.tsNestedType}` : ''}`
  throw new Error(
    `vacuous pair '${pair.label}' — ${pair.rustStruct} parsed ${rustFields.length} field(s) ` +
      `and ${tsName} parsed ${tsFields.length}. A side with no fields matches everything, ` +
      `so this pair proves nothing. Either the struct/interface moved, or the source uses ` +
      `a shape the parser does not read yet.`,
  )
}

function main() {
  const failures = []
  for (const pair of PAIRS) {
    let outcome
    try {
      outcome = checkPair(pair)
    } catch (error) {
      failures.push({ pair, error: error instanceof Error ? error.message : String(error) })
      continue
    }
    if (outcome.result.drifted) failures.push(outcome)
  }

  if (failures.length === 0) {
    console.log(`[rust-ts-shape-drift] ${PAIRS.length} pair(s) checked, no drift.`)
    process.exit(0)
  }

  console.error(`[rust-ts-shape-drift] DRIFT FOUND in ${failures.length}/${PAIRS.length} pair(s):\n`)
  for (const failure of failures) {
    if ('error' in failure) {
      console.error(`  ${failure.pair.label}: ERROR — ${failure.error}`)
      continue
    }
    const { pair, result } = failure
    console.error(`  ${pair.label} (${pair.rustFile}::${pair.rustStruct} <-> ${pair.tsFile}::${pair.tsInterface})`)
    if (result.rustOnly.length) {
      console.error(
        `    DATA-LOSS direction — chiefd sends these, chiefing never declares them: ${result.rustOnly.map((f) => f.name).join(', ')}`,
      )
    }
    if (result.tsOnly.length) {
      console.error(
        `    THROWS-AT-BOUNDARY direction — chiefing declares these, chiefd's struct never sends them: ${result.tsOnly.map((f) => f.name).join(', ')}`,
      )
    }
  }
  process.exit(1)
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main()
