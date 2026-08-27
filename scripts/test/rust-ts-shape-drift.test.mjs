// #875: unit tests for the Rust-struct <-> chiefing-TS-type shape-drift
// check's pure parsing/diffing functions, PLUS the end-to-end proof this
// story's acceptance criteria demand: the real, live pair table (`PAIRS`),
// run against the real, live source tree, must be clean today; and a
// deliberately-broken copy of a real struct/type pair must be caught, in
// BOTH directions, by the same functions the CLI uses. "A check never seen
// to fail is indistinguishable from one that cannot fail."
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

import {
  diffShapes,
  extractRustStructBody,
  parseRustFields,
  rustWireFields,
  snakeToCamel,
  structHasRenameAll,
  tsInterfaceFields,
  tsNestedInterfaceFields,
} from '../rust-ts-shape-drift-lib.mjs'
import { assertNonVacuous, PAIRS, checkPair } from '../rust-ts-shape-drift.mjs'

// ---- snakeToCamel ----------------------------------------------------------

test('snakeToCamel: multi-word field names', () => {
  assert.equal(snakeToCamel('socket_name'), 'socketName')
  assert.equal(snakeToCamel('cycle_started_at_ms'), 'cycleStartedAtMs')
})

test('snakeToCamel: a single-word field is unchanged (already correct either way)', () => {
  assert.equal(snakeToCamel('token'), 'token')
  assert.equal(snakeToCamel('version'), 'version')
})

// ---- extractRustStructBody / structHasRenameAll ----------------------------

// Verbatim in shape from `store/goal_delivery_quiesce_rows.rs`: NO
// struct-level `rename_all`, one explicit per-field `#[serde(rename)]`, and a
// flatten catch-all — the convention `rust-ts-shape-drift-lib.mjs` documents.
// This was `boot_lease_rows.rs`'s `CeoBootLease` until chief-home-is-cwd §4c
// deleted that module with the daemon-side CEO boot; it moved to a live sibling
// rather than becoming a synthetic struct, because a parser fixture that
// mirrors nothing real stops tracking the convention it exists to pin.
const REAL_RUST_STRUCT = `
/// A goal-delivery-quiesce singleton.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDeliveryQuiesce {
    /// Always \`1\`. Compile-time constant, not stored.
    pub version: u32,
    /// The company slug — DERIVED, not stored.
    pub organization: String,
    /// Every automatic mail-backed grant requires its justifying envelope.
    #[serde(rename = "quiescedAt")]
    pub quiesced_at: String,
    /// Any unmodeled key (item D). Empty on a clean doc.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

pub const SOMETHING_ELSE: &str = "not a struct";
`

test('extractRustStructBody finds a real struct body, brace-depth-aware', () => {
  const body = extractRustStructBody(REAL_RUST_STRUCT, 'GoalDeliveryQuiesce')
  assert.ok(body.includes('pub version: u32'))
  assert.ok(body.includes('pub extra: BTreeMap<String, serde_json::Value>'))
  assert.ok(!body.includes('SOMETHING_ELSE'))
})

test('extractRustStructBody returns null for a struct that is not present', () => {
  assert.equal(extractRustStructBody(REAL_RUST_STRUCT, 'NoSuchStruct'), null)
})

test('structHasRenameAll is false for a per-field-rename struct', () => {
  assert.equal(structHasRenameAll(REAL_RUST_STRUCT, 'GoalDeliveryQuiesce'), false)
})

test('structHasRenameAll is true when the struct carries #[serde(rename_all = "camelCase")]', () => {
  const source = `
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckReceipt {
    pub assignment_id: String,
}
`
  assert.equal(structHasRenameAll(source, 'AckReceipt'), true)
})

// ---- parseRustFields --------------------------------------------------------

test('parseRustFields: explicit #[serde(rename = "...")] wins over the raw field name', () => {
  const body = extractRustStructBody(REAL_RUST_STRUCT, 'GoalDeliveryQuiesce')
  const fields = parseRustFields(body, false)
  const renamedField = fields.find((f) => f.rustName === 'quiesced_at')
  assert.equal(renamedField.wireName, 'quiescedAt')
})

