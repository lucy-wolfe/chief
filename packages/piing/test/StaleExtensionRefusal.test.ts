/**
 * A PANE MUST NOT ANSWER FOR CODE IT IS NO LONGER RUNNING.
 *
 * THE LIVE INCIDENT THIS LOCKS (2026-07-24). An operator was told twice the
 * model switch was fixed, then got the receipt `applies at the next claim cycle
 * · no settled-work wait` — a string that existed nowhere in the deployed code.
 * The CEO pane's installed `organization-intercom` WAS the fixed file, written
 * 16:36:04; the pi process that loaded it started 15:48:21. The module
 * answering him had been replaced on disk 48 minutes after it was loaded. The
 * receipt was confident and **nothing durable was written behind it**.
 *
 * Pi loads extensions once at session start, and a deploy only rewrites files.
 * So an already-running pane keeps executing the module it loaded, and — before
 * this guard — said nothing about it. **Silence is the defect.** A wrong answer
 * fails; an unreached fix is silent forever.
 *
 * WHY A NEW GUARD RATHER THAN THE EXISTING DETECTOR. Detection already existed
 * and is correct: `organizationRuntimeExtensionDrift` compares each live pane's
 * `/proc/<pid>` mtime against its materialized extension mtimes, and its own
 * header records a PRIOR occurrence of this same bug. What it cannot do is stop
 * a stale pane mid-conversation — it is an on-demand CLI report plus ONE
 * reconcile call site that USED to sit inside `if (!options.materializationReady)`,
 * false on the ordinary post-deploy path, so the check was skipped exactly when
 * a deploy just happened. (A6 deleted that field and the conditional with it;
 * the reconcile is unconditional now, and this suite is about an ALREADY
 * RUNNING pane either way.) Detection existed; **nothing consulted
 * it at the moment that lied.** This suite locks the consult, at the queue
 * chokepoint, before any durable write.
 *
 * ⚠️ SCOPE — READ THIS BEFORE TRUSTING THIS FILE. What is locked here is the
 * staleness DECISION and the REFUSAL it produces, including the fail-open
 * behaviour and the exact wording an operator has to act on. What is NOT yet
 * locked is the end-to-end path: "a stale pane's real `org_maintain_session`
 * call queues nothing".
 *
 * I wrote that end-to-end test, and it PASSED — and then failed its own
 * negative control. With the guard deleted from `sessionMaintenanceCommand` the
 * suite still went 7/7 green, which means the scripted tool call was never
 * reaching the queue and "no request was written" was trivially true. A probe
 * confirmed it directly: driving `org_maintain_session` from an in-process
 * `world.person()` session produces NO `session-maintenance` document at all —
 * `SESSION-MAINTENANCE DOC PRESENT: false` — with the guard already removed. So
 * that test asserted nothing and has been DELETED rather than left to read as
 * proof. This is the exact failure mode the guard exists to prevent, which is
 * why it is written down instead of quietly dropped.
 *
 * THE HARNESS GAP THAT BLOCKS IT: the in-process manager driver's tool call does
 * not reach the pane-authenticated launcher CLI queue path. Until that is
 * closed, the end-to-end refusal is UNPROVEN — the guard is believed correct and
 * is unit-locked below, but nobody should record it as verified end to end.
 *
 * The two exposures that look identical in a table are separated by
 * construction, because the comparison is per-process ("the mtime *I* loaded"
 * vs "the mtime on disk *now*"): a long-lived pane whose extensions were
 * reinstalled is stale; the launcher CLI (fresh process per invocation) and a
 * Rust/chiefd-only deploy (which reinstalls no extensions) are not. The
 * rollback and fail-open cases below pin that a guard cannot fire on healthy
 * code — a guard that cries wolf gets switched off, and then it protects nobody.
 */
import {
  assertRunningExtensionIsCurrent,
  extensionStalenessOf,
  runningExtensionStaleness
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

describe('staleness is DETECTED and SURFACED — a pane never answers for replaced code', () => {
  // ── The decision itself, proven without a disk ────────────────────────────
  describe('the staleness decision', () => {
    test('disk newer than the loaded stamp is STALE; equal or older is not', () => {
      expect(extensionStalenessOf(1_000, 2_000).stale).toBe(true)
      expect(extensionStalenessOf(1_000, 1_000).stale).toBe(false)
      // A file REPLACED BY AN OLDER COPY (a rollback) is not "stale code
      // answering for new code" — it is the pane running what is on disk.
      expect(extensionStalenessOf(2_000, 1_000).stale).toBe(false)
    })

    test('FAILS OPEN on an unreadable stamp — a guard that blocks real work on a bad read gets switched off', () => {
      expect(extensionStalenessOf(undefined, 2_000).stale).toBe(false)
      expect(extensionStalenessOf(1_000, undefined).stale).toBe(false)
      expect(extensionStalenessOf(undefined, undefined).stale).toBe(false)
      // …and the assertion built on it must not throw in that state.
      expect(() => assertRunningExtensionIsCurrent({ stale: false })).not.toThrow()
    })

    test('the refusal names BOTH timestamps and states that nothing was queued', () => {
      const loaded = Date.parse('2026-07-24T15:48:21.000Z')
      const installed = Date.parse('2026-07-24T16:36:04.000Z')
      let message = ''
      try {
        assertRunningExtensionIsCurrent({
          stale: true,
          loadedMtimeMs: loaded,
          currentMtimeMs: installed
        })
      } catch (error) {
        message = error instanceof Error ? error.message : String(error)
      }
      // The real incident's two timestamps, so the operator can check the claim.
      expect(message).toContain('2026-07-24T15:48:21.000Z')
      expect(message).toContain('2026-07-24T16:36:04.000Z')
      expect(message).toContain('Nothing was queued')
      // WHOSE session, shouted. The first wording said "This pane ... Restart
      // this person", and both phrases are read against the REQUEST — which
      // names somebody else. A CEO setting `maya-head`'s thinking effort read
      // it as "restart maya-head", restarted her, got the identical refusal and
      // looped. The staleness is a property of the CALLER's own loaded module;
      // this check cannot see the target's session at all.
      expect(message).toContain('YOUR OWN session')
      expect(message).toContain('NOT ABOUT THE PERSON YOU NAMED')
      // The remedy moved with the tool. It said "Restart YOURSELF —
      // `org_maintain_session` with `fresh_session` on your own id", and that
      // verb is deleted: an agent cannot restart itself with a clean context at
      // all now. The sentence names what IS available — asking the operator to
      // stop and start the pane — and the property under test is unchanged,
      // that the refusal is about the CALLER and not the person they named.
      expect(message).toContain('Your OWN pane has to be restarted')
      expect(message).toContain('stop and start it')
      expect(message, 'the old ambiguous phrasing must not come back').not.toContain(
        'Restart this person'
      )
      expect(message, 'and it must not name the deleted tool').not.toContain('org_maintain_session')
    })

    test('NEGATIVE CONTROL: this very test process is NOT stale — the guard is silent on healthy code', () => {
      // Reads the real file this module was loaded from. If this ever throws,
      // the guard would be firing on every healthy pane in the fleet.
      expect(runningExtensionStaleness().stale).toBe(false)
      expect(() => assertRunningExtensionIsCurrent()).not.toThrow()
    })
  })
})
