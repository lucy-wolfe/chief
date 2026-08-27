/**
 * A4 part 3: the silent pane-side key refusal.
 *
 * `readAgentKeypair` refuses a group- or world-readable identity key and, until
 * this packet, had NO WAY TO REPORT IT. Its module is copied FLAT into every
 * pi-home, imports nothing but `node:*`, and cannot use `console` (banned by
 * lint), so the refusal was silent. Since A1 made a bad key mode a hard refusal
 * on the daemon side too, the effect was a pane that simply stopped working
 * with nothing anywhere saying why — a strict rule whose only signal is silence
 * is a support incident waiting to happen.
 *
 * The reporting path is `organization-intercom.ts`, which can write and already
 * owns the two channels every other in-pane failure uses together: the durable
 * `.chief/bus/events.jsonl` trail and the `.chief/logs/exceptions.jsonl`
 * diagnostic.
 */
import { existsSync, mkdirSync, mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { isNullish } from '@test/support/Nullish'
import { describe, expect, test } from 'vitest'

import { reportPaneKeyRefusal } from '../../extensions/organization-intercom'

function company(): string {
  // A company IS a directory, so the fixture mints one rather than a slug
  // under a shared orgs root.
  const organizationDir = mkdtempSync(join(tmpdir(), 'piing-key-refusal-'))
  mkdirSync(organizationDir, { recursive: true })
  return organizationDir
}

/** A parsed bus line, narrowed without an assertion. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

function busLines(organizationDir: string): string[] {
  const path = join(organizationDir, '.chief', 'bus', 'events.jsonl')
  return readFileSync(path, 'utf8').split('\n').filter(Boolean)
}

function exceptionLines(organizationDir: string): string[] {
  const path = join(organizationDir, '.chief', 'logs', 'exceptions.jsonl')
  return readFileSync(path, 'utf8').split('\n').filter(Boolean)
}

describe('a refused identity key is reported, not swallowed', () => {
  test('a permissive mode reaches BOTH failure channels, naming the file, the mode and the fix', () => {
    const organizationDir = company()
    const keyPath = join(organizationDir, '.chief', 'agent', 'ceo', 'chiefd-identity.key.pem')
    const identityDir = join(organizationDir, '.chief', 'agent', 'ceo')

    reportPaneKeyRefusal(
      { reason: 'permissive-mode', keyPath, mode: 0o644 },
      { url: 'http://chiefd.test', personId: 'ceo', organizationDir, identityDir }
    )

    const bus = busLines(organizationDir)
    expect(bus).toHaveLength(1)
    expect(bus[0]).toContain('"event":"identity-key-refused"')
    expect(bus[0]).toContain('"reason":"permissive-mode"')
    // The exact bits, not "bad mode": a report an operator cannot act on is
    // barely better than the silence it replaced.
    expect(bus[0]).toContain('"mode":"0644"')
    expect(bus[0]).toContain(keyPath)
    expect(bus[0]).toContain('chmod 600')

    const exceptions = exceptionLines(organizationDir)
    expect(exceptions).toHaveLength(1)
    expect(exceptions[0]).toContain('identity-key-refused')
    expect(exceptions[0]).toContain('chmod 600')
  })

  test('new, start and attach pane diagnostics create no project-root runtime directories', () => {
    const organizationDir = company()
    const keyPath = join(organizationDir, '.chief', 'agent', 'ops', 'chiefd-identity.key.pem')

    // These entry paths all stamp the same company directory. The extension
    // creates both real diagnostic streams from that stamp.
    reportPaneKeyRefusal(
      { reason: 'absent', keyPath },
      {
        url: 'http://chiefd.test',
        personId: 'ops',
        organizationDir,
        identityDir: join(organizationDir, '.chief', 'agent', 'ops')
      }
    )

    expect(existsSync(join(organizationDir, 'bus'))).toBe(false)
    expect(existsSync(join(organizationDir, 'logs'))).toBe(false)
    expect(existsSync(join(organizationDir, '.chief', 'bus', 'events.jsonl'))).toBe(true)
    expect(existsSync(join(organizationDir, '.chief', 'logs', 'exceptions.jsonl'))).toBe(true)
  })

  test('the same refusal is reported ONCE — the key is re-read on every request', () => {
    const organizationDir = company()
    const keyPath = join(organizationDir, '.chief', 'agent', 'coo', 'chiefd-identity.key.pem')
    const refusal = { reason: 'permissive-mode', keyPath, mode: 0o640 } as const
    const identityDir = join(organizationDir, '.chief', 'agent', 'coo')
    const identity = { url: 'http://chiefd.test', personId: 'coo', organizationDir, identityDir }

    reportPaneKeyRefusal(refusal, identity)
    reportPaneKeyRefusal(refusal, identity)
    reportPaneKeyRefusal(refusal, identity)

    expect(busLines(organizationDir)).toHaveLength(1)
  })

  test('an ABSENT key is NOT reported when nothing about the pane claims a person', () => {
    const organizationDir = company()
    const keyPath = join(organizationDir, '.chief', 'chiefd-identity.key.pem')

    reportPaneKeyRefusal(
      { reason: 'absent', keyPath },
      // NO person id. A pane that is not running as somebody legitimately has
      // no identity key, and reporting here would be noise on every ordinary
      // pane.
      {
        url: 'http://chiefd.test',
        personId: '',
        organizationDir,
        identityDir: join(organizationDir, '.chief')
      }
    )

    // Not merely "no line" — no trail is created at all, so a healthy pane
    // that has never had a key does not grow a file that looks like a fault.
    expect(() => busLines(organizationDir)).toThrow()
  })

  test('an ABSENT key IS reported when the pane is running AS a person', () => {
    const organizationDir = company()
    const keyPath = join(organizationDir, '.chief', 'agent', 'cto', 'chiefd-identity.key.pem')
    const identityDir = join(organizationDir, '.chief', 'agent', 'cto')

    reportPaneKeyRefusal(
      { reason: 'absent', keyPath },
      { url: 'http://chiefd.test', personId: 'cto', organizationDir, identityDir }
    )

    // A pane launched from a person's home, whose every org route is fenced,
    // cannot legitimately lack that person's key: it is broken before it does
    // anything. This silence is what hid a real defect for the length of this
    // branch — `paneHomeDirectory` looked one segment too shallow, every
    // person-pane found no key, and the only symptom was `missing bearer
    // token` on every call, a whole segment from the cause.
    const lines = busLines(organizationDir)
    const reported = lines
      .map((line): unknown => JSON.parse(line))
      .filter(isRecord)
      .find((line) => line.event === 'identity-key-refused')
    // The whole trail in the message, joined rather than re-serialized: this
    // file's own writer emits one JSON object per line, so the raw lines ARE
    // the readable form.
    expect(reported, `no identity-key-refused line in:\n${lines.join('\n')}`).toBeTruthy()
    expect(reported?.reason).toBe('absent')
    expect(reported?.personId).toBe('cto')
    // The PATH is the diagnosis for this reason — it is what turns "no key"
    // into "you are looking in the wrong place".
    expect(reported?.keyPath).toBe(keyPath)
  })

  test('an unreadable key is reported too — a key that cannot be loaded is a fault, not a state', () => {
    const organizationDir = company()
    const keyPath = join(organizationDir, '.chief', 'agent', 'cfo', 'chiefd-identity.key.pem')
    const identityDir = join(organizationDir, '.chief', 'agent', 'cfo')

    reportPaneKeyRefusal(
      { reason: 'unreadable', keyPath },
      { url: 'http://chiefd.test', personId: 'cfo', organizationDir, identityDir }
    )

    const bus = busLines(organizationDir)
    expect(bus).toHaveLength(1)
    expect(bus[0]).toContain('"reason":"unreadable"')
    expect(bus[0]).toContain('"mode":"unknown"')
  })
})
