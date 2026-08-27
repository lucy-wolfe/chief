/**
 * REACTIVE_ALLOWLIST — the closed triage register for reactive-scan.ts.
 *
 * #827 (E8-S5) deletes the last poll floors (SSE consumers re-reading chiefd
 * on a 60s timer "just in case") and the two remaining agent-thread parks
 * (`Atomics.wait`/`spawnSync` blocking the extension's own JS thread). SSE is
 * the change channel: a doc-change event, a `reorg` (cursor predates the
 * retained ring — a resync trigger, NOT an unhealthy-channel signal, #296),
 * and the watcher's own heartbeat-timeout/reconnect-backoff already answer
 * "did I miss something", "is the channel alive", and "how do I get back".
 * A recurring timer or a self-rescheduling wait answering the same questions
 * a second time is pure duplication with a real cost (one full re-read per
 * consumer per minute per pane, forever) and is banned.
 *
 * This is the TypeScript sibling of chiefd's `clippy.toml`
 * `disallowed-methods` and follows the repo's established
 * scan-plus-prose-justified-allowlist shape (see
 * scripts/orphanable-spawner-lib.mjs).
 *
 * THE REGISTER IS A CLOSED LIST, NOT A GROWTH SURFACE. An entry may be added
 * only with a reason that names one of the five allowed classes and explains
 * why the wait is not change detection:
 *
 *   deadline           — a bounded timeout enforcing a real deadline (a stuck
 *                         operation must eventually be declared failed), not
 *                         a re-read of state that could instead be pushed.
 *   render-clock        — zero-I/O UI tick (ages a frozen snapshot into a
 *                         visible staleness marker, or dismisses a flash) —
 *                         a correctness/UX surface, never a data sample.
 *   external-protocol   — the wait IS the remote protocol's own mechanism
 *                         (a long-poll interval, a required repeated
 *                         keepalive action) — there is no push alternative
 *                         because the story lives entirely on the other
 *                         side's API contract.
 *   os-liveness         — waiting on process/OS state with no push channel
 *                         (e.g. "did the process I just forked bind its
 *                         port yet"), always bounded by an explicit
 *                         start/stop deadline.
 *   bounded-retry        — a SINGLE, bounded fallback attempt guarding
 *                         against a missed event wedging something forever;
 *                         not a resubscription loop, not unbounded.
 *
 * "Temporarily, until X lands" is explicitly NOT a valid reason (D0 forbids
 * the poll-only mode that reason would excuse). If a component allowlisted
 * here is scheduled to disappear, the correct move is to block landing this
 * change on that component's replacement, not to allowlist a stopgap that
 * quietly becomes permanent — this is why the provider-credential watcher
 * (deleted outright by E8-S3) does NOT get a row here.
 *
 * #966/#967 — ANCHOR CHANGED FROM `(file, primitive)` TO `(file, primitive,
 * match)`, the same content-anchor shape `apps/cli/test/BlockingAllowlist.ts`
 * (#883) and `scripts/test/sql-only-state.test.mjs` already use. The old
 * coarse key meant a pure file move (zero code change) could silently orphan
 * an entry while leaving a real, unregistered site behind — demonstrated
 * live by #963, which moved two `Atomics.wait` sites verbatim out of this
 * file's org-durable-store.ts entry — and, worse, meant ANY file with two
 * real sites of the same primitive had them silently share one entry's
 * justification whether or not that justification actually covered both.
 * #967 is the concrete, already-live instance: this file's LOCK_SLEEPER
 * `Atomics.wait` site was never deliberately triaged on its own terms — it
 * just happened to fall under the fetch-worker's registration because both
 * shared the same (file, primitive) key. It has its own entry below now.
 *
 * `match` is the blessed line's exact TRIMMED source text (leading/trailing
 * whitespace only — internal spacing must match verbatim), matched by
 * `scripts/reactive-scan.ts`'s `scan()` using BAG (multiset) semantics, not
 * set semantics: two real sites sharing byte-identical text in the same file
 * each require their own allowlist entry (duplicate rows, same text) — a
 * set-based "has this text been blessed at all" check would let one entry
 * silently cover an unreviewed duplicate, the same failure mode this whole
 * story exists to close. Line numbers are still deliberately NOT stored —
 * they drift; text moving within a file does not change its bytes.
 */