test('parseRustFields: an unrenamed single-word field is unaffected either way', () => {
  const body = extractRustStructBody(REAL_RUST_STRUCT, 'GoalDeliveryQuiesce')
  const fields = parseRustFields(body, false)
  assert.ok(fields.some((f) => f.rustName === 'version' && f.wireName === 'version'))
})

test('parseRustFields: #[serde(flatten)] is flagged, not silently treated as a normal field', () => {
  const body = extractRustStructBody(REAL_RUST_STRUCT, 'GoalDeliveryQuiesce')
  const fields = parseRustFields(body, false)
  const extraField = fields.find((f) => f.rustName === 'extra')
  assert.equal(extraField.flatten, true)
})

test('parseRustFields: regression -- a generic type\'s own comma (BTreeMap<String, serde_json::Value>) does not get misread as a second field, and prose inside a doc comment ("(>= 1)") does not corrupt the running bracket depth for every field after it', () => {
  const source = `
#[derive(Serialize, Deserialize)]
pub struct AckReceipt {
    pub assignment_id: String,
    /// Runtime generation (>= 1).
    pub generation: i64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
`
  const body = extractRustStructBody(source, 'AckReceipt')
  const fields = parseRustFields(body, false)
  assert.deepEqual(
    fields.map((f) => f.rustName),
    ['assignment_id', 'generation', 'extra'],
  )
  // The historical failure mode: this test would otherwise also report a
  // phantom fourth field named `serde_json` (matched out of the `extra`
  // field's own TYPE) once the running depth corrupted past `generation`'s
  // doc comment.
  assert.equal(fields.length, 3)
})

// ---- tsInterfaceFields / tsNestedInterfaceFields ----------------------------

const REAL_TS_SOURCE = `
export interface GoalDeliveryQuiesceDoc {
  version: 1
  organization: string
  quiescedAt: string
}

export interface ConvergeSafetyDoc {
  schemaVersion: 1
  lastRefusal?: {
    kind: string
    detail: string
    at: string
  }
}
`

test('tsInterfaceFields reads real property keys via the TypeScript compiler API', () => {
  const fields = tsInterfaceFields(REAL_TS_SOURCE, 'GoalDeliveryQuiesceDoc')
  assert.deepEqual(
    fields.map((f) => f.name).sort(),
    ['organization', 'quiescedAt', 'version'],
  )
})

test('tsInterfaceFields throws on an interface name that is not present (a stale pair entry, not a silent empty result)', () => {
  assert.throws(() => tsInterfaceFields(REAL_TS_SOURCE, 'NoSuchInterface'), /not found/)
})

test('tsNestedInterfaceFields reads a nested inline object-literal property type', () => {
  const fields = tsNestedInterfaceFields(REAL_TS_SOURCE, 'ConvergeSafetyDoc', 'lastRefusal')
  assert.deepEqual(
    fields.map((f) => f.name).sort(),
    ['at', 'detail', 'kind'],
  )
})

test('tsNestedInterfaceFields throws (not silently empty) when the named property does not exist', () => {
  assert.throws(
    () => tsNestedInterfaceFields(REAL_TS_SOURCE, 'ConvergeSafetyDoc', 'noSuchProperty'),
    /has no property/,
  )
})

// ---- diffShapes --------------------------------------------------------------

test('diffShapes: no drift when the key sets agree exactly', () => {
  const rust = [{ name: 'a' }, { name: 'b' }]
  const ts = [{ name: 'a' }, { name: 'b' }]
  const result = diffShapes(rust, ts)
  assert.equal(result.drifted, false)
  assert.deepEqual(result.rustOnly, [])
  assert.deepEqual(result.tsOnly, [])
})

