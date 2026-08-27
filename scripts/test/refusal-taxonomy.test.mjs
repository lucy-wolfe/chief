// #1004: a domain refusal must never be answered as a server fault, and the
// two halves of the refusal contract must not drift.
//
// THE DEFECT THIS CLOSES, in one sentence: an agent told "unavailable" retries
// and an agent told "not terminal" acts, and the product said "unavailable" for
// both. Two instances were found the same day from two different directions,
// which is what said there were more — the audit found the whole
// `/v1/org/runtime/*`, `/v1/org/materialize/*`, `/v1/org/model|thinking/*` and
// `/v1/org/company-session-action/*` families answering **HTTP 500** for a
// runtime-generation fence, an unknown person, and ten authorization decisions
// in `close_temporary_launcher_pane`, plus every `company_error` caller
// dropping the refusal's machine code on the floor.
//
// Neither half of this is visible to any other check in the repo. A route test
// asserting `200` proves nothing about the error path; a client unit test
// proves nothing about what chiefd actually sends; and `cargo build` is
// perfectly happy with a route that hands a refusal to a 500. So the guard is
// a TEXT derivation over the real files:
//
//   1. **One status table.** No docstore route module may name an HTTP error
//      status. `route_error.rs` owns the mapping. This is the property that
//      would have failed the day `runtime_routes.rs` grew its local
//      `internal()`.
//   2. **The two halves agree.** chiefd's `REFUSAL_STATUSES` and the client's
//      `REFUSAL_STATUSES` are the same set, and every member of it is a 4xx.
//   3. **A lost CAS names itself.** Every `*_publish_cas` in `writer.rs` raises
//      the one conflict code the client's `SEQ_CONFLICT_CODE` reads. A status
//      is not enough here: 409 now legitimately carries other fences, so
//      `postOrgRouteCas` discriminates on the body's `code`, and a rename on
//      either side turns a retryable lost race into a hard failure.
//
// Shape follows this directory's standard: the real tree passes and says what
// it enumerated; a vacuous scan FAILS; and synthetic fixtures demonstrate red
// then green for each way the property can be broken.
//
// Run with `node --test scripts/test/refusal-taxonomy.test.mjs`.

import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  CAS_WRITER_FILE,
  casCodeDrift,
  casConflictCodes,
  compareRefusalSets,
  DOCSTORE_DIR,
  findRouteOwnedStatuses,
  parseRustRefusalStatuses,
  parseTsRefusalStatuses,
  parseTsSeqConflictCode,
  refusalSetShapeViolations,
  scannedRustFiles,
  stripTestModules,
} from '../refusal-taxonomy-lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const docstoreDir = join(repoRoot, DOCSTORE_DIR)
const rustTaxonomy = join(docstoreDir, 'route_error.rs')
const tsTaxonomy = join(repoRoot, 'packages', 'chiefing', 'src', 'resources', 'OrgRoutes.ts')
const casWriter = join(repoRoot, CAS_WRITER_FILE)

// ---- 1. The real tree, today ---------------------------------------------

test('every docstore route module leaves the status decision to route_error.rs', () => {
  const scanned = scannedRustFiles(docstoreDir)
  assert.ok(
    scanned.length >= 10,
    `expected the docstore surface to have at least 10 route modules, scanned ${scanned.length} ` +
      '— a scan that found almost nothing is a broken guard reporting green'
  )
  assert.ok(scanned.includes('router.rs'), 'router.rs must be in the scan')
  assert.ok(scanned.includes('runtime_routes.rs'), 'runtime_routes.rs must be in the scan')

  const findings = findRouteOwnedStatuses(docstoreDir)
  assert.deepEqual(
    findings,
    [],
    'a route module named an HTTP error status itself. The status a failure is answered with ' +
      'is a taxonomy decision, and it belongs in ' +
      `${DOCSTORE_DIR}/route_error.rs — use RouteError::refused / conflict / not_found / ` +
      'malformed / forbidden / busy / unavailable / fault, or from_chiefd for a ChiefdError. ' +
      'Offenders:\n' +
      findings.map((f) => `  ${f.file}:${f.line}  ${f.status}  ${f.text}`).join('\n')
  )
})

