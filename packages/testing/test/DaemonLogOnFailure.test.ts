/**
 * The daemon log tail that a failed test must surface before teardown deletes
 * it (#1031). `surfaceDaemonLogOnFailure` itself is a five-line `afterEach`
 * around these two — a suite cannot assert on its own failure path without
 * failing, so the readable and formattable halves are tested directly here.
 */
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { formatDaemonLogTail, readDaemonLogTail, tailLines } from '@/DaemonLogOnFailure'

describe('the daemon log tail a failed test surfaces', () => {
  let dir = ''

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'chief-testing-logtail-fixture-'))
  })

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true })
  })

  it('returns only the last N lines, keeping the newest', async () => {
    const logPath = join(dir, 'chiefd.log')
    const lines = Array.from({ length: 200 }, (_, index) => `line-${index}`)
    writeFileSync(logPath, lines.join('\n'))

    const tail = await readDaemonLogTail(logPath, 80)

    expect(tail.split('\n')).toHaveLength(80)
    expect(tail).toContain('line-199')
    expect(tail).toContain('line-120')
    // The whole point of a TAIL: the oldest lines are the ones dropped.
    expect(tail).not.toContain('line-119')
  })

  it('carries the invariant a corrupt-store cause would have written', async () => {
    const logPath = join(dir, 'chiefd.log')
    writeFileSync(
      logPath,
      ['starting', '[activity] store error: "Activity person \'x\' has a prior placement"'].join(
        '\n'
      )
    )

    expect(await readDaemonLogTail(logPath, 80)).toContain('[activity] store error:')
  })

  it('states the reason rather than throwing when there is no log', async () => {
    const missing = join(dir, 'never-written.log')

    expect(await readDaemonLogTail(missing, 80)).toBe(`(no daemon log at ${missing})`)
  })

  it('says the log is empty rather than printing a blank banner', async () => {
    const logPath = join(dir, 'empty.log')
    writeFileSync(logPath, '\n\n')

    expect(await readDaemonLogTail(logPath, 80)).toBe('(the daemon log is empty)')
  })

  it('names the failed test and the log path, because after teardown the path is the answer', () => {
    const banner = formatDaemonLogTail('transfers, benches, recalls', '/tmp/x/chiefd.log', 'BODY')

    expect(banner).toContain('transfers, benches, recalls')
    expect(banner).toContain('/tmp/x/chiefd.log')
    expect(banner).toContain('BODY')
    expect(banner).toContain('discarded when this suite tears down')
  })

  it('tails an in-memory capture the same way it tails a file', () => {
    const captured = Array.from({ length: 200 }, (_, index) => `line-${index}`).join('\n')

    const tail = tailLines(captured, 80)

    expect(tail.split('\n')).toHaveLength(80)
    expect(tail).toContain('line-199')
    expect(tail).not.toContain('line-119')
  })

  it('says an in-memory capture is empty rather than printing a blank banner', () => {
    expect(tailLines('', 80)).toBe('(the daemon log is empty)')
    expect(tailLines('\n\n', 80)).toBe('(the daemon log is empty)')
  })
})