export interface ReactiveAllowance {
  /** Repo-relative path. */
  file: string;
  /** The primitive being allowed. */
  primitive:
    | "setInterval"
    | "setTimeout-self-rescheduling"
    | "Atomics.wait"
    | "Bun.sleepSync"
    | "spawnSync"
    | "sleep";
  /** The blessed line's exact source text, trimmed. THE anchor (#966/#967) —
   *  matched against (file, primitive, match) as a bag, independent of line
   *  number and independent of any other site sharing this file+primitive. */
  match: string;
  /** WHY this is not a poll. Must name the class: deadline | render-clock |
   *  external-protocol | os-liveness | bounded-retry. */
  reason: string;
}

export const REACTIVE_ALLOWLIST: readonly ReactiveAllowance[] = [
  // ===========================================================================
  // RENDER-CLOCK — zero-I/O UI ticks. Ages a frozen snapshot into a visible
  // staleness marker, or dismisses a UI flash. Never a data sample.
  // ===========================================================================
  {
    file: "packages/piing/extensions/team-ui.ts",
    primitive: "setInterval",
    match: "const createFloorTimer = options.createFloorTimer ?? ((fn: () => void, ms: number) => setInterval(fn, ms));",
    reason:
      "render-clock: the footer's render tick. Zero I/O — it calls tui.requestRender() only. Its purpose after #827 is to age a frozen snapshot into a visible '⚠ stale' state once FOOTER_STALE_AFTER_MS has elapsed since the last doc-change; that is a correctness/UX surface, not a change-detection sample. All actual reads happen on doc-change / reorg / dead events via subscribeSse, never on this tick.",
  },
  // team-ui.ts's fire-flash auto-clear and organization-activity-status.ts's
  // status-flash dismissal are ONE-SHOT setTimeout calls with no recursion —
  // not "self-rescheduling" (their callbacks never call setTimeout again).
  // The scan's detector (scripts/reactive-scan.ts) only flags a setTimeout
  // whose own callback nests another setTimeout call, matching the register
  // interface's five primitive names exactly (there is no plain "setTimeout"
  // primitive — a one-shot timer that never recurs is not a poll and needs
  // no registration). No allowlist row for either: an entry with no matching
  // scanned site would itself fail the scan's stale-allowlist check.

  {
    file: "packages/piing/extensions/founder-launch.ts",
    primitive: "setInterval",
    match: "const tick = setInterval(report, PROGRESS_TICK_MS);",
    reason:
      "render-clock: the launch progress line's tick. Zero I/O — the callback calls Pi's onUpdate with a string it composes from two values already in hand (the current phase name and Date.now() - startedAt) and asks chiefd nothing. It samples no state and cannot observe a phase change: phases arrive by push, on the SSE stream the launch is already reading, and each one calls the same report() directly. #1051 is precisely why it exists: a company launch measured at 4m34s (140.6s of it before chiefd was contacted at all) showed one static row, and a line that only moves when a phase changes is frozen for exactly as long as the wait a human is trying to read. The number it advances is in seconds, hence the 1s interval. It is unref'd where the runtime supports it and cleared in a finally on every exit path, so it can never outlive the launch it narrates.",
  },

  // ===========================================================================
  // EXTERNAL-PROTOCOL — the wait IS the remote protocol's own mechanism.
  // ===========================================================================
  {
    file: "apps/web/src/server/PersonStream.ts",
    primitive: "setInterval",
    match: "heartbeat = setInterval(() => controller.enqueue(encoder.encode(': beat\\n\\n')), HEARTBEAT_MS)",
    reason:
      "external-protocol: the SSE comment frame is SSE's own keepalive mechanism, and the bytes are not optional — SseClientService fails a connection silent for 45s and reconnects with backoff, so a person stream carrying only real events would tear down and re-subscribe every 45 seconds while an agent thinks. Nothing distinguishes a dead socket from a thinking agent except traffic. The callback writes one CONSTANT comment (': beat'), reads no state and asks no question; PersonStream.test.ts pins that exact payload, so this site cannot become a sample without failing that test. The direction is also outward — bytes this server sends TO a browser, never a read FROM chiefd.",
  },

  // ===========================================================================
  // OS-LIVENESS — organization-intercom.ts's authoritativeRuntimePane.
  // Ruled by architect2 (#827 Question A, reversing an earlier "convert to
  // async" instruction after evidence surfaced): this spawnSync is the third
  // fallback in a `||` chain (rawPane || preservedPane ||
  // authoritativeRuntimePane(...), :1405-1407) — it executes ONLY when both
  // TMUX_PANE and ORG_LAUNCHER_PANE_ID are absent, a degraded-launch
  // recovery path, not the normal boot path.
  //
  // NOTE — the ruling's original SECOND argument (converting it would force
  // installOrganizationIntercom async, introducing a NEW registration-
  // ordering race on pi.on(...) handlers) is now MOOT on the post-#794 tree:
  // #794 already made installOrganizationIntercom `async` and its default
  // export already `await`s it (for unrelated reasons — the boot-time
  // manifest read), so pi.on(...) registrations already happen after an
  // await point regardless of this function. Re-derived and flagged to
  // architect2 rather than left stated as still-true (the design record
  // has the full re-derivation). The FIRST argument stands independently:
  // converting a spawnSync that only ever runs on a rare fallback path is
  // not worth the complexity, since #827's own harm rationale for deleting
  // agent-thread parks (blocking the agent's UI/SSE reader/mailbox drain)
  // doesn't reach install-time code anyway — there is nothing running yet to
  // freeze. This is a PERMANENT, structural justification, not a
  // "temporarily, until X lands" stopgap, so it satisfies the closed-register
  // rule; the issue body's "exactly these entries and no others" line was an
  // inventory snapshot that missed this site, not a policy decision to
  // exclude it.
  // NOTE (first real gate run, confirmed empirically): authoritativeRuntimePane's
  // spawnSync usage is real in the source (:1347, `run: typeof spawnSync = spawnSync`
  // default-parameterized, called inside the function body as `run(...)`) but this
  // scanner's literal-text detector (`\bspawnSync\s*\(`) does NOT catch it — the
  // actual call site uses the injected/defaulted local parameter name `run(`, not
  // the literal token `spawnSync(`. An allowlist row here would be STALE (no
  // matching scanned site) and fail the scan's own bidirectional check. Removed
  // the row rather than leave a fabricated match. The architect2 ruling on
  // Question A still stands as PRODUCT POLICY (do not convert this site to async;
  // see the design record) — this note just records that the scan
  // itself provides no enforcement for this specific site, a known detector gap
  // (indirection through an injectable parameter). The OTHER detector gap this
  // note used to point at — a re-arm through a named callback — is closed as of
  // #751/R11; this one, indirection through an injected/defaulted parameter, is
  // not, and following it would mean resolving a value the scan cannot see.

  // ===========================================================================
  // DEADLINE — bounded timeouts enforcing a real "this must eventually fail"
  // deadline, not a re-read of state that could be pushed instead.
  // ===========================================================================
  // organization-intercom.ts's `setInterval` primitive has FOUR distinct real
  // sites (#966/#967's own bag-keying makes this visible for the first time —
  // the old (file, primitive) key silently let one entry cover all four).
  // Three are the injectable `OrganizationIntercomScheduler` seam's own type
  // signature and pass-through wrapper — scaffolding the detector's literal
  // `setInterval(` regex necessarily catches, not a second poll decision —
  // and the fourth is the real call this seam exists to make testable: the
  // turn watchdog. All four are registered individually rather than left for
  // the old entry to silently absorb.
  {
    file: "packages/piing/extensions/organization-intercom.ts",
    primitive: "setInterval",
    match: "setInterval(callback: () => void, intervalMs: number): OrganizationIntercomInterval;",
    reason:
      "deadline: `OrganizationIntercomScheduler`'s own interface method signature — a TYPE declaration, not a call. The detector's literal `setInterval(` text match necessarily catches interface members that name the primitive; the real, classified call this seam exists to make injectable/testable is the turnWatchdogTimer entry below.",
  },
  {
    file: "packages/piing/extensions/organization-intercom.ts",
    primitive: "setInterval",
    match: "setInterval(callback, intervalMs) {",
    reason:
      "deadline: `defaultScheduler`'s pass-through implementation of the `OrganizationIntercomScheduler` seam — the method shorthand's own declaration line, not the call inside it (that is the next entry below). Exists only so tests can inject a fake scheduler for the turn watchdog without a real timer running.",
  },
  {
    file: "packages/piing/extensions/organization-intercom.ts",
    primitive: "setInterval",
    match: "return globalThis.setInterval(callback, intervalMs) as unknown as OrganizationIntercomInterval;",
    reason:
      "deadline: `defaultScheduler.setInterval`'s real body — the one place this file actually calls the global primitive, on behalf of the turnWatchdogTimer entry below (production always uses `defaultScheduler`, tests inject their own). Not itself a second poll decision; it is the seam's plumbing.",
  },
  {
    file: "packages/piing/extensions/organization-intercom.ts",
    primitive: "setInterval",
    match: "turnWatchdogTimer = setInterval(turnWatchdogTick, turnWatchdogIntervalMs);",
    reason:
      "deadline: the turn watchdog. Armed at turn_start, disarmed on settle (#368) — an idle company runs it zero times. It enforces the 15-minute stuck-turn deadline; it does not read chiefd on a cadence, it fires once to declare a turn stuck after the deadline elapses.",
  },
  {
    file: "packages/piing/extensions/organization-intercom.ts",
    primitive: "setInterval",
    match: "busyBeat = setInterval(() => {",
    reason:
      "deadline: the busy-work beat (2026-08-24). Armed only while the pane is doing work that emits NO turn events -- a compaction, or chief's own pre-turn compaction hold -- and disarmed the instant that work ends, so an idle company runs it zero times, exactly like the turn watchdog above. It reads nothing and samples nothing: it re-sends the SAME `working:true` fact the turn-event path already sends, because chiefd trusts a beat for a bounded window and silent work would otherwise be read as idleness and parked mid-compaction (measured: a person reaped mid-compaction at ~90% of a 1M context, after paying for a 909k-token summarize call). It carries its own hard ceiling (`ORGANIZATION_COMPACTION_BEAT_CEILING_MS`), after which it stops itself and the ordinary settle owns the person again -- a floor on the operator's ruling, never a pin.",
  },
  {
    file: "packages/piing/extensions/organization-intercom.ts",
    primitive: "setTimeout-self-rescheduling",
    match: "idleResumeTimer = setTimeout(() => {",
    reason:
      "bounded-retry: scheduleIdleResume's outer idle-resume timer (#827 step 7) is flagged because its own callback lexically contains a second, nested setTimeout — the ONE bounded fallback attempt (ORGANIZATION_IDLE_RESUME_MAINTENANCE_FALLBACK_MS) armed only while an idle-resume is waiting on the session-maintenance doc-change the extension already subscribes to. That inner timer is armed at most once per wait (guarded by `if (idleResumeMaintenanceFallbackTimer === undefined)`) and is cancelled the instant the awaited doc-change event arrives; it never re-arms itself, unlike the deleted unconditional self-reschedule this step replaced. Not change detection — it exists only so a missed event cannot wedge the wait forever. (#751/R11 CLOSED the scanner gap this entry used to record: the detector now follows a named callback one hop, and the reflection-retry compaction-hold ladder it named — `runRetry`'s `held` re-arm — was DELETED rather than registered. It failed all five classes: it re-armed every >=100ms purely to re-ask whether `nativeCompaction.requestId` had cleared, a question the compaction's own three exit paths already push through `resumeAfterNativeCompaction`. #751/P4 then deleted that retry queue outright along with the rest of the handoff-document machinery. No row here, because there is no site left.)",
  },

  // The following three rows were NOT in the issue body's original register
  // table — found only once the sleep regex was corrected to also match
  // `.sleep(` seam-injected calls (see scripts/reactive-scan.ts's SIMPLE_MARKERS
  // comment). Same inventory-omission situation architect2 ruled on for
  // authoritativeRuntimePane: real, pre-existing, bounded-retry sites that
  // predate #827 and are not in this story's file-by-file table. Added here
  // so the register is actually complete rather than leaving the scan
  // permanently red on day one; flagged to architect2 as a scope note rather
  // than assumed silently, since none of these three files are #827's to own.
  // #825 (E8-S3): the two rows that used to sit here — org-supervisor.ts's
  // two bounded process-liveness probes (freshly-spawned-child /proc
  // start-time readability, exit confirmation) and org-supervisor-entry.ts's
  // startup-readiness wait — are gone. Both sites died with the detached
  // TypeScript supervisor process that made them: org-supervisor-entry.ts is
  // deleted whole, and org-supervisor.ts no longer spawns, waits for, or
  // probes any child process. Removed rather than left stale (this register
  // is bidirectional).


  // #950/#954 (write, not yet landable -- see
  // the design record): the two new CAS-conflict retry
  // ladders #828 added, retiring the acquireDurableLock this issue's own
  // Isolation section excludes from THIS scan ("any lock (E8-S6)"). A
  // seq-conflict retry is not lock contention -- there is no held lock to
  // wait out, just a bounded reattempt after a losing compare-and-swap,
  // same shape as org-durable-store.ts's already-registered lock-acquire
  // ladder above, registered here because retiring the lock is exactly
  // what makes this a new site rather than a continuation of that one.
  // The four CAS-conflict ladders that used to sit here — org-session-
  // maintenance.ts, org-goal-intents-store.ts, org-acks-store.ts and
  // org-operator-escalation-intents-store.ts — are deleted with the files
  // themselves. Their decisions moved into chiefd
  // (chiefd-core/src/store/session_maintenance_ops.rs, supervision_intake.rs),
  // where the read, the decision and the write happen inside one
  // `BEGIN IMMEDIATE` on the writer thread. There is no compare-and-swap left
  // to lose and therefore no ladder to register: the retry existed only
  // because the decision was made in a different process from the write.
  // org-company-session-actions.ts has TWO CAS-conflict ladders in different
  // functions, with different loop-variable names (attempt vs retryAttempt)
  // so their exact text differs -- two distinct rows, not a duplicate.
  // org-fresh-session-transaction.ts's two whole-function CAS ladders are
  // deleted with the file. The handoff is now one transaction in
  // chiefd-core/src/store/fresh_session.rs: both ledgers live in one chief.db
  // written by one thread, so the elaborate protocol that made two independent
  // HTTP writes look atomic has no subject left.

  // ====================================================================  // PRODUCT-POLICY constant — not a wait at all, a floor on user-arm-able
  // input. Recorded here (not a scan hit — the scan only matches the six
  // primitive kinds above) purely so a future reviewer sees this file was
  // considered and deliberately excluded, matching the issue body's register.
  // ===========================================================================
  // apps/cli/src/legacy/organization/org-reminder-store.ts — MIN_REMINDER_INTERVAL_MS
  // is a floor on what a user may arm as a reminder cadence, not a timer this
  // process runs; it never matches any DETECT_PATTERNS entry below.
] as const;
