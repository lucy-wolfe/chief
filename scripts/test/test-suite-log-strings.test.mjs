// THE LIVE SUITE GREPS THE PRODUCT'S LOG STRINGS, AND NOTHING KNEW.
//
// `docs/testing/TEST_SUITE.md` decides whether a case passes by grepping
// `daemon.log` and the actuator pane for literal strings the product emits.
// That makes the document COUPLED to those strings — and on 2026-08-18 two of
// our own fixes silently invalidated two of its checks, in one day:
//
//   * `d8f4e7714` demoted every fast 2xx served request to DEBUG, which took
//     `POST /v1/org/person/wake` with it. §4.3's wake count is the instrument
//     Case 6 uses to prove one click produced EXACTLY ONE wake. For the hours
//     that shipped it returned 0 before AND after a wake that demonstrably
//     happened — so it could not have detected a genuine DOUBLE wake either.
//     (`e59d43042` restored it; the string is guarded here now.)
//   * `0daa36b0b` deleted `planned=` and `actuated=` end to end. §4.3 did not
//     merely run a dead grep — it spent a paragraph teaching a runner how to
//     interpret `planned>actuated`, a comparison that can never be emitted
//     again.
//
// Both kept RETURNING A NUMBER. That is the same hollow-green failure as an
// assertion about a value's shape that never drives the condition, one layer
// out: the check still ran, still produced a plausible reading, and had
// stopped being able to detect the thing it exists for. A check that returns a
// believable `0` is worse than one that errors.
//
// # What this guard does, and the one thing it deliberately does NOT do
//
// It extracts the log-shaped literals `TEST_SUITE.md` greps for, and asserts
// each one's EXPECTATION against the source: `present` strings must still be
// emitted somewhere, and `absent` strings must still be gone. Both directions
// are real — Case 24 exists to assert `planned=`/`actuated=` never come back,
// so a guard that only checked presence would fail that case's own greps and
// be deleted within a day.
//
// It does NOT try to parse arbitrary shell. The extractor is deliberately
// narrow (see `LOG_SHAPED`) and everything it finds must have a row in
// `EXPECTATIONS`. A pattern with no row FAILS, naming it — so the cost of
// adding a grep to the doc is one line here, and the cost of NOT adding it is
// a red build rather than a check that quietly rots. That is the same shape as
// `guard-wiring-manifest.mjs` and `IntercomSeamClassification`'s bucket map:
// the judgement is recorded as data, so a later change has to restate it.
//
// A stale row fails too. A row naming a pattern the doc no longer greps is a
// guard describing a document that has moved on.

import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'
import test from 'node:test'
import assert from 'node:assert/strict'

const REPO = new URL('../..', import.meta.url).pathname
const DOC = join(REPO, 'docs/testing/TEST_SUITE.md')

/**
 * Every log-shaped literal the suite greps, and what must be true of it.
 *
 * `present` — the product must still emit this; the case that greps it is
 *   reading a real signal.
 * `absent`  — the product must NOT emit this; a case asserts its absence, and
 *   a reappearance is the regression.
 *
 * The `why` is not decoration: when this guard fails, the message is the first
 * thing the person who broke it reads, and "some string moved" is not enough
 * to act on.
 */
