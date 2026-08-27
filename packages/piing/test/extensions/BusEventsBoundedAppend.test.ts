import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, describe, expect, test } from 'vitest'

import {
  appendBoundedJsonlLine,
  BUS_EVENTS_MAX_BYTES,
  resetBusEventsSizeTrackingForTests
} from '../../extensions/bus-events-bounded-append'

// #964: bus/events.jsonl had three producers and only one respected any
// bound. This locks the shared helper the surviving piing producer
// (organization-intercom.ts) routes its bus/events.jsonl append
// through, in both directions -- a rotation on an audit trail is not a
// performance tweak: an eager or wrong rotation destroys history, which is
// worse than the file being oversized.
//
// JSON.stringify below builds plain-data test fixture lines, not
// production formatting -- same class, same disable/enable pattern as
// packages/piing/test/support/JsonFixture.ts's own header (#833/#842): no
// `@tribes-terminal/foundation` (the rule's suggested `toJsonTreeString`/
// `ensureJsonTreeString` replacement) is a dependency of this repo.

let dir: string | undefined

afterEach(() => {
  resetBusEventsSizeTrackingForTests()
  if (dir) {
    rmSync(dir, { recursive: true, force: true })
    dir = undefined
  }
})

function freshDir(): string {
  dir = mkdtempSync(join(tmpdir(), 'bus-events-bounded-append-'))
  return dir
}

interface Fixture {
  i: number
}

function fixtureLine(fields: Record<string, unknown>): string {
  /* eslint-disable lucy/no-json-stringify */
  // Plain-data test fixture, not production formatting -- see this file's
  // header (#833/#842).
  return JSON.stringify(fields)
  /* eslint-enable lucy/no-json-stringify */
}

function parseFixtureLine(line: string): Fixture {
  // Declared-type narrowing from JSON.parse's `any`, not a type assertion
  // (`assertionStyle: 'never'` forbids `as`/`<Type>` here) -- assigning
  // `any` to a more specific declared type is plain TypeScript, not an
  // assertion.
  const parsed: Fixture = JSON.parse(line)
  return parsed
}

describe('appendBoundedJsonlLine', () => {
  test('ARM: a sequence of appends whose total exceeds maxBytes rotates to .1 before the crossing line', () => {
    const root = freshDir()
    const path = join(root, 'events.jsonl')
    const maxBytes = 4096
    const filler = 'x'.repeat(200)
    for (let i = 0; i < 400; i += 1) {
      appendBoundedJsonlLine(path, fixtureLine({ i, filler }), maxBytes)
    }
    const live = statSync(path).size
    const rotated = statSync(`${path}.1`).size
    expect(live).toBeLessThanOrEqual(maxBytes + 512)
    expect(rotated).toBeLessThanOrEqual(maxBytes + 512)
    // A third generation must never accumulate -- the ceiling is 2x the cap.
    expect(existsSync(`${path}.2`)).toBe(false)
    expect(live + rotated).toBeLessThanOrEqual(2 * maxBytes + 1024)
  })

  test('CONTROL: a sequence of appends whose total stays under maxBytes never rotates', () => {
    const root = freshDir()
    const path = join(root, 'events.jsonl')
    const maxBytes = 1024 * 1024
    for (let i = 0; i < 20; i += 1) {
      appendBoundedJsonlLine(path, fixtureLine({ i, small: true }), maxBytes)
    }
    expect(existsSync(`${path}.1`)).toBe(false)
    // Every line survives, in order -- a wrongly-firing rotation would have
    // silently discarded the earlier ones.
    const lines = readFileSync(path, 'utf8').trim().split('\n')
    expect(lines).toHaveLength(20)
    expect(parseFixtureLine(lines[0]).i).toBe(0)
    expect(parseFixtureLine(lines[19]).i).toBe(19)
  })

  test('a rotation preserves the pre-rotation content in .1 rather than discarding it', () => {
    const root = freshDir()
    const path = join(root, 'events.jsonl')
    const maxBytes = 2048
    const filler = 'y'.repeat(300)
    // Fill past the cap once, forcing exactly one rotation.
    for (let i = 0; i < 10; i += 1) {
      appendBoundedJsonlLine(path, fixtureLine({ i, filler }), maxBytes)
    }
    expect(existsSync(`${path}.1`)).toBe(true)
    const rotatedLines = readFileSync(`${path}.1`, 'utf8').trim().split('\n')
    // The rotated generation is real content, not an empty file the rename
    // happened to create.
    expect(rotatedLines.length).toBeGreaterThan(0)
    expect(() => parseFixtureLine(rotatedLines[0])).not.toThrow()
  })

  test('an existing on-disk file (from a prior process) is respected on first append, not treated as empty', () => {
    const root = freshDir()
    const path = join(root, 'events.jsonl')
    const maxBytes = 300
    // Simulate a file already OVER the cap, written by a PRIOR process --
    // this process's in-memory tracked-size cache starts cold and must
    // stat() rather than assume zero.
    writeFileSync(path, `${fixtureLine({ preexisting: true, filler: 'w'.repeat(400) })}\n`)
    appendBoundedJsonlLine(path, fixtureLine({ next: true }), maxBytes)
    // The pre-existing near-cap content plus this new line crosses maxBytes,
    // so this append must have rotated -- proving the cold cache correctly
    // stat()s instead of starting from an assumed-empty file.
    expect(existsSync(`${path}.1`)).toBe(true)
    expect(readFileSync(`${path}.1`, 'utf8')).toContain('preexisting')
  })
})

