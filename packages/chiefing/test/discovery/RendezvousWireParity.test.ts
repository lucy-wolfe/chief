/**
 * THE READER MUST SURVIVE WHAT THE WRITER WRITES.
 *
 * # The outage this exists for
 *
 * On 2026-08-26 the daemon began publishing an additive `build` field on its
 * rendezvous — which binary it is running, so a stale daemon can be replaced.
 * Correct on the writer's side, backward compatible, and it killed every
 * person in a live company: the TypeScript reader in each pane refused the
 * whole record with `unknown field "build" — this daemon is a different
 * build`, Pi exited 1, and the panes died at start-up.
 *
 * Nothing tested the two halves TOGETHER. The Rust side had a wire test, the
 * TypeScript side had a parser test, each passed against its own idea of the
 * record, and the seam between them — a real writer's output read by the real
 * reader — was tested by nobody. That seam is where the packaging bug lived
 * too, one language boundary over. This is that seam, for this record.
 *
 * # Why the field list is DERIVED and never typed here
 *
 * A list of field names copied into this file would be a third source of
 * truth: it would drift from the struct exactly when somebody adds a field,
 * which is the one moment this test exists to cover. So the writer's fields
 * are parsed out of the Rust struct as text — the same technique
 * `RustToolListParity.test.ts` uses, and for the same reason.
 *
 * # What it asserts, in both directions
 *
 * FORWARD: a rendezvous carrying every field the writer can emit is accepted
 * by this reader, including fields this reader has never modeled.
 * NON-VACUOUS: the parse of the Rust struct actually found the fields we know
 * are there, so a rename that empties the list fails loudly instead of making
 * the forward assertion trivially true.
 */
import { readFileSync, statSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import { parseDaemonRendezvous } from '@/discovery/Rendezvous'

/** Walked, not counted in `..` segments — the same helper shape as this
 * directory's other cross-language pins, which do not break when a file moves
 * one level. */
function repoRoot(): string {
  let dir = dirname(fileURLToPath(import.meta.url))
  for (let depth = 0; depth < 10; depth += 1) {
    try {
      if (statSync(join(dir, 'apps', 'chiefd', 'crates')).isDirectory()) return dir
    } catch {
      // keep walking up
    }
    dir = dirname(dir)
  }
  throw new Error('could not locate the repo root (no apps/chiefd/crates above this test)')
}

const RENDEZVOUS_RS = join(repoRoot(), 'apps/chiefd/crates/host-primitives/src/rendezvous.rs')

/** `serde(rename_all = "camelCase")`, applied the way serde applies it. */
function camelCase(field: string): string {
  return field.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase())
}

/**
 * Every field the Rust `DaemonRendezvous` serializes, in the spelling it puts
 * on the wire.
 */
function writerFields(): string[] {
  const source = readFileSync(RENDEZVOUS_RS, 'utf8')
  const body = /pub struct DaemonRendezvous \{([\s\S]*?)\n\}/.exec(source)?.[1] ?? ''
  if (body.length === 0) {
    throw new Error('DaemonRendezvous struct not found — this test would be blind')
  }
  return [...body.matchAll(/^\s{4}pub ([a-z_][a-z0-9_]*):/gm)].flatMap((match) =>
    typeof match[1] === 'string' ? [camelCase(match[1])] : []
  )
}

/** A plausible value for one wire field. The four the reader models must be
 * well-formed; anything else is a field it has never heard of, and its shape
 * is deliberately arbitrary — the reader owes it nothing but indifference. */
function wireValue(field: string): unknown {
  switch (field) {
    case 'dir':
      return '/work/anvils'
    case 'key':
      return 'aaaaaaaaaaaa'
    case 'url':
      return 'http://127.0.0.1:8793'
    case 'pid':
      return 4242
    default:
      return { unmodeled: true, by: 'this reader' }
  }
}

describe('the rendezvous wire, across both languages', () => {
  it('parses a field list out of the Rust writer rather than assuming one', () => {
    const fields = writerFields()
    expect(fields).toContain('dir')
    expect(fields).toContain('key')
    expect(fields).toContain('url')
    expect(fields).toContain('pid')
    expect(fields.length).toBeGreaterThanOrEqual(4)
  })

  it('accepts a rendezvous carrying every field the daemon can write', () => {
    const fields = writerFields()
    const body = Object.fromEntries(fields.map((field) => [field, wireValue(field)]))
    const parsed = parseDaemonRendezvous(body, '/work/anvils')
    expect(parsed.url).toBe('http://127.0.0.1:8793')
    expect(parsed.pid).toBe(4242)
    expect(parsed.key).toBe('aaaaaaaaaaaa')
  })

  it('accepts the exact record shape that caused the outage', () => {
    // Byte-shaped like what the daemon actually published that day: the four
    // modeled fields plus `build`. Kept as a literal beside the derived test
    // on purpose — the derived one covers the future, this one covers the
    // specific record that killed a company, and a regression in either
    // direction should name itself.
    const published = {
      dir: '/work/anvils',
      key: 'aaaaaaaaaaaa',
      url: 'http://127.0.0.1:8793',
      pid: 4242,
      build: {
        exe: '/root/.chief/versions/0.5.0/bin/chiefd',
        identity: { dev: 24, ino: 193693, size: 41235968, mtimeS: 1756000000, mtimeNs: 123456789 }
      }
    }
    expect(() => parseDaemonRendezvous(published, '/work/anvils')).not.toThrow()
  })
})
