/**
 * #1031 — the daemon's account of a mid-suite failure, at the live-daemon
 * runtime where every sighting happened.
 *
 * This file used to PIN THE DEFECT. It asserted that the daemon log was deleted
 * unread, and said in as many words that fixing the diagnostic must come here
 * and change these expectations. That fix has landed
 * (`surfaceDaemonLogOnFailure`), so this now pins the behaviour that replaced
 * it. The prohibition it recorded was deliberately removed, which is the one
 * case where inverting a test is right — the file is rewritten, never deleted.
 *
 * What is unchanged: the log still lives inside the harness's temp data root and
 * `stop()` still removes it. That is fine, and it is exactly why the READ has to
 * happen before teardown. What changed is that something now reads it — an
 * `afterEach`, which vitest runs before `afterAll`, so the cause is surfaced
 * while it still exists.
 *
 * `ChiefdError::Corrupt { store }` still carries only the store name onto the
 * wire, so the daemon's stderr remains the ONLY place the broken invariant is
 * ever named. That is what makes this recoverability load-bearing rather than a
 * nicety.
 */
import { existsSync } from 'node:fs'

import { describe, expect, it } from 'vitest'

import { chiefdBinarySkipTitle, chiefdBinaryTestGate } from '@/ChiefdBinary'
import { startCompanyDaemon } from '@/CompanyDaemon'
import { readDaemonLogTail } from '@/DaemonLogOnFailure'

const SPAWN_DEADLOCK_TIMEOUT_MS = 20_000

const SUITE_LABEL = 'the daemon stderr that carries a corrupt-store cause (real binary)'
const gate = chiefdBinaryTestGate()
const maybeDescribe = gate.present ? describe : describe.skip

maybeDescribe(gate.present ? SUITE_LABEL : chiefdBinarySkipTitle(SUITE_LABEL, gate), () => {
  it(
    'is readable while the daemon runs, and only then — so the read must precede teardown',
    async () => {
      const daemon = await startCompanyDaemon({
        slug: 'issue-1031-corrupt-cause',
        readyTimeoutMs: 20_000
      })

      let stopped = false
      try {
        // The cause has exactly one home, and it is inside the disposable
        // company directory.
        expect(daemon.logPath.startsWith(daemon.dir)).toBe(true)
        expect(existsSync(daemon.logPath)).toBe(true)

        // The behaviour that replaced the defect: while the daemon is up, the
        // log is RECOVERABLE — this is the same call the failure hook makes, so
        // a mid-suite failure now surfaces whatever the daemon last said.
        const tail = await readDaemonLogTail(daemon.logPath, 80)
        expect(tail).not.toMatch(/^\(no daemon log at /)

        await daemon.stop()
        stopped = true

        // ...and teardown still takes it. Unchanged, and the whole reason the
        // read above has to happen first.
        expect(existsSync(daemon.logPath)).toBe(false)
        expect(existsSync(daemon.dir)).toBe(false)
        expect(await readDaemonLogTail(daemon.logPath, 80)).toMatch(/^\(no daemon log at /)
      } finally {
        if (!stopped) await daemon.stop()
      }
    },
    SPAWN_DEADLOCK_TIMEOUT_MS
  )
})