const EXPECTATIONS = {
  // ---- present: the suite reads these to decide a case -------------------
  'event="org.person.wake.applied"': {
    expect: 'present',
    why: '§4.3 counts applied wakes; Case 6 proves one click is one wake'
  },
  'path=/v1/org/person/wake': {
    expect: 'present',
    why: "§4.3's request count — the literal subject of Case 6's \"exactly one signed request\". Demoting this below INFO blinds Case 6 (see d8f4e7714)"
  },
  'desired=': {
    expect: 'present',
    why: '§4.2/§4.3 and Case 24 read the reconcile line; this replaced planned=/actuated='
  },
  'reconcile actuation pass': {
    expect: 'present',
    why: "§4.3's actuation record — the line whose notes name WHO was launched and why. A live company relaunched six people after an operator stood it down with daemon.log holding nothing but 'supervision cycle committed', because the level asked only whether the pass recorded something NEW and a wake grant for an already-desired person records nothing new (chiefd-daemon run.rs actuation_pass_log_level)"
  },
  'docstore.request': {
    expect: 'present',
    why: 'Case 25 counts these to prove the log stays readable'
  },
  'the pass FAILED after ': {
    expect: 'present',
    why: "Case 20's whole signature. Greped as a PREFIX because the step index varies with plan ordering"
  },
  'has failed to stay up': {
    expect: 'present',
    why: 'Case 35 reads this line to watch the retry count climb past the limit that used to end it, and §4.8 reads it to tell a real crash loop from the actuator billing people for its own wreckage'
  },
  'no launch spec': {
    expect: 'present',
    why: 'Case 19 treats this as a FAIL-by-reason: it names an internal lookup, not something an operator can repair'
  },
  'event="sidebar.wake.refused-by-gate"': {
    expect: 'present',
    why: "Case 19's POSITIVE signal for a click on a gate-refused person, emitted exactly where the wake POST used to be. Without it that case rests on an unmoved counter, which a click that never landed satisfies equally well (e9b7b0202)"
  },
  'cannot be launched': {
    expect: 'present',
    why: "Case 20's CORRECT reading — StepError::LaunchRefused's own Display. Its presence is how a runner tells a skipped refusal from a failed pass (9f56f997a)"
  },
  'is not in the launch roster': {
    expect: 'present',
    why: 'Case 20 distinguishes this CORRECT fail-stop from the bug: a person never a candidate for lookup is structurally different from one the gate declined (#52)'
  },
  '@organization_sidebar': {
    expect: 'present',
    why: 'Case 17 counts tagged rails beside rail PROCESSES; the pair is the duplicate-rail signature'
  },
  'created moments ago': {
    expect: 'present',
    why: "Case 16 reads the founding boot's own copy out of the pane argv"
  },
  'next real piece of work': {
    expect: 'present',
    why: 'Case 16 proves a later hire still gets to work — the direction that rots if only the founding boot is covered'
  },
  'did not fit the model': {
    expect: 'present',
    why: "Case 18's pane half greps the card's own sentence. It lives in providerRequestTooLargeSpec (packages/piing/extensions/card-style.ts) and nowhere else; the whole defect was that this read 0 in a live pane while every event assertion was green"
  },
  'will not be retried': {
    expect: 'present',
    why: "Case 18's second sentence, and the one that tells the reader nothing is coming. Deleting it leaves a card that names a failure without saying it is permanent, which is what sends an operator back to check provider health that is fine"
  },
  'maximum context length is': {
    expect: 'present',
    why: "The PROVIDER's own words, which Case 18 greps expecting ZERO of in the pane — the raw dump the card replaces. It is in the product exactly once, as providerRequestTooLargeError's detection pattern, and that pattern existing is what makes the measurement mean anything: delete it and the overflow is classified as an ordinary provider error, no card is built, and the grep reads 0 for the wrong reason"
  },
  // ---- absent: a case asserts these never come back ----------------------
  'planned=': {
    expect: 'absent',
    why: 'deleted by 0daa36b0b — it printed desired_people under a name two designs old. Case 24 asserts its absence'
  },
  'actuated=': {
    expect: 'absent',
    why: 'deleted by 0daa36b0b — a field permanently zero is worse than no field, because a reader branches on it. Case 24 asserts its absence'
  }
}

/** Source trees whose strings the suite reads. Test files count: a string
 *  asserted only by a test is still a string the product emits. */
const SOURCE_ROOTS = [
  'apps/chiefd/crates',
  'packages/piing/extensions',
  'packages/chiefing/src'
]
const SOURCE_EXTENSIONS = ['.rs', '.ts']

function sourceFiles(dir, out = []) {
  let entries
  try {
    entries = readdirSync(dir)
  } catch {
    return out
  }
  for (const entry of entries) {
    if (entry === 'node_modules' || entry === 'target' || entry === 'dist') continue
    const full = join(dir, entry)
    const stat = statSync(full)
    if (stat.isDirectory()) sourceFiles(full, out)
    else if (SOURCE_EXTENSIONS.some((ext) => entry.endsWith(ext))) out.push(full)
  }
  return out
}

/** One haystack, read once. */
function sourceHaystack() {
  const files = SOURCE_ROOTS.flatMap((root) => sourceFiles(join(REPO, root)))
  assert.ok(files.length > 100, `expected a real source tree, found ${files.length} files`)
  return files.map((file) => readFileSync(file, 'utf8')).join('\n')
}