test('diffShapes: DATA-LOSS direction -- a Rust-only field is reported as rustOnly', () => {
  const rust = [{ name: 'a' }, { name: 'newField' }]
  const ts = [{ name: 'a' }]
  const result = diffShapes(rust, ts)
  assert.equal(result.drifted, true)
  assert.deepEqual(result.rustOnly.map((f) => f.name), ['newField'])
  assert.deepEqual(result.tsOnly, [])
})

test('diffShapes: THROWS-AT-BOUNDARY direction -- a TS-only field is reported as tsOnly, distinctly from rustOnly', () => {
  const rust = [{ name: 'a' }]
  const ts = [{ name: 'a' }, { name: 'phantomField' }]
  const result = diffShapes(rust, ts)
  assert.equal(result.drifted, true)
  assert.deepEqual(result.rustOnly, [])
  assert.deepEqual(result.tsOnly.map((f) => f.name), ['phantomField'])
})

// ---- End-to-end: the real, live pair table against the real, live tree -----

test('every real PAIRS entry is clean against the live tree today', () => {
  const failures = []
  for (const pair of PAIRS) {
    let outcome
    try {
      outcome = checkPair(pair)
    } catch (error) {
      failures.push(`${pair.label}: ERROR — ${error instanceof Error ? error.message : String(error)}`)
      continue
    }
    if (outcome.result.drifted) {
      failures.push(
        `${pair.label}: rustOnly=${JSON.stringify(outcome.result.rustOnly)} tsOnly=${JSON.stringify(outcome.result.tsOnly)}`,
      )
    }
  }
  assert.deepEqual(failures, [], `drift found:\n${failures.join('\n')}`)
})

test('PAIRS is non-empty and covers at least the #844 audit\'s floor (20+ document structs, plus sub-struct pairs)', () => {
  // A floor, not a ceiling -- mirrors cargo-test-floor.mjs's own convention.
  // Must never silently drop to near-zero because a refactor renamed a file
  // and a pair entry stopped resolving without anyone noticing the count.
  //
  // 28 -> 26: three pairs lost their SUBJECT rather than their coverage.
  // `ack-receipt` and `acks-queue` described the acknowledgement-receipt queue
  // and `memory-record` the memory store; both features are deleted, and a
  // pair naming a struct that no longer exists is the stale entry this floor
  // exists to catch, not coverage worth keeping.
  //
  // 26 -> 24 for the same reason again: `founder-model-bootstrap` and
  // `provider-observation` named `chiefd-core`'s `model_catalog.rs`, and
  // provider/model management is deleted whole — the Rust file and both TS
  // interfaces are gone, so the pairs describe nothing.
  //
  // 24 -> 22, once more: `ceo-boot-lease` and `prepare-ceo-only` named
  // `boot_lease_rows.rs` and `OrgPrepareCeoOnlyResponse`, both deleted with the
  // daemon-side CEO boot (chief-home-is-cwd §4c).
  //
  // 22 -> 20: `materialization` and `materialization-checkpoint` named the
  // durable projection state deleted in chief-home-is-cwd §4d. Both Rust
  // structs and both TypeScript interfaces are gone, so these pairs also have
  // no subject to protect.
  assert.ok(PAIRS.length >= 20, `expected at least 20 pairs, found ${PAIRS.length}`)
})

// ---- End-to-end: demonstrated red, against the REAL launch_intent_rows.rs/RowDocs.ts pair, not a synthetic fixture ----
//
// This drove the `ceo-boot-lease` pair until chief-home-is-cwd §4c deleted
// `boot_lease_rows.rs` with the daemon-side CEO boot. It moved to
// `launch-intent` rather than to a synthetic struct: the point of the test is
// that the REAL TS side is read off disk, so a pair whose file no longer exists
// cannot carry it.