test('chiefd and the TypeScript client agree on exactly which statuses are refusals', () => {
  const rust = parseRustRefusalStatuses(readFileSync(rustTaxonomy, 'utf8'))
  const ts = parseTsRefusalStatuses(readFileSync(tsTaxonomy, 'utf8'))

  assert.ok(rust, `could not parse REFUSAL_STATUSES out of ${rustTaxonomy}`)
  assert.ok(ts, `could not parse REFUSAL_STATUSES out of ${tsTaxonomy}`)
  assert.ok(rust.length >= 3, 'a refusal set this small is a parse failure, not a taxonomy')

  const { missingInClient, missingInServer } = compareRefusalSets(rust, ts)
  assert.deepEqual(
    missingInClient,
    [],
    `chiefd answers ${missingInClient.join(', ')} as an actionable refusal and the client reads ` +
      'it as an outage. That is the whole defect: the agent is told "chiefd unavailable" for a ' +
      `rule it could act on. Add it to REFUSAL_STATUSES in ${tsTaxonomy}.`
  )
  assert.deepEqual(
    missingInServer,
    [],
    `the client treats ${missingInServer.join(', ')} as a refusal and chiefd never sends one. ` +
      'A refusal set wider than the server\'s decodes a genuine outage as a product rule, which ' +
      'is the same defect pointing the other way.'
  )
})

test('every refusal status is a 4xx — a 5xx can never be a refusal', () => {
  const rust = parseRustRefusalStatuses(readFileSync(rustTaxonomy, 'utf8'))
  assert.ok(rust)
  assert.deepEqual(
    refusalSetShapeViolations(rust),
    [],
    'a status outside 4xx was declared a refusal. 4xx means the caller can act; 5xx means chiefd ' +
      'could not answer. Collapsing them is the defect regardless of which direction it happens in.'
  )
})