/**
 * Is this token a LOG-SHAPED literal rather than shell or regex noise?
 *
 * Narrow on purpose. A guard with false positives is worse than the rot it
 * catches — it gets suppressed, and then it catches nothing. So a token is
 * only considered when it carries one of the marks a product log string has
 * (`=`, a dotted event name, a `@tag`, or real words with spaces) AND carries
 * none of the marks shell or regex noise has.
 */
function isLogShaped(token) {
  // Shell variables and regex machinery: not a literal at all.
  if (/[$^*+?()[\]\\]/.test(token)) return false
  if (token.length < 4) return false
  // Bare single words -- `refus`, `converged`, `held`, `WARN`, `FAILED` -- are
  // fragments and level names, not strings whose disappearance is a defect.
  return /[=.@]/.test(token) || token.trim().includes(' ')
}

/** Every log-shaped literal the doc greps, in first-seen order. */
function docPatterns() {
  const doc = readFileSync(DOC, 'utf8')
  const blocks = [...doc.matchAll(/```(?:bash)?\n([\s\S]*?)```/g)].map((m) => m[1])
  const found = new Set()
  for (const block of blocks) {
    for (const call of block.matchAll(/grep[\w\s-]*\s+'([^']+)'|grep[\w\s-]*\s+"([^"]+)"/g)) {
      const raw = call[1] ?? call[2]
      // A single grep may carry an alternation: `desired=|refus` is two
      // patterns, and the one that matters is the one that could rot.
      for (const piece of raw.split(/\\\||\|/)) {
        // Strip a trailing regex quantifier/class: `desired=[0-9]*` is a grep
        // for `desired=`.
        const token = piece.replace(/\[[^\]]*\][*+?]?$/, '')
        if (isLogShaped(token)) found.add(token)
      }
    }
  }
  return found
}

/** Where a pattern must be looked for in the source. */
function sourceNeedle(pattern) {
  // `event="x.y"` is a tracing field in Rust (`event = "x.y"`) and a property
  // in TS. The stable part across both is the quoted VALUE.
  const asEvent = pattern.match(/^event="(.+)"$/)
  if (asEvent) return asEvent[1]
  // `path=/v1/...` is the ROUTE, which is what the source declares.
  const asPath = pattern.match(/^path=(\/.+)$/)
  if (asPath) return asPath[1]
  // `name=` is a tracing FIELD. Rust writes it as `name = value,` on its own
  // line inside the macro, so anchor to the line start — a plain
  // `includes("name = ")` also matches `let name = ...`, an ordinary local
  // binding that emits nothing. That false positive is not hypothetical: it
  // reported `planned=`/`actuated=` as "back" on the strength of
  // `let actuated = Arc::new(..)` in a test helper and `let planned =
  // manifest.people.get(id)?` in the roster, neither of which is a log line.
  // A guard that cries wolf gets suppressed, and then it guards nothing.
  const asField = pattern.match(/^(\w+)=$/)
  if (asField) return new RegExp(`^[ \\t]*${asField[1]} = `, 'm')
  return pattern.trimEnd()
}

/** Does the source carry this needle? String needles are substrings; field
 *  needles are line-anchored patterns (see `sourceNeedle`). */
function emitted(haystack, needle) {
  return needle instanceof RegExp ? needle.test(haystack) : haystack.includes(needle)
}

test('every log string TEST_SUITE.md greps has a recorded expectation', () => {
  const patterns = docPatterns()
  const unrecorded = [...patterns].filter((p) => !(p in EXPECTATIONS)).sort()
  assert.deepEqual(
    unrecorded,
    [],
    `TEST_SUITE.md greps ${unrecorded.length} log string(s) with no row in EXPECTATIONS.\n` +
      `Add a row saying whether the product must emit it ('present') or must not ('absent'),\n` +
      `and WHY — the reason is what the next person who breaks it reads:\n` +
      unrecorded.map((p) => `  ${JSON.stringify(p)}`).join('\n')
  )
})

test('no expectation names a string the suite has stopped greping', () => {
  const patterns = docPatterns()
  const stale = Object.keys(EXPECTATIONS)
    .filter((p) => !patterns.has(p))
    .sort()
  assert.deepEqual(
    stale,
    [],
    `EXPECTATIONS names ${stale.length} string(s) TEST_SUITE.md no longer greps.\n` +
      `A guard describing a document that has moved on protects nothing — delete the row,\n` +
      `or restore the check in the doc if it was dropped by accident:\n` +
      stale.map((p) => `  ${JSON.stringify(p)}`).join('\n')
  )
})

