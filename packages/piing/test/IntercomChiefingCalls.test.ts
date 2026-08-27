import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { withoutComments } from '@test/support/TypeScriptSource'
import { describe, expect, test } from 'vitest'

// E4-S8 (#794): organization-intercom.ts's ~24 raw chiefd call sites are
// converted onto `@chief/chiefing/extension-runtime` -- some through
// `RowStoresClient`'s named typed methods (where one exists 1:1 for the
// route), the rest through the shared `postOrgRoute` + `FetchTransport`
// directly (manifest, the supervision/activity/session-maintenance
// aggregates, mailbox, person/transfer -- none of which
// RowStoresClient/DocsClient cover). This is a
// source-level regression proving both halves of that split stayed wired
// the way the PR description reports, without re-deriving 14k lines of
// control flow.

const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url))
const SOURCE = readFileSync(join(PACKAGE_ROOT, 'extensions/organization-intercom.ts'), 'utf8')
/** The same source with comments removed. The prose deliberately QUOTES the
 *  mechanisms it explains — including the ambient read this file's invariant
 *  bans — so a text ban has to be about code or it bans its own explanation. */
const SOURCE_CODE = withoutComments(SOURCE)

describe('IntercomChiefingCalls (E4-S8): route-family transport split', () => {
  test('imports the chiefing extension-runtime subpath, not a relative chiefing path', () => {
    expect(SOURCE).toContain('from "@chief/chiefing/extension-runtime";')
    expect(SOURCE).not.toMatch(/from\s+["'](\.\.\/)+packages\/chiefing/)
  })

  test('RowStoresClient covers event-journal, runtime, and the semantic insert', () => {
    // `.readCeoBootLease<` stood here. Its ONE call site was the roster's
    // CEO-only-projection arm, and both the method and the store are deleted
    // with the daemon-side CEO boot (chief-home-is-cwd §4c).
    // `.readHealthMonitor<` stood here too. Its ONE call site was the
    // health-incident ack's identity check, which proved an alert against the
    // recipient's runtime GENERATION; the generation is deleted, so the check
    // and its read went with it. No other intercom code reads that store.
    expect(SOURCE).toContain('.readEventOnceMarker<')
    expect(SOURCE).toContain('.insertEventOnceMarker(')
    expect(SOURCE).toContain('.readRuntime<')
    expect(SOURCE).toContain('.insertOperatorEscalationIntent(')
  })

  test('routes with no covering RowStoresClient/DocsClient method still call postOrgRoute directly at the documented chiefd path', () => {
    const directRoutes = [
      '/v1/org/manifest/read',
      '/v1/org/activity/read',
      '/v1/org/mailbox/read-person',
      '/v1/org/mailbox/delta',
      '/v1/org/person/transfer',
      // #751/P4: the two routes that were still a subprocess. `runtime/launch`
      // is the reconcile every mutating tool runs after its durable write, and
      // `lifecycle-status/read` is the control board.
      '/v1/org/runtime/launch',
      '/v1/org/lifecycle-status/read',
      // The durable-reminder family. These were the LAST recurrence mechanism
      // left after the Pi `/loop` addon was deleted, and all three were
      // reaching a CLI that now serves only `founder-pi`.
      '/v1/reminders/arm',
      '/v1/reminders/list',
      '/v1/reminders/stop',
      // #751/P4, the last four families: the count of subprocess call sites in
      // this file is now ZERO.
      // TOMBSTONE: `/v1/org/activity/command-status` was pinned here as "the
      // only surviving `org activity` verb". Its one reader asked whether a
      // park was already pending before compacting, and a routine idle park is
      // born terminal — so the answer was always no and the compact never ran.
      // The gate, the reader and the route literal are all deleted; the beat
      // below is what the family is now.
      '/v1/org/activity/agent-state',
      // The multi-unit resume, one all-or-nothing transaction.
      '/v1/org/department/resume-many',
      // The session-maintenance verbs. `queue` also serves `auto-compact`.
      //
      // TOMBSTONE: `/v1/org/session-maintenance/complete-native` and
      // `/v1/org/company-session-action/skip-parked` were pinned here. Both
      // routes are deleted with `org_maintain_session`, so the extension must
      // NOT reference either any more — the same shape as the resource-catalog
      // tombstone below.
      //
      // `/v1/org/fresh-session/{apply,complete}` STAY pinned and are a KNOWN
      // DEFECT rather than an endorsement: this guard asserts only that the
      // extension NAMES the path, and no chiefd route serves either — on
      // `origin/main` as well as here, so it is not this branch's regression.
      // Recorded rather than fixed; see the packet notes.
      '/v1/org/session-maintenance/queue',
      '/v1/org/session-maintenance/start',
      '/v1/org/session-maintenance/defer',
      '/v1/org/session-maintenance/interrupt',
      '/v1/org/session-maintenance/recover',
      '/v1/org/session-maintenance/finish',
      '/v1/org/fresh-session/apply',
      '/v1/org/fresh-session/complete'
      // TOMBSTONE (chief-home-is-cwd §3/§4e): `/v1/org/resource-catalog/read`
      // was pinned here as the hire preflight's installed Pi resource
      // inventory. The preflight and the route are both deleted — a hire names
      // no resource — so the extension must NOT reference it any more.
    ]
    for (const route of directRoutes) {
      expect(SOURCE, `expected organization-intercom.ts to still reference ${route}`).toContain(
        route
      )
    }
    // supervision/activity/session-maintenance share one templated route
    // (`/v1/org/${storeName}/read`) inside `chiefdReadNormalized`, rather than
    // three separate literals.
    expect(SOURCE).toContain('`/v1/org/${storeName}/read`')
  })

  test('chiefdAtomicPersonTransfer maps a decoded OrgRowRefusalError into the typed {refused, detail} value -- never a second parser', () => {
    expect(SOURCE).toContain(
      'if (error instanceof OrgRowRefusalError) return { refused: error.code, detail: error.detail };'
    )
  })

  test('a permanent fresh-session apply refusal is finished as failed with its exact code and detail', () => {
    expect(SOURCE_CODE).toContain('const failure = sessionMaintenanceFailure(retryError);')
    expect(SOURCE_CODE).toContain('status: failure.terminalStatus,')
    expect(SOURCE_CODE).toContain('refusalCode: failure.refusalCode')
    expect(SOURCE_CODE).toContain('refusalDetail: failure.refusalDetail')
    expect(SOURCE_CODE).toContain('event: "session-maintenance-fresh-session-apply-failed"')
  })

  // #751/P3: the eleven `runLifecycle` call sites moved onto chiefd's own API,
  // which deleted the helper outright. These assertions are the cheap half of
  // that claim — that each verb reaches a NAMED chiefd route, and that the
  // subprocess helper is gone rather than merely unused. The expensive half
  // (does chiefd accept the shape) lives in the Rust route tests and the live
  // exercise, and neither is replaceable by a source scan.
  test('every staffing and structure verb names its chiefd route', () => {
    for (const route of [
      '/v1/org/department/reparent',
      '/v1/org/department/move-members',
      '/v1/org/person/appoint-head',
      '/v1/org/person/replace-head-and-offboard',
      '/v1/org/person/hire-preview',
      '/v1/org/person/hire',
      '/v1/org/person/bench-lifecycle',
      '/v1/org/person/recall',
      '/v1/org/person/start',
      '/v1/org/person/shutdown',
      '/v1/org/staffing/lifecycle'
    ]) {
      expect(SOURCE, `expected organization-intercom.ts to post at ${route}`).toContain(route)
    }
  })

  test('the lifecycle subprocess helper is deleted, not merely unused', () => {
    // `runLifecycle` spawned `apps/cli/src/Main.ts` for all eleven verbs. A
    // helper left behind is a helper a later packet re-reaches for.
    expect(SOURCE).not.toMatch(/\brunLifecycle\s*\(/)
    // No ported verb translates placement into argv OR into a request field: a
    // client naming a tmux socket asserts authority over something it cannot
    // see, and every one of these routes wakes chiefd's own reconcile on its
    // way out. `runtimeIdentity` was the ONE reader of the injected socket and
    // session; its last caller was the multi-unit resume, which now posts to
    // chiefd directly, so the accessor is DELETED rather than left for a
    // future caller that must not exist. Zero, in both directions: the helper
    // is gone, and no other reader of the two fields took its place.
    expect(SOURCE.match(/runtimeIdentity\(/g) ?? []).toHaveLength(0)
    expect(SOURCE.match(/["']--socket["']/g) ?? []).toHaveLength(0)
    expect(SOURCE.match(/["']--session["']/g) ?? []).toHaveLength(0)
  })

  // #751/P4. The defect this pins was reported by a CEO, not by a test: an
  // `org_launch_department` that returned
  //   `org reconcile belfort-capital failed (exit 1): chiefd: unknown command 'org'`
  // AFTER `/v1/org/department/create` had already answered 200. Three packets
  // moved their own route and left this helper alone, so every one of them
  // committed its write and then failed the tool. The lesson is in the shape,
  // not the string: a route proof is not a tool proof.
  test('the reconcile every mutating tool runs is a chiefd call, not a CLI verb', () => {
    expect(SOURCE).not.toContain('"org", "reconcile"')
    expect(SOURCE).not.toContain('"--request-person"')
    expect(SOURCE).not.toContain('"org", "lifecycle-status"')
    // `requestedPersonIds` is a HINT chiefd evaluates per person against a real
    // pending envelope. `/v1/org/projection/reconcile` drops it on the floor,
    // so a department create would converge the company without opening the
    // launch fence for the people it had just created. Naming the wrong twin
    // here is silent — nothing else in the tree would fail.
    expect(SOURCE).toContain('requestedPersonIds')
    expect(SOURCE).not.toContain('"/v1/org/projection/reconcile"')
  })

  test('a bench and an offboard take the LIFECYCLE route, never the bare structural verb', () => {
    // The plain verb leaves the handoff fence up, and a departed person can
    // never complete the handoff that clears it -- their pane would stay open
    // forever. Naming the structural path here would be a silent regression
    // with no failing assertion anywhere else.
    expect(SOURCE).not.toContain('"/v1/org/person/bench"')
    expect(SOURCE).not.toContain('"/v1/org/person/offboard"')
  })

  test('a 2xx without applied:true is a THROW, never a guess (the staffing-lifecycle defect class)', () => {
    // `/v1/org/staffing/lifecycle` answered `{"status":"applied"}` with no
    // `applied` key, and four verbs committed their mutation and then reported
    // failure. This family's decoder refuses the same shape rather than
    // inventing a success.
    expect(SOURCE).toContain(
      'if (wire?.applied !== true) throw new Error(`chiefd docstore ${path} returned an invalid outcome`);'
    )
  })

  test('the two former bare /chiefd docstore/ degrade checks are now structural instanceof checks', () => {
    const occurrences =
      SOURCE.split('error instanceof ChiefdUnavailableError || error instanceof OrgRowRefusalError')
        .length - 1
    expect(occurrences).toBeGreaterThanOrEqual(2)
    expect(SOURCE).not.toContain('/chiefd docstore/.test(')
  })

  test('every chiefdPostJson definition is async and delegates to the shared postOrgRoute (no private decoder)', () => {
    // The chokepoint takes a `ChiefdEndpoint`, not a bare URL: the address AND
    // the credential that signs for it are per-install, so a host running
    // several companies in one process cannot have either resolved ambiently.
    expect(SOURCE).toContain(
      'async function chiefdPostJson<T>(endpoint: ChiefdEndpoint, path: string, body: unknown): Promise<T> {\n  return postOrgRoute<T>(chiefdTransport(endpoint), endpoint.url, path, body);\n}'
    )
  })

  test('no chiefd address in this file comes from an environment variable at all', () => {
    // THE MULTI-COMPANY INVARIANT, as source text.
    //
    // It used to allow exactly one read of `ORG_CHIEFD_URL` — in
    // `readOrganizationRuntimeContext`, where a pane's stamped-in address
    // entered the runtime context. That single read was the defect: one
    // variable per PROCESS cannot name the right daemon in a process hosting
    // several companies, and a wrong daemon ANSWERS rather than erroring. So
    // the name may not appear here in ANY form — a reintroduction is the defect
    // coming back.
    expect(SOURCE_CODE).not.toContain('ORG_CHIEFD_URL')
    expect(SOURCE_CODE).not.toContain('function durableStoreUrl(')
    // Non-vacuity: the replacement really is the ONE shared reader of the
    // directory's own rendezvous, not a private second implementation of
    // company discovery.
    expect(SOURCE_CODE).toContain('readDaemonRendezvous(context.organizationDir)')
  })

  test('the company key is READ off that rendezvous, never derived here', () => {
    // The composite `documentKey(slug, orgsRoot)` was rebuilt at a dozen call
    // sites in this file. A key built slightly differently does not fail
    // loudly — it matches no live company, so the route 404s and the write
    // silently never happens. There is one producer now and this file is a
    // reader, so neither the helper nor a private hash may appear.
    expect(SOURCE_CODE).not.toContain('documentKey(')
    expect(SOURCE_CODE).not.toMatch(/createHash\("sha256"\)[\s\S]{0,80}\.slice\(0, 12\)/)
    expect(SOURCE_CODE).toContain('const key = context.companyKey?.trim();')
  })

  test('the pre-park compaction makes no COMPANY-WIDE call, because it runs in every pane', () => {
    // `/v1/org/runtime/launch` starts every person the manifest wants up, so
    // `require_company_wide_authority` grants it only to the head of the root
    // department. `queueAutomaticParkCompaction` runs in EVERY person's pane,
    // and it used to open with `reconcileRuntime(context)` — which posts that
    // route. Every non-CEO caller was refused `403
    // caller-out-of-company-scope` and returned before requesting
    // `auto-compact`, so the >50%-context compact-before-park was dead
    // product-wide. Measured on a live box 2026-08-24: 590
    // `automatic-park-compaction-deferred` rows against 592 refused launches in
    // the daemon's own log, still firing at 93 a day.
    //
    // A SOURCE ASSERTION, deliberately. Driving this function needs a lifecycle
    // fence, a live lease, a context-usage probe and the scheduler that calls
    // it — a fixture an order of magnitude larger than the property, which is
    // simply "this function does not reach a route it can never be authorized
    // for". Same reasoning `docstore/org_slice.rs`'s handler-body assertions
    // record for their own case.
    const body = SOURCE_CODE.split('async function queueAutomaticParkCompaction')[1]
    expect(body, 'queueAutomaticParkCompaction is defined in this file').toBeDefined()
    const upToNextFunction = (body ?? '').split('\n}\n')[0] ?? ''
    expect(
      upToNextFunction,
      'the compaction must not nudge the company-wide reconcile: a leaf person may not start ' +
        'the whole company, and the refusal skips the compaction entirely'
    ).not.toContain('reconcileRuntime(')
    // BOTH SPELLINGS. The fix is the ABSENCE of the company-wide call, not the
    // absence of one helper's name: posting the route directly, or wrapping
    // either form in a try/catch so the 403 is swallowed, reintroduces exactly
    // the defect while leaving the assertion above green. A swallowed refusal
    // is the worse version — it keeps the functional loss and deletes the
    // evidence that was the only reason anybody found this.
    expect(upToNextFunction).not.toContain('/v1/org/runtime/launch')
    // NON-VACUITY, both halves: the body really was captured, and the work it
    // exists to request is still requested.
    expect(upToNextFunction).toContain('hasOpenOrganizationWork(context)')
    expect(upToNextFunction).toContain('"auto-compact"')
  })

  test('the automatic compaction asks NOTHING about a pending park, because a routine park is born terminal', () => {
    // THE GATE HAD NO YES. This function used to read
    // `/v1/org/activity/command-status` and require a `park` in
    // `pendingTransitions` before compacting. `begin_transition` mints a
    // routine idle park `TransitionStatus::Forced` with `handoff_deadline_at`
    // at the admission instant — "A ROUTINE IDLE PARK IS BORN TERMINAL...
    // `is_pending()` is false" — and `pending_transitions` returns only
    // `AwaitingHandoff | Overdue`. So the list could never contain the thing
    // the gate required.
    //
    // Box evidence, a live box 2026-08-24: every all-time decline is
    // `no-pending-park` or `usage-low`, and the automatic compact's reason
    // string appears ZERO times in the entire bus. The feature never fired
    // once. All 28 historical compactions were tool-driven.
    //
    // The trigger is SETTLE now, and that is a positive property rather than a
    // fallback: a routine park cannot be admitted until the settle lease
    // expires, so a compact started here owns the whole lease to finish in.
    const body = SOURCE_CODE.split('async function queueAutomaticParkCompaction')[1]
    const upToNextFunction = (body ?? '').split('\n}\n')[0] ?? ''
    expect(upToNextFunction, 'the function body was captured').toContain('"auto-compact"')
    // THE ROUTE, THE READER AND THE REASON, each named separately. The gate can
    // come back as a re-read of the same route, as a fresh `pendingTransitions`
    // filter over some other source, or as a resurrected reason string in a
    // decline that reads like instrumentation — so no one of these three
    // absences is the assertion on its own.
    expect(upToNextFunction).not.toContain('command-status')
    expect(upToNextFunction).not.toContain('pendingTransitions')
    expect(upToNextFunction).not.toContain('no-pending-park')
    // And the client stack is DELETED rather than left unused, so a later
    // packet cannot reach for a helper that is still sitting there. The route
    // survives in chiefd for the CLI; this file must not name it.
    expect(SOURCE_CODE).not.toContain('activityCommand(')
    expect(SOURCE_CODE).not.toContain('/v1/org/activity/command-status')
    // NON-VACUITY for the deletions above: the ACTIVITY family is still here,
    // still reached by literal, and still down to exactly one verb.
    expect(SOURCE_CODE).toContain('/v1/org/activity/agent-state')
  })

  test('the surviving compaction gates are the ones that can answer, and all four still report their decline', () => {
    // Sanchez's ruling, adopted: deleting the gate that could not open must not
    // quieten the gates that can. Each of these can genuinely refuse a
    // compaction, and #1230's one-row-per-state decline trail is what turned a
    // silent dead feature into a diagnosis — so it stays, minus the arm whose
    // branch no longer exists.
    const body = SOURCE_CODE.split('async function queueAutomaticParkCompaction')[1]
    const upToNextFunction = (body ?? '').split('\n}\n')[0] ?? ''
    for (const reason of [
      'no-extension-context',
      'fence-stale',
      'open-work',
      'usage-unavailable',
      'usage-low',
      'fence-stale-at-request'
    ]) {
      expect(
        upToNextFunction,
        `expected the compaction to still report a '${reason}' decline`
      ).toContain(`decline("${reason}")`)
    }
    // The usage floor is the one gate that makes this safe to fire on every
    // qualifying settle: a completed compaction drops usage under it, so the
    // pass is SELF-QUIETING and does not re-fire until the context grows back.
    expect(upToNextFunction).toContain('<= 50) return decline("usage-low")')
  })
})

// TOMBSTONE: `a Pi that cannot replace sessions is refused honestly, not
// retried forever` — the five arms that pinned #1244's capability gate.
//
// They were right for one day. The operator then ruled the whole feature out,
// so there is no `fresh_session` to refuse, no capability to probe, and no
// unfinishable row to terminalize. A gate whose subject is deleted is not a
// property; it is an instrument pointed at nothing.