describe('BUS_EVENTS_MAX_BYTES is now the sole authority for the bound (#751/G5)', () => {
  // REWRITTEN. This was a cross-file "same-value guard": it read
  // `apps/cli/src/legacy/organization/org-log.ts`'s `ORG_JOURNAL_MAX_BYTES`
  // and asserted the two literals matched, because neither side could import
  // the other. That peer no longer exists — the seventeen ported TypeScript
  // modules were deleted (`org-log.ts` among them) and chiefd's journal is a
  // 48h rolling SQLite window (`chiefd-core/src/store/event_journal.rs`),
  // not a byte-capped file. So the guard was reading a path that is gone and
  // failing with ENOENT, which is a stale test, not a caught regression.
  //
  // The contract it was protecting still exists, it just moved: with the
  // launcher-side producer deleted, this constant is the ONLY bound on
  // `bus/events.jsonl`, and the drift risk is no longer "two files disagree"
  // but "a producer inlines its own number instead of importing this one".
  // That is what is asserted now, plus the value itself, so a silent change
  // to either is still loud.
  const EXTENSIONS_ROOT = join(import.meta.dirname, '../../extensions')
  const PRODUCERS = ['organization-intercom.ts'] as const

  test('the value is frozen at 128 MiB', () => {
    expect(BUS_EVENTS_MAX_BYTES).toBe(128 * 1024 * 1024)
  })

  test.each(PRODUCERS)('%s imports the shared bound rather than inlining a number', (producer) => {
    const source = readFileSync(join(EXTENSIONS_ROOT, producer), 'utf8')
    expect(source).toMatch(
      /import \{[^}]*\bBUS_EVENTS_MAX_BYTES\b[^}]*\} from "\.\/bus-events-bounded-append"/
    )
    expect(source).toContain('BUS_EVENTS_MAX_BYTES)')
  })

  test('no producer carries its own byte literal for this file', () => {
    for (const producer of PRODUCERS) {
      const source = readFileSync(join(EXTENSIONS_ROOT, producer), 'utf8')
      expect(source, `${producer} inlines the bound`).not.toContain('128 * 1024 * 1024')
    }
  })

  test('negative self-check: the import scan can actually miss', () => {
    expect('import { somethingElse } from "./bus-events-bounded-append";').not.toMatch(
      /import \{[^}]*\bBUS_EVENTS_MAX_BYTES\b[^}]*\} from "\.\/bus-events-bounded-append"/
    )
  })
})
