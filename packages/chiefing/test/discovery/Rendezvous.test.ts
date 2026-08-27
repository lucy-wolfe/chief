import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import {
  parseDaemonRendezvous,
  readDaemonRendezvous,
  RENDEZVOUS_FILENAME,
  rendezvousPath
} from '@/discovery/Rendezvous'

const KEY = '0123456789ab'

function sample(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    dir: '/work/anvils',
    key: KEY,
    url: 'http://127.0.0.1:8793',
    pid: 4242,
    ...overrides
  }
}

describe('rendezvousPath', () => {
  /** Byte-identical to `host_primitives::rendezvous::rendezvous_path`. Two
   * programs read this file and neither may spell the path its own way. */
  it('is the directory own disposable run folder', () => {
    expect(rendezvousPath('/work/anvils')).toBe('/work/anvils/.chief/run/daemon.json')
    expect(RENDEZVOUS_FILENAME).toBe('daemon.json')
  })
})

describe('parseDaemonRendezvous', () => {
  it('accepts the shape both programs decode', () => {
    expect(parseDaemonRendezvous(sample(), '/work/anvils')).toEqual({
      dir: '/work/anvils',
      key: KEY,
      url: 'http://127.0.0.1:8793',
      pid: 4242
    })
  })

  /**
   * THE COPIED-FILE CASE, which is the one a bare "read the url" would miss.
   *
   * `.chief/` lives INSIDE the company directory, so copying a project copies
   * its rendezvous — and the copy still names the ORIGINAL directory. Binding
   * it would point the new directory's pane at the old directory's daemon,
   * which answers, commits, and returns 200: the exact silent split-brain the
   * composite key existed to prevent.
   */
  it('refuses a rendezvous that describes another directory', () => {
    expect(() => parseDaemonRendezvous(sample(), '/work/anvils-copy')).toThrow(
      /describes "\/work\/anvils", not "\/work\/anvils-copy"/
    )
    expect(() => parseDaemonRendezvous(sample(), '/work')).toThrow()
  })

  /**
   * THE DIRECTION THIS OUTAGE REVERSED, pinned so it cannot come back.
   *
   * This test used to assert the OPPOSITE — that an unmodeled field is a
   * refusal — and that assertion is what killed every person in a live company
   * on 2026-08-26. The daemon began publishing an additive `build` field; this
   * reader refused the whole record; every pane died at start-up with
   * `unknown field "build" — this daemon is a different build`, naming a skew
   * that was not happening.
   *
   * A reader of somebody else's record is forward compatible or it is a
   * scheduled outage. The fields this parser USES are still checked, every
   * one, by the tests around this one.
   */
  it('accepts a rendezvous carrying a field it does not know', () => {
    const withFuture = parseDaemonRendezvous(
      sample({ build: { exe: '/root/.chief/versions/0.5.0/bin/chiefd', identity: { ino: 7 } } }),
      '/work/anvils'
    )
    expect(withFuture.url).toBe(sample().url)
    expect(withFuture.pid).toBe(sample().pid)
    expect('build' in withFuture).toBe(false)

    // The concrete historical case this file used to refuse, now accepted for
    // the same reason: a daemon from a build that keyed companies differently
    // still tells this reader everything it needs.
    expect(() =>
      parseDaemonRendezvous(sample({ orgsRoot: '/home/op/.chiefd/orgs' }), '/work/anvils')
    ).not.toThrow()
  })

  /** AND THE FIELDS IT USES ARE STILL A CONTRACT. Forward compatibility is
   * about fields nobody modeled, never about the four this reader depends on:
   * a rendezvous missing one of those is still unusable and still refused. */
  it('still refuses a rendezvous missing a field it depends on', () => {
    for (const absent of ['dir', 'key', 'url', 'pid']) {
      const body = { ...sample() }
      delete body[absent]
      expect(() => parseDaemonRendezvous(body, '/work/anvils'), absent).toThrow(
        /malformed daemon rendezvous/
      )
    }
  })

  it('refuses a key that is not twelve lowercase hex characters', () => {
    for (const bad of ['', 'anvils', '0123456789a', '0123456789AB']) {
      expect(() => parseDaemonRendezvous(sample({ key: bad }), '/work/anvils'), bad).toThrow()
    }
  })

  it('refuses a pid that is not a positive integer', () => {
    for (const bad of [0, -1, 1.5, '4242']) {
      expect(() => parseDaemonRendezvous(sample({ pid: bad }), '/work/anvils')).toThrow()
    }
  })

  it('refuses an empty url and a non-object', () => {
    expect(() => parseDaemonRendezvous(sample({ url: '' }), '/work/anvils')).toThrow()
    expect(() => parseDaemonRendezvous('daemon', '/work/anvils')).toThrow()
    expect(() => parseDaemonRendezvous(null, '/work/anvils')).toThrow()
  })
})

describe('readDaemonRendezvous', () => {
  let dir: string

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'chiefing-rendezvous-'))
  })

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true })
  })

  function publish(body: unknown): void {
    mkdirSync(join(dir, '.chief', 'run'), { recursive: true })
    /* eslint-disable lucy/no-json-stringify */
    // Test-only fixture body; @tribes-terminal/foundation is not a dependency here.
    writeFileSync(rendezvousPath(dir), JSON.stringify(body))
    /* eslint-enable lucy/no-json-stringify */
  }

  /** A pane learns its daemon's url AND its own company key from ONE local
   * read. No registry is on the path between a command and its own company. */
  it('reads the url and the company key with no network call at all', () => {
    publish({ dir, key: KEY, url: 'http://127.0.0.1:8793', pid: 4242 })
    expect(readDaemonRendezvous(dir)).toEqual({
      dir,
      key: KEY,
      url: 'http://127.0.0.1:8793',
      pid: 4242
    })
  })

  /** An ABSENT file is "not started", not a fault: a company that has never
   * booted, or one whose disposable `run/` folder was deleted, legitimately
   * has none and the caller's answer is "boot it". */
  it('resolves undefined when no daemon has published', () => {
    expect(readDaemonRendezvous(dir)).toBeUndefined()
  })

  /** A file that IS there but does not decode is a daemon disagreeing with its
   * own client — no caller recovers from that by guessing, so it throws rather
   * than reading as "not started". */
  it('throws on a present but undecodable file rather than reading it as absent', () => {
    mkdirSync(join(dir, '.chief', 'run'), { recursive: true })
    writeFileSync(rendezvousPath(dir), 'not json')
    expect(() => readDaemonRendezvous(dir)).toThrow(/not JSON/)

    publish({ dir: '/somewhere/else', key: KEY, url: 'http://127.0.0.1:1', pid: 1 })
    expect(() => readDaemonRendezvous(dir)).toThrow(/describes/)
  })
})