test('demonstrated red: a field added to the REAL LaunchIntent Rust struct that RowDocs.ts never picks up is caught, in the correct (data-loss) direction', () => {
  const pair = PAIRS.find((p) => p.label === 'launch-intent')
  assert.ok(pair, 'the launch-intent pair must exist for this test to mean anything')

  const rustFieldsWithBogus = rustWireFields(
    `
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchIntent {
    pub version: u32,
    pub organization: String,
    pub person_ids: Vec<String>,
    pub updated_at: String,
    #[serde(default)]
    pub attributions: BTreeMap<String, StartAttribution>,
    pub bogus_new_field: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
`,
    'LaunchIntent',
  )
  const realTsFields = tsInterfaceFields(
    readFileSync(new URL('../../packages/chiefing/src/types/RowDocs.ts', import.meta.url), 'utf8'),
    'LaunchIntentDoc',
  )
  const result = diffShapes(rustFieldsWithBogus, realTsFields)
  assert.equal(result.drifted, true)
  assert.deepEqual(result.rustOnly.map((f) => f.name), ['bogusNewField'])
  assert.deepEqual(result.tsOnly, [])
})

// ---- Restricted visibility: the shape that made a pair prove nothing --------

// Verbatim in shape from `chiefd-api/src/docstore/company_tree.rs`: every field
// `pub(crate)`, which is ordinary for a crate-internal projection and which the
// parser could not read at all.
const CRATE_VISIBLE_STRUCT = `
/// One person, as the tree carries them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TreePerson {
    /// Stable person id.
    pub(crate) id: String,
    /// Structural role.
    pub(crate) kind: String,
    /// Whether they still work here.
    pub(crate) employment_state: String,
    /// Identity colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) accent: Option<String>,
}
`

test('extractRustStructBody finds a pub(crate) struct', () => {
  // It matched the literal `pub struct`, so this returned null and the pair
  // failed as "struct not found (stale pair entry?)" — an error that points at
  // the pair table rather than at the parser that cannot read the file.
  const body = extractRustStructBody(CRATE_VISIBLE_STRUCT, 'TreePerson')
  assert.ok(body !== null, 'a pub(crate) struct must be findable')
  assert.ok(body.includes('pub(crate) id: String'))
})

test('structHasRenameAll reads the attributes of a pub(crate) struct', () => {
  // Same literal-`pub` bug, and this one fails QUIETLY: not-found returns
  // false, i.e. "no rename_all", which would report every field's wire name in
  // snake_case and manufacture drift on every field at once.
  assert.equal(structHasRenameAll(CRATE_VISIBLE_STRUCT, 'TreePerson'), true)
})

test('parseRustFields reads every visibility form, and a raw identifier', () => {
  // THE DEFECT. `(?:pub\s+)?` matched `pub name:` and a bare `name:` and
  // nothing else, so a struct of `pub(crate)` fields parsed to the EMPTY list —
  // and an empty field list diffs clean against any interface, so the pair
  // passed while comparing nothing.
  const cases = [
    ['pub(crate) id: String,', 'id'],
    ['pub(super) id: String,', 'id'],
    ['pub(in crate::store) id: String,', 'id'],
    ['pub id: String,', 'id'],
    ['id: String,', 'id'],
    ['pub r#type: String,', 'type'],
    ['#[serde(default)]\n    pub(crate) id: String,', 'id'],
  ]
  for (const [source, expected] of cases) {
    const fields = parseRustFields(source, true)
    assert.deepEqual(
      fields.map((f) => f.wireName),
      [expected],
      `failed to read: ${source}`,
    )
  }
})

test('a pub(crate) struct yields its real field set, not an empty one', () => {
  assert.deepEqual(
    rustWireFields(CRATE_VISIBLE_STRUCT, 'TreePerson').map((f) => f.name),
    ['id', 'kind', 'employmentState', 'accent'],
  )
})

// ---- The non-vacuity floor -------------------------------------------------

