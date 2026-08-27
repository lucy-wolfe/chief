# Concept-collision audit — two stores implementing ONE concept

The companion to `store-implementation-audit.md`, and the harder geometry.

## Why this needs its own pass

| | two implementations of ONE STORE | two stores implementing ONE CONCEPT |
|---|---|---|
| how they fail | the same read returns different bytes | they never disagree, because they never touch |
| each side, viewed alone | one is stale or wrong | **both are individually correct** |
| how you find it | compare reads | ask *"what else claims this word?"* |
| what a sweep finds | the divergence | **nothing — every check passes** |

Everything built for the store class — the collision guard, the who-writes
question, the id-scheme discipline — **finds none of this class.** Both sides
pass their own tests, both are internally consistent, and no comparison exists
to run because the two halves share no row, no key and no reader.

## The diagnostic

Not *"do they disagree"*. **"COULD they ever disagree, and would anything
surface it?"**

If two subsystems can hold contradictory states for one concept and nothing in
the system would ever notice, that is the finding — whether or not they are
contradictory today. #22's `blocked` is exactly that, and nothing errored.

## Record OBSERVABILITY, not just the verdict

For every collision, also record: **could either side ever observe the other's
state?**

If neither can, then no test will ever fail, no monitor will ever fire, and no
amount of care at the keyboard helps — **the enumeration itself is the only
defence.** That makes this document the artifact, not a list of fixes to work
through: its value is that the next person inherits the pairing instead of
rediscovering it after an operator complaint.

Where one side CAN observe the other, say so and name the check — that pair is
testable, and a test should be written.

## Verdict vocabulary

- **SAME-THING-TWO-STORES** — a finding. One concept, two homes, silent
  divergence possible.
- **DIFFERENT-THINGS-SAME-WORD** — safe, but worth naming: the collision is in
  the vocabulary, not the data, and it costs reader-hours.
- **SINGLE-OWNER** — clear; recorded so nobody re-checks it.
- **AUTHORITY-PLUS-PROJECTION** — one writer, one regenerable copy. Safe *only*
  while nothing writes the projection.

## Findings

### `skill` — AUTHORITY-PLUS-PROJECTION, with a silent-overwrite edge (#48)