test('the taxonomy module states the mapping it owns, so the rule is readable where it lives', () => {
  const source = readFileSync(rustTaxonomy, 'utf8')
  for (const constructor of [
    'pub fn refused(',
    'pub fn conflict(',
    'pub fn not_found(',
    'pub fn malformed(',
    'pub fn forbidden(',
    'pub fn busy(',
    'pub fn unavailable(',
    'pub fn fault(',
    'pub fn from_chiefd(',
  ]) {
    assert.ok(source.includes(constructor), `route_error.rs must expose ${constructor}…)`)
  }
  // There must be no way to mint a status without naming its class.
  assert.ok(
    !/pub fn new\(\s*status/.test(source),
    'route_error.rs must not expose a raw status constructor — every construction names the ' +
      'CLASS of outcome, which is what makes "route picks its own status" unwritable'
  )
})

test('chiefd and the client agree on the code that means "your CAS sequence is stale"', () => {
  const clientCode = parseTsSeqConflictCode(readFileSync(tsTaxonomy, 'utf8'))
  assert.ok(clientCode, `could not parse SEQ_CONFLICT_CODE out of ${tsTaxonomy}`)

  const methods = casConflictCodes(readFileSync(casWriter, 'utf8'))
  assert.ok(
    // Was 5 until `acks_publish_cas` went with the acknowledgement-receipt
    // queue, and 4 until `goal_intents_publish_cas` went with the goal
    // feature. Three genuine CAS methods remain: supervision,
    // session-maintenance and operator-escalation intents.
    methods.length >= 3,
    `expected the writer actor to carry at least 3 *_publish_cas methods, found ` +
      `${methods.length} — a scan that found almost nothing is a broken guard reporting green`
  )

  const { silent, foreign } = casCodeDrift(methods, clientCode)
  assert.deepEqual(
    silent,
    [],
    'a *_publish_cas method raises no ChiefdError::conflict at all — its sequence check is gone ' +
      'or unreachable, so a lost race commits over the winner:\n' +
      silent.map((entry) => `  ${entry.method}`).join('\n')
  )
  assert.deepEqual(
    foreign,
    [],
    `a *_publish_cas method raises a conflict code the client does not recognise as a stale ` +
      `sequence (it reads only '${clientCode}'). That 409 reaches the caller as a plain ` +
      'refusal instead of the retryable SeqConflictError it is, so a lost race becomes a hard ' +
      `failure. Fix the code, or SEQ_CONFLICT_CODE in ${tsTaxonomy}. Offenders:\n` +
      foreign.map((entry) => `  ${entry.method}  ${entry.codes.join(', ')}`).join('\n')
  )
})

// ---- 2. Non-vacuity -------------------------------------------------------

test('an empty directory FAILS the scan rather than passing it', () => {
  const dir = mkdtempSync(join(tmpdir(), 'refusal-taxonomy-empty-'))
  try {
    assert.equal(scannedRustFiles(dir).length, 0)
    // The real-tree test above asserts a floor precisely so this state cannot
    // report success. Demonstrated here rather than trusted.
    assert.deepEqual(findRouteOwnedStatuses(dir), [])
    assert.ok(
      scannedRustFiles(dir).length < 10,
      'the floor in the real-tree test is what turns this empty scan into a failure'
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// ---- 3. Demonstrated red, then green -------------------------------------

function fixture(files) {
  const dir = mkdtempSync(join(tmpdir(), 'refusal-taxonomy-fixture-'))
  mkdirSync(dir, { recursive: true })
  for (const [name, body] of Object.entries(files)) writeFileSync(join(dir, name), body)
  return dir
}

test('RED: a route that maps a domain refusal through an internal-error path is caught', () => {
  const dir = fixture({
    'route_error.rs': 'pub const REFUSAL_STATUSES: [u16; 1] = [422];\n',
    // The exact shape runtime_routes.rs carried: one local helper that turns
    // every outcome, refusal included, into a 500.
    'runtime_routes.rs': [
      'fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {',
      '    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())',
      '}',
      '',
      'async fn thinking_change() -> Result<Json<Value>, (StatusCode, String)> {',
      '    execute_thinking_change().await.map_err(internal)',
      '}',
      '',
    ].join('\n'),
  })
  try {
    const findings = findRouteOwnedStatuses(dir)
    assert.equal(findings.length, 1)
    assert.equal(findings[0].status, 'INTERNAL_SERVER_ERROR')
    assert.equal(findings[0].file, 'runtime_routes.rs')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('GREEN: the same route classifying through the taxonomy passes', () => {
  const dir = fixture({
    'route_error.rs': 'pub const REFUSAL_STATUSES: [u16; 1] = [422];\n',
    'runtime_routes.rs': [
      'fn lifecycle_error(error: &RuntimeLifecycleError) -> RouteError {',
      '    match error {',
      '        Lifecycle::Store(store) => RouteError::from_chiefd(store),',
      '        Lifecycle::Host(host) => RouteError::fault("host-step-failed", host.to_string()),',
      '    }',
      '}',
      '',
    ].join('\n'),
  })
  try {
    assert.deepEqual(findRouteOwnedStatuses(dir), [])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a status named only inside #[cfg(test)] is not a violation', () => {
  const dir = fixture({
    'route_error.rs': 'pub const REFUSAL_STATUSES: [u16; 1] = [422];\n',
    'router.rs': [
      'async fn handler() -> Result<Json<Value>, RouteError> {',
      '    Err(RouteError::not_found("unknown-company", "nope"))',
      '}',
      '',
      '#[cfg(test)]',
      'mod tests {',
      '    #[test]',
      '    fn a_foreign_slug_is_a_404() {',
      '        assert_eq!(handler().status(), StatusCode::NOT_FOUND);',
      '    }',
      '}',
      '',
    ].join('\n'),
  })
  try {
    assert.deepEqual(
      findRouteOwnedStatuses(dir),
      [],
      'a test asserting a route answered 404 is the point of the test, not a taxonomy violation'
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('stripTestModules removes the whole test module and nothing else', () => {
  const source = [
    'fn production() {}',
    '#[cfg(test)]',
    'mod tests {',
    '    fn nested() { let _ = StatusCode::IM_A_TEAPOT; }',
    '}',
    'fn after() {}',
  ].join('\n')
  const stripped = stripTestModules(source)
  assert.ok(stripped.includes('fn production()'))
  assert.ok(stripped.includes('fn after()'))
  assert.ok(!stripped.includes('IM_A_TEAPOT'))
})

test('RED: a client refusal set narrower than the server is caught, naming the status', () => {
  const drift = compareRefusalSets([400, 403, 404, 409, 422], [400, 404, 422])
  assert.deepEqual(drift.missingInClient, [403, 409])
  assert.deepEqual(drift.missingInServer, [])
})

test('RED: a client refusal set WIDER than the server is caught too', () => {
  const drift = compareRefusalSets([400, 404, 422], [400, 404, 422, 503])
  assert.deepEqual(drift.missingInServer, [503])
})

test('RED: putting a 5xx in the refusal set is caught', () => {
  assert.deepEqual(refusalSetShapeViolations([400, 404, 422, 503]), [503])
  assert.deepEqual(refusalSetShapeViolations([400, 403, 404, 409, 422]), [])
})

test('the parsers read the real declarations, and reject a shape they cannot trust', () => {
  assert.deepEqual(parseRustRefusalStatuses('pub const REFUSAL_STATUSES: [u16; 2] = [400, 422];'), [
    400, 422,
  ])
  // A declared length that disagrees with the list is a half-finished edit.
  assert.equal(
    parseRustRefusalStatuses('pub const REFUSAL_STATUSES: [u16; 3] = [400, 422];'),
    undefined
  )
  assert.equal(parseRustRefusalStatuses('const OTHER: [u16; 1] = [400];'), undefined)
  assert.deepEqual(
    parseTsRefusalStatuses('export const REFUSAL_STATUSES: readonly number[] = [400, 409]'),
    [400, 409]
  )
  assert.equal(parseTsRefusalStatuses('const REFUSAL_STATUSES = [400]'), undefined)
})

// ---- 4. The CAS conflict code, red then green -----------------------------

/** A synthetic writer actor: one CAS method per `[name, code]`, plus one
 * non-CAS publish sibling that must never be scanned. */
function writerFixture(methods) {
  const body = methods
    .map(([method, code]) =>
      [
        `    /// Raises \`seq-conflict\` — prose, not a raise site.`,
        `    pub async fn ${method}(&self, expected_seq: i64) -> Result<i64, ChiefdError> {`,
        '        self.in_transaction(move |tx| {',
        '            let current = current_seq(tx)?;',
        '            if current != expected_seq {',
        ...(code === undefined
          ? ['                return Ok(());']
          : [
              '                return Err(ChiefdError::conflict(',
              `                    "${code}",`,
              '                    expected_seq.to_string(),',
              '                    current.to_string(),',
              '                ));',
            ]),
        '            }',
        '            Ok(())',
        '        })',
        '        .await',
        '    }',
      ].join('\n')
    )
    .join('\n\n')
  return [
    'impl CompanyDb {',
    '    pub async fn supervision_publish(&self, body: String) -> Result<i64, ChiefdError> {',
    '        self.enqueue(body).await',
    '    }',
    '',
    body,
    '}',
    '',
    '#[cfg(test)]',
    'mod tests {',
    '    #[tokio::test]',
    '    async fn supervision_publish_cas_rejects_a_stale_seq() {',
    '        assert!(matches!(error, ChiefdError::conflict("not-a-real-site", ..)));',
    '    }',
    '}',
    '',
  ].join('\n')
}

test('GREEN: every CAS method raising the client\'s code passes, and non-CAS siblings are not scanned', () => {
  const methods = casConflictCodes(
    writerFixture([
      ['supervision_publish_cas', 'seq-conflict'],
      ['acks_publish_cas', 'seq-conflict'],
    ])
  )
  assert.deepEqual(methods, [
    { method: 'acks_publish_cas', codes: ['seq-conflict'] },
    { method: 'supervision_publish_cas', codes: ['seq-conflict'] },
  ])
  const drift = casCodeDrift(methods, 'seq-conflict')
  assert.deepEqual(drift.silent, [])
  assert.deepEqual(drift.foreign, [])
})

test('RED: a CAS method that renames its conflict code is caught, naming the method', () => {
  const methods = casConflictCodes(
    writerFixture([
      ['supervision_publish_cas', 'seq-conflict'],
      // The drift the client cannot see: a 409 that IS a stale sequence, under
      // a name the client reads as some other fence.
      ['acks_publish_cas', 'cas-mismatch'],
    ])
  )
  const drift = casCodeDrift(methods, 'seq-conflict')
  assert.deepEqual(drift.foreign, [{ method: 'acks_publish_cas', codes: ['cas-mismatch'] }])
  assert.deepEqual(drift.silent, [])
})

test('RED: a CAS method whose sequence check no longer raises at all is caught', () => {
  const methods = casConflictCodes(
    writerFixture([
      ['supervision_publish_cas', 'seq-conflict'],
      ['operator_escalation_intents_publish_cas', undefined],
    ])
  )
  const drift = casCodeDrift(methods, 'seq-conflict')
  assert.deepEqual(drift.silent, [{ method: 'operator_escalation_intents_publish_cas', codes: [] }])
})

test('RED: renaming the client constant away from chiefd is caught from the other side', () => {
  const methods = casConflictCodes(writerFixture([['supervision_publish_cas', 'seq-conflict']]))
  const drift = casCodeDrift(methods, 'stale-seq')
  assert.deepEqual(drift.foreign, [{ method: 'supervision_publish_cas', codes: ['seq-conflict'] }])
})

test('the client constant parser rejects a shape it cannot trust', () => {
  assert.equal(parseTsSeqConflictCode("export const SEQ_CONFLICT_CODE = 'seq-conflict'"), 'seq-conflict')
  assert.equal(parseTsSeqConflictCode("const SEQ_CONFLICT_CODE = 'seq-conflict'"), undefined)
  assert.equal(parseTsSeqConflictCode("export const SEQ_CONFLICT_CODE = ''"), undefined)
})