test('diffShapes reports NO drift when a side is empty — which is why the floor exists', () => {
  // Not a bug in `diffShapes`: an empty set genuinely differs from nothing.
  // It is the reason a vacuous pair cannot be allowed to reach it, because at
  // this point the result is indistinguishable from a correct pass.
  const clean = diffShapes([], [{ name: 'anything', optional: false }])
  assert.equal(clean.drifted, true, 'ts-only fields still drift')
  const bothWays = diffShapes([], [])
  assert.equal(bothWays.drifted, false, 'two empty sets agree — vacuously')
})

test('assertNonVacuous refuses a pair whose Rust side parsed to nothing', () => {
  // The floor. Without it, the first pair naming a struct the parser cannot
  // read becomes a green check that proves nothing, and reads in the summary
  // as one more pair checked.
  const pair = { label: 'synthetic', rustStruct: 'TreePerson', tsInterface: 'CompanyTreePerson' }
  assert.throws(
    () => assertNonVacuous(pair, [], [{ name: 'id', optional: false }]),
    /vacuous pair 'synthetic'.*TreePerson parsed 0 field/s,
  )
})

test('assertNonVacuous refuses a pair whose TS side parsed to nothing', () => {
  const pair = { label: 'synthetic', rustStruct: 'TreePerson', tsInterface: 'CompanyTreePerson' }
  assert.throws(
    () => assertNonVacuous(pair, [{ name: 'id', optional: false }], []),
    /CompanyTreePerson parsed 0/,
  )
})

test('assertNonVacuous passes a pair with fields on both sides', () => {
  assert.doesNotThrow(() =>
    assertNonVacuous({ label: 'synthetic', rustStruct: 'R', tsInterface: 'T' }, [
      { name: 'id', optional: false },
    ], [{ name: 'id', optional: false }]),
  )
})

test('every declared pair actually compares something', () => {
  // The audit that would have caught this class at any point in the guard's
  // life. Runs over the REAL table against REAL sources, so a pair that stops
  // resolving — a rename, a moved file, a shape the parser cannot read — is a
  // red here rather than a silent green in the drift check itself.
  for (const pair of PAIRS) {
    assert.doesNotThrow(() => checkPair(pair), `pair '${pair.label}' proves nothing`)
  }
})

// ---- The company-tree pairs ------------------------------------------------

test('the /v1/org/tree/structured projection is covered at every level', () => {
  // It was covered at NO level, which is how it drifted: `PersonRecord` carries
  // `employment_state`, the projection dropped it, and a departed person
  // rendered identically to an active one. A response shape is only as checked
  // as its least-checked level, so all three structs are pairs.
  for (const label of ['company-tree-person', 'company-tree-department', 'company-tree']) {
    assert.ok(
      PAIRS.some((p) => p.label === label),
      `missing pair: ${label}`,
    )
  }
})

test('demonstrated red: a field added to the REAL TreePerson that OrgSlice.ts never picks up is caught in the data-loss direction', () => {
  const pair = PAIRS.find((p) => p.label === 'company-tree-person')
  assert.ok(pair, 'the company-tree-person pair must exist for this test to mean anything')

  const rustFieldsWithBogus = rustWireFields(
    `
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TreePerson {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) kind: String,
    // Mirrors the REAL TreePerson's field set. employment_state arrived with
    // the departed-person projection and must be here, or this fixture drifts
    // from the struct it stands in for and the assertion below reports the
    // fixture's own gap as tsOnly instead of the one field it means to inject.
    // (No backticks in this comment: it lives inside a JS template literal.)
    pub(crate) employment_state: String,
    pub(crate) newly_added_by_chiefd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) accent: Option<String>,
}
`,
    'TreePerson',
  )
  const tsFields = tsInterfaceFields(
    readFileSync(new URL('../../packages/chiefing/src/types/OrgSlice.ts', import.meta.url), 'utf8'),
    pair.tsInterface,
  )
  const result = diffShapes(rustFieldsWithBogus, tsFields)

  assert.equal(result.drifted, true)
  assert.deepEqual(
    result.rustOnly.map((f) => f.name),
    ['newlyAddedByChiefd'],
  )
  assert.deepEqual(result.tsOnly, [])
})
