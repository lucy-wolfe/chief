/**
 * EACH REMINDER FLOOR HAS TWO HOMES, AND THIS IS WHAT KEEPS EACH ONE VALUE.
 *
 * chiefd refuses a reminder armed below `MIN_REMINDER_INTERVAL_MS` (the delay
 * floor, every reminder) or, when it recurs, below
 * `MIN_RECURRING_REMINDER_INTERVAL_MS` (the cadence floor). The arm tool
 * mirrors both: the schema's `minimum` is the delay floor, because a JSON
 * schema cannot express a bound that depends on another field, and the cadence
 * floor is named in the description.
 *
 * The daemon is authoritative — a caller that somehow sends less is refused
 * server-side, with a refusal that explains itself — but a schema that
 * advertises a cadence the daemon rejects teaches every agent to ask for
 * something it cannot have.
 *
 * Two independent copies of one contract value, in two languages, is a defect
 * class this repository has already paid for in a live outage. So the Rust side
 * is READ, never copied.
 *
 * # Two floors, and only one of them is a literal
 *
 * `MIN_REMINDER_INTERVAL_MS` is the DELAY floor and is still written as a
 * number — `60 * 1_000` — because a one-shot's interval is a delay, not a
 * cadence, and it is coupled to nothing.
 *
 * `MIN_RECURRING_REMINDER_INTERVAL_MS` is the CADENCE floor and is not a
 * literal at all:
 *
 *     pub const MIN_RECURRING_REMINDER_INTERVAL_MS: i64 =
 *         2 * crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS;
 *
 * # Why this EVALUATES the relation instead of matching a number
 *
 * A regex hunting for a number beside `MIN_RECURRING_REMINDER_INTERVAL_MS`
 * would find the `2`, or nothing — and a version that had once matched a
 * literal would keep reporting green for ever while the real floor moved
 * underneath it. **A derived constant must be derived by its reader too**: this
 * test resolves the named constants out of the Rust source and computes, so the
 * day the lease moves — as it already did once, from 120s to 300s, leaving the
 * old floor stranded and a live company resident for ever — the computation
 * moves with it and every assertion here is about the value that actually
 * ships.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  MIN_RECURRING_REMINDER_INTERVAL_MS,
  MIN_REMINDER_INTERVAL_MS
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const SUPERVISION_RS = join(REPO_ROOT, 'apps/chiefd/crates/chiefd-core/src/store/supervision.rs')
const ACTIVITY_RS = join(REPO_ROOT, 'apps/chiefd/crates/chiefd-core/src/store/activity.rs')
const INTERCOM_TS = join(REPO_ROOT, 'packages/piing/extensions/organization-intercom.ts')

/**
 * A Rust `pub const <name>: i64 = …;`, EVALUATED.
 *
 * Written without a single `null` comparison or non-null assertion, which the
 * workspace lint forbids: every step is a `?.` with a `??` default, and an
 * unresolvable declaration produces an empty string that the length check
 * turns into a loud refusal. The refusal matters more than the tidiness — a
 * silent `0` here would satisfy every parity assertion below while describing
 * a floor nobody ships.
 */
function rustConstant(source: string, name: string): number {
  const expression = (new RegExp(`pub const ${name}: i64 =\\s*([^;]+);`).exec(source)?.[1] ?? '')
    .replace(/crate::[a-z_:]+::/g, '')
    // Numeric separators only. Stripping every underscore would also flatten
    // the NAME of a referenced constant, and the recursive lookup below would
    // then hunt for a symbol that does not exist and report itself blind —
    // measured while writing this, which is why the replacement is narrow.
    .replace(/(\d)_(?=\d)/g, '$1')
    .trim()
  if (expression.length === 0) {
    throw new Error(`${name} could not be read from the Rust source — this test has gone blind`)
  }
  // Only the shapes these constants actually use. Anything else refuses rather
  // than being guessed at.
  const product = /^(\d+)\s*\*\s*(\d+)$/.exec(expression)?.slice(1) ?? []
  if (product.length === 2) return Number(product[0]) * Number(product[1])
  const single = /^(\d+)$/.exec(expression)?.slice(1) ?? []
  if (single.length === 1) return Number(single[0])
  const named = /^(\d+)\s*\*\s*([A-Z_]+)$/.exec(expression)?.slice(1) ?? []
  if (named.length === 2) {
    return Number(named[0]) * rustConstant(readFileSync(ACTIVITY_RS, 'utf8'), named[1] ?? '')
  }
  throw new Error(`${name} is declared as \`${expression}\`, which this test cannot evaluate`)
}

describe('the reminder floors, across both languages', () => {
  test('both floors match the values the daemon enforces', () => {
    const supervision = readFileSync(SUPERVISION_RS, 'utf8')
    const delayFloor = rustConstant(supervision, 'MIN_REMINDER_INTERVAL_MS')
    const cadenceFloor = rustConstant(supervision, 'MIN_RECURRING_REMINDER_INTERVAL_MS')
    expect(delayFloor).toBeGreaterThan(0)
    expect(cadenceFloor).toBeGreaterThan(0)
    expect(MIN_REMINDER_INTERVAL_MS).toBe(delayFloor)
    expect(MIN_RECURRING_REMINDER_INTERVAL_MS).toBe(cadenceFloor)
  })

  test('the recurring floor clears two settle windows, computed from the Rust source', () => {
    const lease = rustConstant(
      readFileSync(ACTIVITY_RS, 'utf8'),
      'ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS'
    )
    expect(lease).toBeGreaterThan(0)
    // The same relation the Rust test pins, asserted here against the value the
    // TOOL advertises — so a number edited by hand to something merely large
    // enough still fails, because it must equal the derived floor.
    expect(MIN_RECURRING_REMINDER_INTERVAL_MS).toBe(2 * lease)
    // And the two floors are different questions: a delay is not a cadence.
    expect(MIN_REMINDER_INTERVAL_MS).toBeLessThan(MIN_RECURRING_REMINDER_INTERVAL_MS)
  })

  test('the reader is not vacuous: it evaluates a derived constant, not a literal', () => {
    // If the recurring floor is ever rewritten as a bare number in Rust, the
    // parity assertions above still pass — and the coupling to the lease is
    // silently gone. This is the arm that notices.
    const source = readFileSync(SUPERVISION_RS, 'utf8')
    const derived = new RegExp(
      'pub const MIN_RECURRING_REMINDER_INTERVAL_MS: i64 =\\s*' +
        '2 \\* crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS;'
    )
    expect(source).toMatch(derived)
  })

  test('the arm schema names the constant rather than repeating a number', () => {
    // Not vacuous, and deliberately about the SOURCE: asserting the schema's
    // minimum equals the constant would be circular, since it is built from it.
    // What can regress is somebody writing the number back in — which is how
    // this value got two homes in the first place. So the check is that the
    // schema line still REFERENCES the constant.
    const extension = readFileSync(INTERCOM_TS, 'utf8')
    expect(extension).toMatch(/minimum: MIN_REMINDER_INTERVAL_MS/)
    expect(extension).not.toMatch(/minimum: 60_000/)
    expect(extension).not.toMatch(/minimum: 600_000/)
  })
})