test('every string the suite reads as a signal is still emitted', () => {
  const haystack = sourceHaystack()
  const missing = Object.entries(EXPECTATIONS)
    .filter(([, rule]) => rule.expect === 'present')
    .filter(([pattern]) => !emitted(haystack, sourceNeedle(pattern)))
    .map(([pattern, rule]) => `  ${JSON.stringify(pattern)} — ${rule.why}`)
  assert.deepEqual(
    missing,
    [],
    `TEST_SUITE.md decides a case by greping ${missing.length} string(s) the product no longer emits.\n` +
      `The grep will return 0 or nothing, and a runner will read that as a verdict —\n` +
      `which is exactly how the wake count went blind for hours on 2026-08-18.\n` +
      `Either restore the string, or update the doc AND the row here to match reality:\n` +
      missing.join('\n')
  )
})

/**
 * THE OTHER HALF, and the one a string search cannot reach.
 *
 * `d8f4e7714` did not DELETE `/v1/org/person/wake` — the route existed
 * throughout, so the presence check above would have stayed green while §4.3's
 * count read 0. What changed was the LEVEL: the line stopped being emitted at
 * the default one. A string that exists and is never printed is exactly as
 * blind as a string that is gone, and it is the harder of the two to see.
 *
 * `e59d43042` made that decision readable: `POLLING_READ_SEGMENTS` is the
 * closed list of final path segments demoted to DEBUG, and everything else —
 * every mutation — stays at INFO. So the rule the suite depends on is
 * checkable as data: a route the doc greps must not end in a demoted segment.
 * Adding `wake` to that list would turn this red instead of turning Case 6
 * into a check that always passes.
 */
test('no route the suite greps has been demoted out of the default log', () => {
  const router = readFileSync(
    join(REPO, 'apps/chiefd/crates/chiefd-api/src/docstore/router.rs'),
    'utf8'
  )
  const declaration = router.match(/const POLLING_READ_SEGMENTS[^=]*=\s*([\s\S]*?);/)
  assert.ok(
    declaration,
    'POLLING_READ_SEGMENTS is gone from docstore/router.rs. It is the rule this ' +
      'check reads; if the demotion moved, point this guard at its new home rather ' +
      'than deleting the check.'
  )
  const demoted = [...declaration[1].matchAll(/"([^"]+)"/g)].map((m) => m[1])
  assert.ok(demoted.length > 0, `expected demoted segments, parsed: ${declaration[1]}`)

  // Built with an explicit loop rather than a chained map/filter: pairing a
  // regex match with its rule in a tuple gives the array a union element type,
  // and `rule.why` then does not typecheck. The repo typechecks .mjs too.
  const blinded = []
  for (const [pattern, rule] of Object.entries(EXPECTATIONS)) {
    if (rule.expect !== 'present') continue
    const route = pattern.match(/^path=(\/.+)$/)
    if (!route) continue
    const segment = route[1].split('/').pop()
    if (segment !== undefined && demoted.includes(segment)) {
      blinded.push(`  ${route[1]} — ${rule.why}`)
    }
  }

  assert.deepEqual(
    blinded,
    [],
    `${blinded.length} route(s) the suite counts have been demoted to DEBUG.\n` +
      `The string still exists, so nothing else notices — and the grep silently\n` +
      `returns 0 whatever happens, which is how Case 6 went blind for hours:\n` +
      blinded.join('\n') +
      `\ndemoted segments: ${demoted.join(', ')}`
  )
})

test('every string a case asserts the absence of is still absent', () => {
  const haystack = sourceHaystack()
  const returned = Object.entries(EXPECTATIONS)
    .filter(([, rule]) => rule.expect === 'absent')
    .filter(([pattern]) => emitted(haystack, sourceNeedle(pattern)))
    .map(([pattern, rule]) => `  ${JSON.stringify(pattern)} — ${rule.why}`)
  assert.deepEqual(
    returned,
    [],
    `${returned.length} string(s) a case asserts are GONE have come back:\n` +
      returned.join('\n')
  )
})