**Observability: NONE in the direction that matters.** The store cannot see an
edit made to the projection — the swap simply overwrites it — and nothing
compares the two before or after. No test can catch this; only the rule
"agents may not write the projection" (or making disk authoritative, which is
what #48 asks for) does.

The durable `learned-skills/<personId>` store is the documented canonical
source (`src/organization/org-learned-skills.ts:25-31`); the files under a
person's pi-home `skills/` are a regenerable projection produced by
`projectLearnedSkills`, called from `materializeOrganization`
(`org-materialize.ts:560`), which runs on runtime reconcile
(`org-runtime.ts:532`).

**The mechanism worth recording**, because #48 is boarded as a product
disagreement ("skills must be disk-authoritative") without it: the projection
is an atomic per-skill swap — `target = join(skillsRoot, skill.slug)`, staged
then `renameSync`d (`org-learned-skills.ts:843-856`). So **an agent editing its
own projected learned skill has that edit destroyed at the next materialize,
silently** — no error, no diff, no record that a write was lost. The blast
radius is precisely the projected learned-skill slugs; a skill the agent
creates under a *different* slug is not swapped and survives.

That is the concrete answer to "could they disagree and would anything surface
it": they can, on every reconcile, and nothing does.

### `task` / `assignment` / `goal` — SAME-THING-TWO-STORES, the largest finding

**Two independent work ledgers, in different storage engines, with no key
between them anywhere in the repository.**

- **Org TASKS store** — SQLite `tasks` table
  (`chiefd-api/src/docstore/tasks.rs`), TS client `org-task-store.ts`, commands
  `org-task-command.ts`. Keyed `(slug, task_id)`, dept-scoped ids.
- **Supervision assignment ledger** — JSON document
  (`chiefd-core/src/store/supervision.rs`, `org-supervision-state.ts`). Keyed by
  assignment id under `delegatedGoalId` / `managerGoals`.

They carry **the same four fields under the same names** — `request`,
`expected_output`, assignee, status (`tasks.rs` vs `supervision.rs:274-278`) —
and each defines its own answer to "is this work still open":
`TaskStatus::is_terminal()` (`tasks.rs:121-124`, 8 states) and
`AssignmentStatus::is_open()` (`supervision.rs:234-238`, 4 states).

**Cross-link grep: ZERO.** `taskId`/`task_id` appears in no supervision source;
`assignmentId`/`delegatedGoalId`/`goal-` appears in none of `tasks.rs`,
`org-task-store.ts`, `org-task-command.ts`. No foreign key, no join, no
reconciler.

**Observability: NONE, in either direction.** No code path in the repository
reads both. `org-supervision-state.ts:820-890` validates only intra-ledger
coherence; `tasks.rs:130-150` validates only intra-row legality. There is no
cross-store invariant and no conformance scenario naming both. **So no test can
ever fail, and this enumeration is the only defence.**

**Concrete consequence today**, not hypothetical: the live-work status line
(`extensions/organization-live-work.ts:307-333`) iterates `assignmentOrder`
only. **A person with five `in_progress` tasks and no assignments renders as
having no live work** — which is also what the idle/settle machinery reads.

**The open question I cannot answer from the code, and it decides the fix:** are
these meant to be the same work, or deliberately separate tiers (supervision =
manager→agent runtime delegation, tasks = a human-facing tracker)? If the same,
a foreign key and a cross-store invariant are missing. If separate tiers, the
fix is *vocabulary separation*, not reconciliation — and the current naming is
actively misleading. `org-task-command.ts:4-9` describes itself as mirroring
the assignment CLI, which reads as parallel-by-design rather than layered, but
no design doc states ownership. **This needs a human who knows the intent.**

### `blocked` — one safe collision and one with real harm

**blocked(task) vs blocked(delivery gate): DIFFERENT-THINGS-SAME-WORD (safe).**
`TaskStatus::Blocked` (`tasks.rs:79-80`, requires a non-empty `blocked_reason`)
means "a human must clear a blocker". `blocked_generations`
(`supervision/delivery.rs:171-192`) is a per-`(assignee, generation)` transport
gate — computed, never stored, never a work state. They cannot be confused by
code; they cost reader-hours only.

**blocked(task) vs blocked(check-in prose): SAME-THING-TWO-STORES (finding),
and this one has teeth.** The check-in contract asks the agent for literally
`STATUS: working | blocked | done` (`check_ins.rs:75`,
`org-agent-contracts.ts:30`) — the same semantic `TaskStatus::Blocked` stores —
but the answer lands in a message body that nothing parses into either store.

**The harm:** an assignment's `progressDeadlineAt` is set and cleared purely
from `assignment.status === "acknowledged"`
(`org-supervision-state.ts:677-682`), consulting no task status. So supervision
will eventually mark an assignment **failed for silence on work the task store
already knows is blocked** — "blocked" in the task store buys no grace in the
assignment store. Symmetrically, `org task status --blocked` fires a
`task_blocked` notification while the assignment sits `acknowledged`, ticking.

**Observability: NONE.** Nothing consults task status from the supervision
deadline path.

### `priority` / `focus` — SAME-THING-TWO-STORES, low blast radius

A 4-value enum with fairness aging (`org-goal-priority.ts:11-25`,
`effectiveGoalRank`, `FOCUS_DEFERRAL_CAP`) versus a bare sort integer
defaulting to `200_000` (`tasks.rs:182-183,709`). No conversion function
exists. A goal at `urgent` and its real-world twin task at `900000` contradict
freely.

Mild because priority is explicitly declared presentation-only
(`org-goal-priority.ts:7-8`) — it misleads a human, it corrupts no state. But
note the instructive detail: that file claims to be *"one canonical priority
vocabulary … shared by every goal/assignment consumer"* so consumers *"can
never disagree."* **True within the ledger, and silently excluding half the
system** — a correct-in-scope invariant whose scope nobody stated.

### `deadline` / `due` — DIFFERENT-THINGS-SAME-WORD (safe)

Three genuinely distinct clocks: `workDeadlineAt` (when the work is expected),
`acknowledgementDeadlineAt` / `progressDeadlineAt` (when to declare the agent
dead), and schedule `nextDueAt` (when to wake the manager next). Correctly
separated, and the one place they could interact is deliberately wired
(`check_ins.rs:39`, `deadlines.rs:372-375` folding all schedules into a single
next-wake) — the exact cross-check absent from the findings above.

**The asymmetry is the finding, not a contradiction:** assignment work carries
a mandatory `workDeadlineAt`; **tasks have no due date at all** — `due` and
`deadline` appear nowhere in `tasks.rs` — so task work can never be late. That
reinforces the first finding: these are not two views of one system.

### `owner` — SAME-THING-TWO-STORES, and it is about who owns a running fleet

Two stores answer "who owns this runtime", in two different durability domains,
with no reconciler:

- **O1 `runtime-owner`** — a per-company docstore row
  (`src/organization/org-runtime-ownership.ts:39-44`), `status: active |
  released`, `socketName`.
- **O3 the host registry** — `/tmp/team-launcher-<uid>/supervisors`
  (`src/organization/org-supervisor-host-owner.ts:22-36`), keyed
  `(socketName, sessionName)` with `token`/`pid`/`processStart`; the Rust
  counterpart consumes it at `chiefd-host/src/supervisor_takeover.rs:22-28`.

Both are keyed by socket/session, both carry claim + liveness semantics.

**Concrete contradictory sequence:** supervisor P claims the host registry for
`(sock-A, sess-A)` and writes `runtime-owner = {active, sock-A}`. P is
`SIGKILL`ed — the `/tmp` record is now dead-but-present and the docstore row
still says `active`. A takeover from `sock-B` classifies the dead record as
reclaimable (`supervisor_takeover.rs:27`) and claims it. **Two stores now name
two different sockets as owner of one runtime.**

**Observability: NONE.** `org-diagnostics.ts` has zero hits reconciling
`runtimeOwnership` against `hostOwner`; every consumer
(`triber/stop.ts`, `ls.ts`, `reset.ts`, `attach-wiring.ts`,
`launcher-wiring.ts`, `cli.ts:94`) calls `loadOrganizationRuntimeOwnership`
alone. **Nobody loads both.** The only thing that could catch it is the tmux
tag (the physical ground truth), and only if a reconcile happens to run against
the wrong session — incidental, not a check.

Given that a fleet-ownership disagreement is what produced the shadow fleet,
this is the one I would fix rather than merely record.

### `lease` — DIFFERENT-THINGS-SAME-WORD ×3, with one dual-exclusion edge

chiefd duty leases (`chiefd-core/src/lease.rs`), the docstore row lock
(`org-durable-store.ts:389-475`) and the crash-safe file lock
(`chiefd-host/src/crash_safe_lock.rs`) are three mechanisms at three scopes,
and the code argues the point itself — `crash_safe_lock.rs:6-10` is an explicit
case for "one mutual-exclusion mechanism, not two". Their retry ladders are
shared POLICY (both ported from `org-lock-retry.ts`), not shared state.
`ceo-boot-lease` is not a lock at all but a durable record of a hold already
taken, and says so.

**The edge worth recording:** for a store that has BOTH a SQL and a file form,
a TypeScript writer holding the crash-safe FILE lock and a chiefd writer
holding the SQL generation-CAS are excluding against two different objects.
`runtime-owner` is named as exactly such a store (`org-store.ts:733` registers
it file-backed; `org-runtime-ownership.ts:89` mutates it through the docstore).
The CAS surfaces a competing SQL writer and is blind to a file-form writer.
**That compounds with the `owner` finding above — same store, two exclusion
domains AND two ownership records.**

### `intent` — MY HYPOTHESIS WAS WRONG (recorded, not quietly dropped)

I expected `lifecycle_intents` (native table) and `launch-intent` (TS document)
to be one mutual-exclusion fence with two homes. **They are not.**
`lifecycle_intents` is a company create/remove/move crash-recovery journal —
phases `creating | removal-pending | removing | moving`, keyed `(slug, txn_id)`,
and `lifecycle.rs:3` claims `registry.db` as sole authority for company
lifecycle PHASE. That is company *existence*. `launch-intent` is per-person
authorisation *within* a live company. Disjoint units; they cannot contradict.

`goal-intents` and `operator-escalation-intents` are queues — "a request not
yet applied" — not fences. DIFFERENT-THINGS-SAME-WORD throughout.

### `generation` — SINGLE-OWNER today, with a latent confusion nothing prevents

`assignment.generation` is `runtimeGeneration` denormalised, and the invariant
is enforced (`org-supervision-state.ts:845-847`: `<= runtime.generation`, exact
equality for active statuses). `session-epoch` is an instant, not a counter, so
no comparison is even expressible.

**The latent part:** the person incarnation counter and the docstore row CAS
counter are both called `generation`, both are bare `number`, both start at 1,
and both are live in one function's scope (`org-supervision.ts:341` reads the
row generation; `:262`/`:415-419` read the runtime generation). **No site
compares one kind to the other today — and nothing structural stops it.** A
mis-wire would produce a value that passes `Number.isSafeInteger(...) && >= 1`
and would only fail if it happened to exceed the runtime generation; for small
values it passes silently. The cross-process boundary is worse: the
`ORGANIZATION_RUNTIME_GENERATION` env var is untyped, so whatever writes it
must simply know it is an incarnation counter and not a row counter.

**Observability: NONE**, and the defence is naming discipline alone. A newtype
on either side would convert it into a compile error.


### `health` — already filed as #93

Both sides collect and write; the names differ (`health` vs `health-monitor`)
so it reads as two concepts, and only one has a consumer that acts. Filed under
the store audit because the tables also differ, but it is genuinely both
geometries at once.

## Boundary of this pass

Stated plainly, because an unfinished audit that names its edge is worth more
than one that implies coverage:

- This pass works from **vocabulary**, so it finds only collisions where the two
  subsystems chose the SAME WORD. Two stores implementing one concept under
  *different* names (the `health`/`health-monitor` shape) are invisible to it —
  those need the concept-by-concept pass, which is larger.
- It does not cover data that crosses the process boundary into Pi itself
  (session transcripts, native Pi state), where "who owns this" has a third
  possible answer.

## A meta-finding from running this audit

Two of my own sweeps returned contradictory claims about `launch_intent`: one
said its Rust writers target the native `documents` ledger (no bridge), the
other said `NAME = "launch-intent"` means it writes the SAME docstore row as
TypeScript. I resolved it by reading `Ledgers::put_document` (`ledger.rs:362`),
which inserts into `self.documents` — **the native per-company map, not
`org_documents`.** The first was right.

The second inference — *same NAME therefore same store* — is **exactly the
trap this entire audit exists to prevent**, and it was made by a reader who had
been explicitly briefed on that trap in the same prompt. That is worth more
than the finding itself: the confusion is not carelessness, it is the natural
reading, which is why it needs a mechanical guard rather than a warning.
