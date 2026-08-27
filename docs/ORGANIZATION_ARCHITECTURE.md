# Organization architecture reference

> The detailed durability and runtime reference. For the source map and the
> concise data-flow overview, see [Architecture](ARCHITECTURE.md). For the
> newcomer-friendly explanation, see [What is a company?](WHAT_IS_A_COMPANY.md).
> For repairing a running company whose durable state has drifted, see the
> [Live recovery runbook](LIVE_RECOVERY_RUNBOOK.md).

chiefd manages companies, not independent panes that happen to call each other.
One company is one recursive unit hierarchy, one SQLite database and one
detached `chiefd run` daemon. The daemon is client-agnostic: it decides who
should be running and publishes that, and a client — the `chief` binary in a
terminal, or `apps/web` in a browser — decides where those people are shown and
is the only thing that runs them. This document covers what is durable, who may
change it, and what happens when a process dies mid-change. Where the code and
an older intention disagree, it describes the code.

## Where a company's state lives

Every durable fact is a row. Each company owns one SQLite file at
`~/.chiefd/orgs/.<slug>.chief.db`, a dotfile *beside* its directory rather than
inside it, so genesis and removal can manage the directory and the database
independently and in the order removal requires
(`apps/chiefd/crates/chiefd-daemon/src/company_db_target.rs`). The dotfile prefix also
keeps the database invisible to every directory-entry enumeration of the data
root. The schema is one constant, `COMPANY_SCHEMA_SQL`
(`chiefd-core/src/schema.rs`), applied by one shared opener; the `slug` column
in every table is a composite document key (`<slug>@sha256(dataRoot)[..12]`),
not the bare slug.

The only tree on disk is the company's Pi artifacts, and the materializer is the
one writer of it (`chiefd-host/src/materialize/mod.rs`):

```text
~/.chiefd/orgs/<slug>/
├── people/<person>/
│   ├── pi-home/                 Pi's own home; chiefd stages it, Pi owns it
│   │   ├── skills/              baseline copies, the <person>-role skill, plan skills
│   │   ├── extensions/          <entry>/index.ts plus its helper closure
│   │   ├── packages/
│   │   ├── sessions/            private Pi history; the `--session` resume target
│   │   ├── settings.json        written ONCE at creation, never re-merged
│   │   ├── trust.json           written ONCE at creation
│   │   ├── chiefd-identity.key.pem
│   │   └── .organization-reload-hard-contract.json
│   └── workspace/
│       ├── AGENTS.md            the ONE on-disk projection of the person contract
│       ├── company    -> ../../../shared/company
│       └── department -> ../../../shared/departments/<id>
├── shared/company/
├── shared/departments/<department-id>/
├── logs/                        bounded redacted diagnostics
└── bus/events.jsonl             an append-only trail the Pi extensions write
```

**Everything the previous generation of this document put beside that tree is
gone, and not because it moved — because the concept did.** There is no
`org.json`, no `state/` directory, no `person.json`, no `bus/mailboxes/`, no
`bus/acks/`, no pid file, and no JSON projection of durable state anywhere
outside a Pi home. That is MANDATE 5, stated in
`chiefd-host/src/runtime_lifecycle.rs` and enforced by the materializer, whose
launch gate `symlink_metadata`s every managed root. Mail is the `mailbox` table;
acknowledgement receipts are `ack_receipts` rows; the per-person materialization
checkpoint that replaced `person.json` is a `materialization_checkpoints` row;
the operating contract is `person_contracts`. Backing up a company directory
therefore backs up Pi's homes and nothing else — the company is the database.

A person's directory is company-global by construction: every path is
`people/<person-id>/…` with no department segment anywhere in it, which is why a
transfer can never move or duplicate sessions, workspace, or memory.

Two things under the tree are written but never read back as authority.
`logs/exceptions.jsonl` collects bounded redacted diagnostics from the
background-memory worker and the Pi extensions, and the health monitor scans it
with a durable per-file cursor. `bus/events.jsonl` belongs to the Pi extensions,
not to chiefd — chiefd neither writes nor reads it. The health snapshot used to
seed `logs/supervisor.log` into that scan as well, a read with no writer since
the detached supervisor process was deleted; the seed is gone
(`chiefd-host/src/gather/health_snapshot.rs`). It was not repointed at the
daemon log under `~/.chiefd/run/<slug>.log`, which is `tracing` diagnostics and
never authority: every error-level line there would trip the scanner's
substring filter, including the health monitor's own gather-failed and
commit-refused warnings, so the duty would manufacture incidents out of its own
reporting. The directory walk still admits any unstructured `.log` in the
company's `logs/`, so nothing that acquires a writer later needs the seed back.

## The structural model is normalized rows, not a manifest document

The recursive hierarchy and the staffing graph are real typed columns. No column
stores JSON; text enums are `CHECK`-constrained, arrays became child tables,
id-keyed maps became primary keys, and ordering became an `ordinal`.

| Structure | Table | What the database itself enforces |
| --- | --- | --- |
| Units | `departments` | `kind IN ('company','department','contract')`; exactly one root per company (`departments_one_root`); at most one headship per person (`departments_one_head`); sibling order is a bijection within a parent (`departments_sibling_ordinal`); the three `contract_*` columns are present exactly when `kind = 'contract'` |
| People | `people` | `kind IN ('worker','head','executive')`, `employment_state IN ('active','benched','departed')`, one `department_id` foreign key, and a per-company `ordinal` bijection |
| Capability plan | `person_tools`, `person_resources`, `person_prompts` | ordered tool grants, `kind IN ('skill','extension','package')` resource grants with a rationale, ordered prompt-template refs |
| Operating contract | `person_contracts` | the contract text and its md5, deliberately without a `people` foreign key so the store can be rewritten whole on its own fence |
| Staffing history | `staffing_history` | the eight-term action vocabulary, deliberately without a `people` foreign key so a departed person's history survives them |

What SQL cannot express — tree acyclicity on a reparent, whole-tree ordinal
bijectivity, head-exists-and-is-a-member — runs in `validate()` inside the same
`BEGIN IMMEDIATE` transaction as the mutation.

**The monotonic manifest revision and its generation compare-and-swap are
retired, not renamed.** There is no `revision` field, no `expectedRevision`
parameter and no `--revision` flag;
`scripts/test/organization-revision-tripwire.test.mjs` is a repo tripwire that
fails if one comes back. What the revision was genuinely needed for is now
`org_events`: a strictly monotonic per-company `seq` allocated from a dedicated
counter row inside the same transaction as the mutation (never `MAX(seq)+1`,
never a global autoincrement), carrying `entity`, `entity_id`, `op` and a
`detail_ref` of the form `table:pk` — a thin index that points at the owning row
rather than inlining it. SQLite's single writer makes commit order equal to `seq`
order, so the feed is gap-free and totally ordered per company. Every consumer —
the SSE watch, the Pi footer, converge triggers, materialization staleness —
re-reads from its last acknowledged `seq`; the socket is only a nudge and no
consumer trusts the wire.

Materialization staleness is keyed to that feed rather than to a revision:
`materialization.checkpoint_seq` is compared against `max(org_events.seq)`, which
is a comparison and not a lock, so a stale check can never fence a writer.

## Units, people, and who should be running

Every company's root unit has `kind = 'company'` and is headed by the CEO.
Durable children are `department`; bounded engagements are `contract` and carry
an engagement, a launch timestamp and an optional expiry. Departments and
contracts may recursively contain either kind. People are occupants of units,
never nodes of the structural tree. There is no separate contract table — a
contract is a department row with transient metadata, so stop, resume and remove
are the same operations for both kinds.

`runtime::reconcile_plan` carried two unrelated things under one name and was
split. What survives in chiefd-core is `runtime::desired` — the desired-person
model and the ONE predicate `is_desired_person`. The topology half — desired
panes, desired windows, the observed topology, the ordered plan of pane steps
and the layout maths — moved to the client's `actuate::plan` and
`placement.rs`. Nothing left in chiefd-core names a session, a window, a pane, a
socket or a layout.

Placement is therefore a pure function the **client** computes
(`chief-cli/src/placement.rs`):

- every person sits in the window of their **own** department, heads included —
  a head is not an exception because appointing a head MOVES their
  `department_id` into the unit they head, so clicking a department shows that
  department's team with the person who leads it.

Until 2026-08-14 a head sat in their department's **parent** window instead, so
a manager appeared beside the peers they report to. That rule was retired
because it was the only surface that disagreed with the durable record it was
displaying; the rail still lists a child department's head under the parent, as
a row, so the manager is still reachable from above.

The backend keeps no second copy of that rule, and a test in
`chiefd-core/src/store/organization.rs` named
`the_store_defines_no_head_in_parent_placement_rule` is what stops one growing
back. The stale-copy risk was real rather than theoretical: the old
`pane_department_id` answer was persisted in
`person_activity.last_pane_department_id` and
`transitions.from_pane_department_id`, so the wrong answer outlived the pass
that computed it.

Availability is inherited, and it fails closed. `stopped_organization_unit_ancestor`
walks upward from a unit with a visited set and returns the first non-active
ancestor; an unknown unit or a cycle in the ancestry reads as **not** active,
because a structurally broken tree must never read as "keep running". One paused
ancestor therefore makes its whole descendant subtree effectively stopped, and a
person is desired only when their assigned chain is active, their employment
state is `active`, and any unit they head is itself in an active chain.

Hiring, recalling, transferring, starting a person into, or reparenting
under a paused subtree is refused at both layers: the pure decision core returns
`stopped-destination` naming the *ancestor* that caused it, and the SQL layer's
`department_or_ancestor_is_paused` — which returns true for a cycle and true for
a missing department — guards `create_department`, `reparent_department`,
`transfer`, `move-members`, `hire`, `start` and `recall`.

Placement is derived from these rows on every pass, by the client, from facts it
re-reads. A runtime label is an observation and never decides ownership, which is
what makes a restart deterministic and stops a window outliving the unit it was
named for.

## The operator surface

The human lifecycle is the `chief` binary's own verb table and nothing else.
`OPERATOR_VERBS` in `apps/chiefd/crates/chief-cli/src/main.rs` is that table, and
it has seven rows — `ls`, `attach`, `stop`, `rm`, `actuate`, `reset`,
`topology` — beside bare `chief`, `help` and `--version`. Bare `chief` opens
Founder mode when the directory has no company. When the company exists, it
uses the same start-and-attach path as `chief attach`. `route()` matches the
verb table and answers anything else
with `RouteError::UnknownCommand`. Every verb rejects unknown flags, duplicate
flags and stray positionals before any mutation, and `--yes` is accepted only
by `rm` and `reset`, which delete or shed state.

`chief` does not dispatch the daemon itself. `DAEMON_VERBS` — `run`,
`docstore-only`, `bootstrap-store`, `set-actuation-config`, `clear-breaker`,
`memory-worker` — are `exec`ed into the sibling `chiefd` binary resolved
from `current_exe()`, never from `PATH`, so `chief run` and `chiefd run`
are one invocation of one program. `host` is the seventh unadvertised mode and is
answered by the client itself: its own doc comment says it "is spawned by
`scripts/start-stack.ts` and never typed, so it is deliberately absent from
[`OPERATOR_VERBS`] and therefore from the usage text."

`chief rm <company>` is the one verb that deletes durable state, and its order is
recorded as data in `chief-cli/src/remove.rs` — `const REMOVE_ORDER: [&str; 4] =
["confirm", "stop-runtime-and-daemon", "delete-durable-state",
"delete-beacond-row"]`. The beacond row goes **last** on purpose: a row without a
company is a state `chief ls` can name and `chief rm` can finish, while a
company without a row is unreachable. There is deliberately no preflight.

`chief ls` has four status words, and the fourth is the one an operator meets
after a partial removal. `CompanyStatus` in `chief-cli/src/listing.rs` is
`Running | Stopped | Missing | Unknown`, labelled `"running"`, `"stopped"`,
`"missing"`, `"unknown"`. `missing` means "beacond holds a row and there is no
company behind it: the store database named by the row's `orgsRoot` is not on
disk". Nothing can start it, so it is offered `chief rm` and never
`chief attach`. A healthy daemon is asked first and wins outright; only then does
on-disk store presence decide, so `missing` is never a verdict about a running
company.

There is no `company`, `department`, `contract`, `launch`, `reconcile`, `catalog`
or `provider-admission` CLI namespace, and there is no TypeScript CLI: `apps/cli`
is deleted, and `scripts/test/no-ts-cli-stub.test.mjs` keeps it deleted — its
`KNOWN_REMAINING` exact set is now **empty**, because the last production
reference went with the subprocess transport. Unit and staffing lifecycle
*inside* a running company is the protected `org_*` tool family the Pi processes
carry, not a second command surface. The repo's `bun run` targets are development
entry points; there is no `bun start`.

### From nothing to a running CEO, and why the order is the product

Bare `chief` or `chief attach` in the company directory takes an operator from
nothing to a running CEO pane. On a stopped company it starts the daemon
immediately, reads the recorded
socket back from that daemon, and then does three things **in this order**
(`chief-cli/src/attach.rs`):

1. **Make an actuator present.** `ensure_actuator` asks chiefd who is actuating
   and starts one only when the answer is nobody.
2. **Wait for that actuator to take chiefd's lease.** `await_actuator` is bounded
   by `ACTUATOR_BUDGET` = 45 s. A separate wait, `await_company_session`, follows
   the intent, because the lease is granted by the actuator's first report and
   the session is minted after it — attaching on presence alone hits tmux's
   `can't find session`.
3. **State CEO-only intent**, `prepare_ceo_only`.

The order is not stylistic, and the code says so in its own words:

> CEO-only intent stated while nobody is actuating is silently lost. The route
> answers `{"prepared":true}` and the company's CEO stays `desiredActive: false`
> forever, so the operator gets a healthy daemon, a 200, an encouraging line of
> output, and no company.

That block records the measurement behind it — three fresh companies, intent
before an actuator staying `desiredActive: false` at 5 s and at 25 s, and an
actuator present with no intent staying false too. **Neither half is sufficient
alone**, which is why an implementation that starts an actuator *after* stating
intent looks correct and produces nothing.

At most one actuator ever exists per company, enforced three ways: chiefd's own
presence answer is consulted first and `present` starts nothing (including an
operator's own `chief actuate` in another terminal, which attach cannot see); a
live actuator session is never respawned into; and a tmux read that does not
answer is `ActuatorSession::Unknown`, which fails closed, because "I could not
tell" must never start a duplicate.

### The actuator lives in its own session

The actuator does **not** live in the company's session, and the placement is
forbidden rather than merely avoided. The company session is the actuator's own
projection: `actuate::observe` reaps — `kill-session` — any session on the socket
carrying an EMPTY organization tag, because that is the corpse of a half-finished
mint. A session `attach` minted carries no such tag, so the actuator's first
observation would kill the session its own pane is in.

`actuator_session_name` is therefore `format!("chiefd-actuator-{company_session}")`
— for slug `acme`, `chiefd-actuator-org-acme_`, on the company's own socket. The
discriminating text is a **prefix** and not a suffix, and that was found by
running it rather than by reasoning: `<company-session>-actuator` reads better and
does not work, because `tmux -t <name>` falls back to prefix matching. On a live
`chief attach attach-proof`, the actuator in `org-attach-proof-actuator` asked
tmux for `org-attach-proof`, was handed its own session, read an empty
organization tag, correctly classified that as debris, and reaped itself. A name
can only be resolved by prefix to something it is a prefix *of*, and the company
session name is not a prefix of this one.

### How a protected tool reaches chiefd

`packages/piing/extensions/organization-intercom.ts` is the one protected
extension, and **it reaches chiefd only over HTTP**: `FetchTransport` and
`postOrgRoute` from `@chief/chiefing/extension-runtime`. A Pi extension talks to
the daemon it is already connected to, over that daemon's own API. The ported
surface spans manifest, memory, mailbox, department structure, person staffing,
goals and goal intents, assignments and acks, tasks, reminders, model and
thinking changes, session maintenance, the resource catalog, runtime and
lifecycle status, and the row stores.

**Every extension resolves its own company's daemon, and there is one function
that does it.** `resolveCompanyChiefdUrl`
(`packages/chiefing/src/discovery/DiscoveryClient.ts`) is a single beacond
lookup and nothing else — no port derivation, no cache, no retry, no fallback:

```ts
const row = await discovery.lookup(slug)
if (isNullish(row)) throw new UnknownCompanyError({ slug })
if (isNullish(row.url)) throw new CompanyNotRunningError({ slug })
return row.url
```

Two failures, two errors, because a caller must tell "create it" from "boot it".
Its three production callers are the three extensions that speak to chiefd:
`organization-intercom.ts`, `team-ui.ts` (the footer) and
`organization-memory.ts` (the memory tools). The absence of a
fixed-port fallback is the point: *"the old `DEFAULT_CHIEFD_URL` fallback is what
let a bootstrap for company A silently adopt company B's live daemon. 'I don't
know where it is' must be an error, not a guess."*

**`ORG_CHIEFD_URL` is gone entirely** — the stamp, both writers and every reader.
It carried one chiefd address per PROCESS, which is the right shape for exactly
one deployment and has no correct value at all in `apps/web`, where one server
process serves many companies. Its failure mode was the worst available: a wrong
daemon *answers*, commits the mutation into another company's database and
returns 200. `scripts/test/no-chiefd-url-stamp.test.mjs` bans the name from
production code under `apps/chiefd/crates`, `apps/web/src` and `packages`, with
comments stripped before the scan and **no exceptions at all** — "there is no row
here that can rot, because there are no rows." The one address-shaped variable a
Pi process reads is `BEACOND_URL`, an optional override of the compiled-in
`http://127.0.0.1:6969`.

`packages/piing/test/IntercomSeamClassification.test.ts` caps the distinct route
literals at 62, forbids a raw `fetch(` outright, and pins the subprocess
call-site count at **zero** — the count now includes any declaration, because
there is no declaration left to subtract. The subprocess transport into
`apps/cli/src/Main.ts` was deleted rather than ported, along with the sixteen
names in `DELETED_TRANSPORT_NAMES` that none of this code may grow back. Those
ceilings are ceilings, not targets: a packet that raises one has re-grown what
the guard exists to remove.

One `spawnSync` survives in that file, and it is unrelated to chiefd:
`authoritativeRuntimePane` reads `tmux list-panes -a` and then `ps` to recover
the pane id when the environment does not carry it. It is the third rung of a
ladder — raw `TMUX_PANE`, then `ORG_LAUNCHER_PANE_ID`, then this — and it
discovers **identity only**. The guard requires it to stay (`expect(SOURCE)
.toContain('import { spawnSync } from "node:child_process";')`) while banning a
bare `spawn(`, so the one legitimate subprocess cannot be used as cover for a
second transport.

## Runtime: one converge cycle, not a launch command

There is no `launch` and no `reconcile` verb. A company's runtime is a single
converge cycle that runs on every relevant change: **observe → plan → apply**.
The cycle is now split across the boundary: chiefd answers *who* from
`chiefd-core::runtime::desired` and publishes person-scoped actions, and the
client plans and applies *where* from `chief-cli/src/actuate/{plan,interpret}`.
`chiefd-host/src/converge_apply/` is the daemon's remaining impure half —
materialization, the launch catalog, the credential staging.

- **Observe** happens in the CLIENT (`chief-cli/src/actuate/observe.rs`), which
  reads the live tmux server and fails closed, and posts the result to
  `POST /v1/org/runtime/observed`. A foreign, duplicate or partially tagged
  object is refused, never adopted; an audit that cannot be read is
  *untrusted*, which is not the same as absent and never grants permission.
  The distinction reaches chiefd as an enum, not as a `people` list beside an
  `observationTrusted` flag — "untrusted, and here are zero people" would read
  downstream as *nothing is running*, which is a mandate to spawn the whole
  company a second time on top of one already up.
- **Plan** turns the manifest, the activity projection and the observation into
  an ordered list of steps. Before any pane is created, materialization is
  refreshed and every desired pane's argv is preflighted as a real argv vector —
  never an echoed shell line — carrying only that person's generated skill and
  extension paths, tool allowlist, role prompt, resolved provider and model,
  optional thinking level, and newest private session. A missing resource or an
  unroutable provider aborts before tmux is touched.
- **Apply** (`chief-cli/src/actuate/interpret`) runs steps strictly in plan order
  with no parallelism, and every destructive step **re-reads the live pane
  ownership tag at apply time** and proceeds only if it still matches what the
  plan expected. That is the module's most important property: it closes the gap
  between observing and acting. A failed step is fail-stop — later steps are
  abandoned, every pane this pass minted and tagged is re-verified and reaped, and
  idempotency comes from re-planning on the next pass rather than replaying this
  one.

A pane already carrying the current runtime generation is left untouched; a pane
whose generation fence changed is respawned in place from its newest private
session, so a revised identity or capability plan actually takes effect. The
generation travels both as the `@organization_generation` tmux tag and as
`ORGANIZATION_RUNTIME_GENERATION` in the pane's environment.

### Runtime state is rows

| Fact | Rows | Route |
| --- | --- | --- |
| Which socket and session own this company | `runtime_owner` — socket, session, `claimed_at`, `validated_at`, `status IN ('active','released')`; a released owner is a real recorded state with no socket, not an absent row | `POST /v1/org/runtime-owner/read` |
| What the runtime currently looks like | `runtime` (socket, status, the ramp columns, the recovery fingerprint and its confirmation, `recon_phase`/`recon_started_at`) with `runtime_panes`, `runtime_recovery_people`, `runtime_monitor_warnings` | `POST /v1/org/runtime/read` |
| Who is actuating, and whether they could vouch for what they saw | `runtime_actuation` (actuator id, `reported_at_ms`, `lease_ms`, `observation_trusted` and its `untrusted_reason`, coupled by `CHECK ((observation_trusted = 1) = (untrusted_reason IS NULL))`), `runtime_actuation_people`, `runtime_actuation_unknown` | `POST /v1/org/runtime/observed`, `POST /v1/org/runtime/actions` |
| Who should be running, and why | `person_activity`, `launch_intent` (presence *is* the intent), `runtime_generations` (the fence plus its attestation pair), `activity_meta` (the round-robin park cursor) | `POST /v1/org/activity/read` |
| In-flight lifecycle moves | `transitions` | `POST /v1/org/activity/command-status` |
| Whether each supervision duty is alive | `supervisor_watermarks` — one row per duty, its declared interval, `last_success_at`, `run_count`, and its most recent failure only, cleared on the next success | folded into the health monitor's durable facts |
| One-time effects | `event_once_markers`, keyed `(slug, sha256(event id))` | `POST /v1/org/event-journal/read` |
| Health observations and incidents | `health_monitor_observations`, `health_monitor_incidents`, `health_monitor_cursors`, `health_monitor_terminal_resolutions` | `POST /v1/org/health-monitor/read` |
| Converge serialization state | `converge_safety` — actuation mode, the destructive-action budget override, the circuit breaker, and the single-flight cycle claim | `POST /v1/org/converge-safety/read` |

### Ownership, and what counts as proof

Socket ownership is separate from the observation. Normal human commands infer
both values from the `runtime_owner` row; the expert override must supply both
`--socket` (the tmux `-L` server name) and the exact session, which is derived
and stored nowhere.

The derivation has exactly one producer, `session_name_for_slug` in
`chief-cli/src/placement.rs`: `format!("org-{slug}{SESSION_TERMINATOR}")` with
`pub const SESSION_TERMINATOR: char = '_'`. The trailing `_` is a terminator,
not decoration, and the reason is measured rather than argued — `tmux -t <name>`
matches exactly first and falls back to PREFIX, so a live `org-acme-corp` once
answered a probe for `org-acme`. The impossibility proof is two facts: a slug is
`[a-z0-9-]` only (`paths::is_canonical_slug` validates, `genesis::slugify`
produces), so it can never contain `_`; and every company session is `org-` +
slug + terminator. If `"org-" + a + "_"` is a prefix of `"org-" + b + "_"`, the
shorter name's terminator would have to be a character of `b`, which fact one
forbids — so `a == b`. **No pair of different companies can collide, for any
slug either could have, with no length limit and no reserved-name list.**
`SESSION_TERMINATOR` is `_` rather than `.`, `:`, `!`, `+`, `-`, `^` or `$`
because tmux refuses the first two in a session name and gives the rest meaning
in target syntax.

**One name that looks like it and is not.** `runtime_session_for_slug` in
`chiefd-core/src/store/organization.rs` returns a bare `org-<slug>` with **no**
terminator. It is the `sessionName` DOCUMENT FIELD on the CEO boot lease, the
launch intent and the quiesce row — never handed to `tmux -t` — and its own doc
says so: *"NOT the client's tmux session name. Do not use it as one … the two
have DIVERGED on purpose."* `organization-intercom.ts`'s
`conventionalRuntimeSession` is its TypeScript twin and re-derives the same bare
string in order to *validate* that field, so the two must agree or lease
validation rejects every lease.

That split is the hazard, and it is guarded rather than remembered.
`scripts/test/tmux-session-name-single-definition.test.mjs` reads the terminator
out of `placement.rs`, names every producer with its job, requires each
tmux-TARGET producer to carry the terminator, and requires the document-field
pair to move together. It exists because the convention change did **not** reach
the shell: the deploy shell (a `deploy-common.sh`, since removed) kept a prefix-collidable
`tmux -t "org-$SLUG"` and a `grep -Fqx "org-$SLUG"` that, after the move, could
no longer match any live session at all — so `assert_company_panes_unchanged`
compared `""` against `""` and passed. A gate protecting Pi's panes across a
daemon hand-off had been checking nothing, silently. **An exact-match probe that
stops matching everything looks exactly like a probe that finds nothing wrong.**

Display names, `$TMUX` and the default server are never evidence.

A request arriving on a different socket audits the *recorded* socket's tmux
server directly (`runtime_lifecycle::observe_prior_ownership` over
`chiefd-core/src/store/runtime_ownership.rs`). A live, correctly tagged session
refuses the request; an untrusted or failed audit resolves conservatively to
"still projecting", so a transient tmux hiccup is never permission to take a
company from another socket. Only a *proved* absence permits an atomic takeover.

The takeover gate rests on that audit alone, and the second input it used to
take is gone rather than dormant. `supervisor_process_state` — whose only writer
was the detached org-supervisor deleted with #825 — had four readers that
outlived it, and their silence was not neutral: it disabled two `audit_ownership`
refusals and one `assert_company_stopped` arm, and pinned `RuntimeLiveness` to
`Stopped`. The five retired supervisor tables (`supervisor_process_state`,
`supervisor_state`, `supervisor_armed_intent`, `supervisor_runtime_events`,
`supervisor_runtime_event_log`) are now **dropped at open** and no longer
declared, held by two tests in `chiefd-core/src/schema.rs`:

```rust
assert!(!COMPANY_SCHEMA_SQL.contains(&format!("CREATE TABLE IF NOT EXISTS {table}(")), …);
assert!(COMPANY_SCHEMA_SQL.contains(&format!("DROP TABLE IF EXISTS {table};")), …);
```

The rule those tests encode is worth more than the tables: *"a dropped mechanism
that still has a table is a mechanism waiting to be re-wired."* No `CREATE` for
any of these names follows the `DROP`, deliberately — dropping and recreating
under one name would erase live state on every daemon boot. The live authority
for "is this company supervised" is `supervisor_watermarks`.

### Serialization without locks

MANDATE 4 bans locks outright. `withOrganizationRuntimeLock`, the `.org.lock` and
`.runtime.lock` files, and the `tmux_writer_lease` that briefly replaced them are
deleted rather than ported — a second mutual-exclusion mechanism over a resource
that already has one is not defence in depth, it is a disagreement waiting to be
observed. Serialization is structural, at three layers, none of which is a lock:

1. **beacond** admits exactly one chiefd per company before its storage opens.
   Registration is a single `BEGIN IMMEDIATE` keyed by the slug, and a slug
   already registered to a live pid is refused rather than replaced. That, and not
   a mutual-exclusion object, is what stops a copied or renamed checkout starting
   a competing daemon.
2. **The writer actor** is one thread running one `BEGIN IMMEDIATE` per mutation,
   and every durable fact a converge pass produces is published in one
   transaction.
3. **`converge_safety::begin_cycle`** holds a durable single-flight claim, so a
   second concurrent pass — attended or duty-driven — is skipped rather than
   queued.

The one mutual-exclusion object left on this path is the CEO boot lease
(`boot_lease`), and it serializes no writes: it fences an attended CEO-only boot's
slow pre-converge phase — provider preflight and materialization — against the
reconcile duty, which is a window no transaction spans.

MANDATE 1 bans polling with the same finality. The two bounded waits that remain —
CEO pane liveness and session absence — are each one `tokio::time::timeout` around
an `await` on a change signal the feed nudges. There is no sleep loop and no busy
retry; the deadline is the only failure bound, and a failed read inside a wait
never rejects it.

## Supervision is duties in one daemon

There is no supervisor process. Launching a company starts exactly one detached
`chiefd run` daemon for it, and supervision is **seven duties inside that one
process**, enumerated once in `chiefd-core/src/store/supervisor_watermark.rs`'s
`Duty::ALL` and bound to their bodies in one registration table in `run.rs`:

| Duty | Declared interval | Scheduling |
| --- | --- | --- |
| `supervision_reconcile` | 30 s | reactive |
| `mailbox_wake` | 30 s | reactive |
| `deadline_evaluation` | 30 s | reactive, next-deadline sleep |
| `reminder_dispatch` | 5 min | reactive, next-deadline sleep |
| `health_monitor` | 5 min | reactive, next-armed-confirmation sleep |
| `background_memory` | 30 s | self-triggered from the change feed |

Those intervals are liveness *expectations*, not wake rates — what the startup
self-audit measures silence against, three windows before it raises a
`supervisor_duty_stalled` incident. Duties are event-driven: a row change wakes
the duty that cares about it, and with nothing to do a reactive duty rests on a
fallback floor rather than spinning. Every duty must be classified as reactive,
self-triggered, or explicitly justified as non-reactive, and a construction-time
conformance check refuses a duty that is in none or more than one of those lists.
The non-reactive list is now empty: its only member was the retired channel's
long poll, whose justification was that the remote side offered no push. Every
surviving duty is reactive or self-triggered, and the empty list stays as the
seam a future non-reactive duty would have to argue its way into.
Each duty folds its `last_success_at` and `run_count` into `supervisor_watermarks`
inside the same transaction as the work it just did, so a duty cannot report
success for a commit that did not land, and a duty added without a watermark is a
duty whose silence nobody would notice. A panicking duty task is restarted rather
than silently vanishing.

Reconcile compares the ownership-audited pane identities against exactly the
people whose `person_activity.last_desired_active` is true. A missing or extra
owned pane invokes the safe reconciler immediately; an exact healthy set keeps the
low-cost path. Reconciler return is not evidence of recovery — a second
fail-closed ownership audit against freshly read activity must match exactly
before a pass may claim health, so an immediately exiting replacement stays a
crash rather than becoming false green. Crash and recovery facts are columns of
the `runtime` row — a fingerprint, when it was observed, and whether the 15-second
confirmation has been met — beside `runtime_recovery_people`, which names each
person the audit found missing or unexpected.

### Goals, assignments, and the recovery ladder

A manager goal is one outcome owned by a CEO or a head, open until that owner
explicitly completes or cancels it. Each manager carries two protected schedules,
`goal_watches` and `manager_check_ins`, both at
`DEFAULT_MANAGER_SCHEDULE_INTERVAL_MS` = **15 minutes**; "protected" is literal —
`validate` refuses any protected schedule whose interval differs from the resolved
company value. After `MANAGER_GOAL_STALLED_REVIEW_LIMIT` = **three** still-open
reviews, a goal escalates once to the parent manager, with `escalated_at` making
that once-per-goal.

Assignments carry one manager, one assignee, an expected output, an absolute work
deadline, a runtime generation and an acknowledgement state, and the whole ladder
is constants in `chiefd-core/src/store/supervision.rs` evaluated by the
`deadline_evaluation` duty in this exact order:

1. **Work deadline** (`work_deadline_at`) — absolute. It fails overdue work even
   if heartbeats are still arriving.
2. **Progress silence** — `ASSIGNMENT_PROGRESS_TIMEOUT_MS` = 5 minutes, evaluated
   only while acknowledged. First expiry spends the one allowed generation
   replacement; a second fails the assignment. There is no heartbeat *cadence*
   constant anywhere in the code — chiefd enforces a silence threshold, and how
   often a worker refreshes it is the caller's business.
3. **Acknowledgement timeout** — `ACKNOWLEDGEMENT_TIMEOUT_MS` = 90 seconds, armed
   at delivery and evaluated only while awaiting an ack.
   `ACKNOWLEDGEMENT_RETRY_LIMIT` = 1 redelivers once under the same assignment id;
   then `GENERATION_REPLACEMENT_LIMIT` = 1 replaces the assignee's generation once;
   then the assignment fails with `acknowledgement_exhausted`.

Both limits are `CHECK`-constrained on the `assignments` row itself
(`ack_retry <= 1`, `replacement_count <= 1`), as is the coherence rule that a
`progress_deadline_at` may exist only on an acknowledged assignment. `effects` is
the ordered outbox with its own per-company sequence and a
`pending|delivered|superseded|failed` status; `effect_payloads` carries the
kind-specific content as bounded scalar child rows rather than JSON;
`ack_receipts` is the inbound receipt queue, drained by key so a replay is a
no-op.

Goal priority orders work and never changes identity. Every manager goal and every
delegated-goal group carries one of `urgent`, `high`, `normal`, `low`, and one
pure comparator ranks them:

```rust
effective_rank = min(3, base_rank(priority) + (min(focus_deferral_count, 12) / 4))
```

with `FOCUS_DEFERRAL_CAP = 12`, ties broken by never-focused first, then earliest
`last_focused_at`, then `created_at`, then id — a total order that reconstructs
identically after a restart. Aging advances once per *committed* evaluation, never
once per missed wall-clock interval. The schema-v1 → v2 ledger normalization the
previous version of this document described does not exist in Rust: `validate`
merely tolerates the number, the row reconstruct stamps version 2
unconditionally, and nothing synthesizes delegated-goal groups from an assignment
id prefix. `priority_mode`'s `legacy-default` is a permitted `CHECK` value that no
loader ever writes.

Reminders are the only recurrence mechanism. A `reminders` row carries the prompt,
interval (floor 60 s), next due time, recurrence flag, fire count and optional
expiry, bounded to 16 per person and a 2,000-character prompt. Firing, re-arming
and the watermark commit together in one transaction; recovery fires once rather
than replaying a catch-up burst. **A reminder never leases its owner**: dispatch
enqueues an effect, the mailbox wake turns it into a durable envelope, and the
"wake" is a targeted reconcile that ensures the recipient's pane exists. The
envelope is durable before any wake, so a wake that never arrives costs latency
and nothing else.

### The passive health monitor

The health monitor never changes agent state. It sleeps to its nearest armed
confirmation deadline rather than on a fixed interval, and an observation must be
seen a second time before it may page, so a single transient never raises. A
tagged pane that has been work-free for `IDLE_PANE_STALE_MS` (**ten minutes**) with
its routine idle-park transition still unreleased becomes one manager-owned
`idle_pane_awaiting_release` incident; the recovery is to release that exact
transition and let normal reconciliation park the pane. The monitor never kills a
pane and never creates a competing transition.

## Activity, transitions and graceful parking

Parking, transfer and offboarding all run through one
generation-fenced `transitions` record. Its statuses are
`awaiting_handoff → overdue → ready` (the open set, enforced by the partial unique
index `transitions_one_active` as a *positive* list, so a future terminal status
cannot wedge a person) and the terminal `applied`, `cancelled`, `forced`.
`intent_id` is nullable by design: `NULL` is an unowned routine idle park, and a
non-null value is an intent-bound transition a routine park may not replace.

**Releasing a transition is the whole commit record, and it carries no payload.**
`ReleaseInput` is exactly three fields — transition id, person id, runtime
generation — and the identity fence is the point: the caller must prove which
transition, who they are, and at which generation, and none of the three may be
claimed rather than injected. A transition belonging to someone else, a stale
generation, or an already-terminal transition refuses; a repeat release of a
`ready` transition rewrites the same status and is a no-op. Only a released
transition may apply, and one staffing call
(`POST /v1/org/staffing/lifecycle`) prepares, releases and applies in sequence, so
a finished person moves immediately. The release is load-bearing: an applied
transition is what sheds launch intent and drives the pane teardown, so deleting
that call would leave a departed person's pane open forever.

**Reflection is deleted from the product.** The `org_reflect` tool and the bounded
handoff payload an agent used to write before parking, benching, transferring or
offboarding are gone; their two tables (`reflection_handoffs`,
`reflection_handoff_items`) are dropped behind an idempotent migration boundary —
items first, because the child carried a foreign key — and the activity status
wire has no reflection key. Nothing in the product may be called a reflection or
pretend one occurred. The state machine itself is untouched and load-bearing; it
simply records that the transition was *released*, never what was said.

A routine idle park is the one transition nothing releases in-call, and it is
bounded rather than retried. Its identity is exact — action `park`, no
`intent_id`, and the reason string `IDLE_AUTO_PARK_REASON` — because several rules
apply only to it:

- `ORGANIZATION_AUTOMATIC_PARK_MAX_IN_FLIGHT` = **2** routine parks may be in
  flight per company at once. A pass admits `2 − in_flight` new ones through the
  durable round-robin cursor in `activity_meta.automatic_park_cursor`, so a
  company that loses many leases at once parks in pairs instead of tearing itself
  down. The CEO is never a candidate.
- `HANDOFF_GRACE_MS` = **two minutes** from request to deadline, after which the
  transition is `overdue`; one further grace window later
  (`ORGANIZATION_AUTOMATIC_PARK_OVERDUE_LEASE_MS`, the same two minutes) the
  admission is made terminal as `forced` — four minutes end to end, and never
  retried.
- `forced` records that *nobody* released the transition and is deliberately not
  `applied`, which asserts that somebody did. It is deliberately excluded from
  `is_released`, and the person's `active_transition_id` pointer is kept so they
  cannot re-enter auto-park candidacy on their own.

An intent-bound or manually prepared transition gets no such rescue: it is swept
to `cancelled` one grace window past its deadline, which is exactly what the
health monitor's ten-minute incident pages on. Non-CEO people also stay resident
for `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS` = **two minutes** after their last
durable demand clears, so a burst of work does not thrash panes, and terminal
transitions are retained to a cap of 200 per company. That is the operator's
stated cap — "two minutes maximum from settle, total" — and the constant already
holds it.

**What an operator actually SEES is longer than two minutes, and the lease is
not the reason.** The park decision is a branch inside the reconcile pass, and
`SupervisionReconcile` is a reactive duty: its nominal 30 s interval is demoted
to `max(interval, DEFAULT_REACTIVE_FALLBACK_FLOOR)` = **60 s** by `supervise`
(`chiefd-daemon/src/run.rs`), so at rest the decision lands up to a minute after
the lease expires and a quiet company settles in up to three minutes. Do not
lower the lease to make the total read 120 s: that corrupts a correct number to
paper over a sampling floor, and it would be wrong on the reactive path, where
the gap does not apply at all. Two related traps, both recorded because each
looks like the knob and is not: a pane that died mid-turn without ever sending
`agent_settled` settles at `AGENT_ACTIVITY_LIVENESS_MS` (300 s) + the lease =
**420 s**, and `org_settings.supervision_interval_ms` exists in the schema and is
projected to callers but is read by no Rust loop, so changing it moves nothing.

### What actually leases a person

Three direct sources, plus the CEO, plus one expansion
(`chiefd-host/src/gather/health_snapshot.rs`):

1. an open manager goal, leasing its manager;
2. an open assignment, leasing its assignee;
3. a pending non-incident mailbox envelope, leasing its recipient.

The CEO is leased unconditionally, and every leased person's operational mailbox
chain is walked upward so the management path stays live while a report's work is
open. **Pi work loops are not a lease source and no longer exist.** The Pi
`/loop` package is deleted, `workspace/.pi/loops/` is gone, the `work_monitoring`
column is dropped from `people` with a schema test pinning its absence, and
durable reminders replaced the session loop it gated. A protected goal review does
not lease, a check-in does not lease, and a pending reminder does not lease.

The activity command surface takes an authenticated person identity from outside
the Pi-controlled payload: `POST /v1/org/activity/command-status` and
`POST /v1/org/activity/prepare` both require a `callerPersonId` the trusted adapter
injects. There is no `/v1/org/activity/release` route at all — release is reachable
only in-process from the staffing-lifecycle handler — and there is no agent-facing
tool for finishing a transition, so an agent is never prompted to complete one.

## Staffing, removal, and what refuses

A person sits in exactly one department, `department_id`, and a transfer moves
it. The column was once the pair `home_department_id` / `assigned_department_id`;
loan was the only verb that could make the two disagree, so when loan was deleted
the pair became one column (#1081). Every staffing verb appends to
`staffing_history`. Offboard *adopts* an already-released offboard transition instead of
superseding it, because superseding would mint a fresh `awaiting_handoff` row with
nobody left to release it. The plain verb leaves the launch-intent fence up for the
handoff window; the unattended variant withdraws it in the same transaction,
because a person with no runtime generation could never complete that handoff and
the fence would hold a departed person's pane open forever.

Recursive unit removal is one guarded transaction, not a journal. It refuses the
root unit — remove the company instead of its executive root. A pure preview
computes the same blast radius before
commit, exposed as `POST /v1/org/unit/removal-impact` and
`/v1/org/unit/removal-preview`; the tools require an explicit `confirmImpact`.
The commit OFFBOARDS, per person homed in the subtree, through the same
`depart_person_rows` an ordinary offboard composes: desired-off,
`employment_state → departed`, re-homed to the removed subtree's PARENT (which
the root refusal guarantees exists and which is never inside its own child's
subtree), `staffing_history 'offboarded'` recording the unit they left, and
their open assignments released. A head of a now-deleted department is demoted
to `worker`. It then deletes the subtree's departments and restores the
department and people ordinal bijections.

It deletes no person. Removing a unit and firing a person are the same act the
product calls "fires" on both surfaces, so they may not disagree about what
firing durably means, and `org_offboard` retains the record deliberately —
`staffing_history` carries no people FK precisely so a person's ledger outlives
them. The hard delete this replaced therefore did not erase the history, it made
it wrong: an orphaned `hired` row with no `offboarded` row and nobody it belongs
to. Two accessors died with it, `organization_rows::delete_person` and
`activity::rows::delete_person_state`; there is deliberately no named verb left
for erasing a person. The launch-intent fence still goes immediately here — the
unattended shape — because the department a leaver would hand off to is deleted
in the same transaction.

Two things the previous version of this document described as live machinery
were never implemented, and both are now deleted rather than left described. The
`unit_removals`/`unit_removal_members` journal and its
`planned → manifest-committed → runtime-reconciled` phases were a `CHECK`
constraint and a TypeScript type with no producer and no consumer in either
language; the tables are dropped behind an explicit idempotent migration at open
and the types are gone. Removal was never journalled in practice:
`remove_department_tree` is one guarded transaction, a second removal of the same
unit returns `unknown-department`, and retry idempotency is implemented one layer
up in the intercom, which re-reads the manifest and treats an absent unit as
success. Whole-company removal's two-phase `PREPARE/QUARANTINE/FINALIZE` journal
is likewise retired and its four tables dropped.

**What replaced it is `chief rm`, and it is a client-side sequence rather than a
journal.** `chief-cli/src/remove.rs` confirms, stops the runtime and the daemon
through the same `stop_runtime` `chief stop` uses, deletes the store database
(with its `-wal`/`-shm` sidecars) and the Pi-artifact tree, and drops the beacond
row **last** — recorded as data in `REMOVE_ORDER` so the ordering cannot be
reordered silently. The row goes last because a beacond row with no company
behind it is a state the product can name and finish (`chief ls` shows
`missing`, `chief rm` completes it), while a company with no row is
unreachable. There is deliberately no preflight, and no hidden quarantine
rename: the only surviving use of the word "quarantine" in the Rust is the
unrelated stray-object quarantine in the client's planner.

## Capability plans

A person's plan is the `person_tools` and `person_prompts` rows. There is no
resource half: `person_resources` and the whole catalog it was validated
against are deleted (chief-home-is-cwd §3/§4e). **Chief does not manage skills.**
An agent's skills are the Markdown skills in the company's own library at
`<dir>/.chief/skills`, and exactly ONE of them is installed per person — the
role skill — through a symlink written into their PROJECT scope at
`<dir>/.chief/agent/<id>/.pi/skills/<role>`. Pi does everything
that used to happen here: it discovers them, parses their frontmatter, enforces
the Agent Skills name rules, dedupes by realpath and reports a name collision
with a winner and a loser. Nobody selects a subset, so nothing validates one,
and a company's skills change by editing that directory — every agent's next
session sees it, with nothing to store, propagate or invalidate.

Extensions do not follow the symlink rule: the org extensions are chief's own
shipped code and the set must be exact, so they reach a pane as
`--extension <path>` argv per spawn. Pi packages are Pi's own package manager's
business.

### Who gets what

The TOOL grant is the whole of it, and it is still chief's decision: composed
per person by `converge_apply/resource_catalog.rs::person_tool_names` and passed
as the pane's `--tools` allowlist.

- **Baseline for everyone** — the Pi builtin floor
  (`read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`) plus the active
  runtime family: `org_send` with a real `to: "all"` broadcast, `org_roster`,
  and `org_create_reminder`/`org_list_reminders`/`org_stop_reminder`.
- **Managers** (`executive` or `head`) additionally receive the manager tool
  family, `org_delegate` among them, plus the staffing and unit-lifecycle
  tools. A head's authority is confined to their own recursive subtree; the
  CEO's covers the company.
- **The executive only** — not a head — receives `org_escalate_to_operator`.
- **Everyone** holds the subtree tools, which refuse anything outside the
  caller's own subtree, so growing the organization downward takes authority
  over nobody.

Work-loop monitoring is not in that list because it no longer exists. Several
comments in `converge_apply/resource_catalog.rs` still mention "loop tools for
monitored workers"; the code beneath them grants none.

`workspace/AGENTS.md` is the single on-disk projection of `person_contracts`,
deliberately singular after an earlier four-copy fan-out let one text drift apart.
The text is generated in `chiefd-core/src/store/agent_contracts.rs` and branches
on person kind: a manager contract (delegate, staff from the roster, verify,
unblock, route engineering and release work to their owners, never run a CLI from
a shell) with the un-removable protected check-in section appended, or a worker
contract (own one output, surface blockers early, submit the final result exactly
once through `org_send` with `completeAssignment`, stay inside the mandate).
Rendering is fail-loud: a company with no committed contracts, or a person with no
committed contract, refuses to materialize rather than writing an empty
`AGENTS.md` over a real one. The `md5` column is the rewrite fingerprint — equal
digests mean no rewrite, which preserves the file's mtime and therefore
extension-drift detection. Each person also gets a generated `<person-id>-role`
skill whose body is "Read `AGENTS.md` first."

### Model and thinking

`people` carries `provider`, `model`, a required `model_reason` and an optional
`thinking`, with a default level of `low`. `thinking_reason` and the `CHECK`
that made the elevated levels require it were deleted in #1139: permission is
the gate, and a justification string nobody reads is not one.

**The provider is general, not a fixed route.** `PI_BUILTIN_PROVIDERS` is a
35-entry list of Pi's native providers, and `openrouter` is one row in it rather
than the destination. A non-native provider is routable as long as the operator's
root registry declares a complete transport contract; otherwise materialization
refuses with the unroutable-provider code. The two credential channels are
deliberately separate: a native provider gets exactly its own entry projected into
the person's mode-0600 `auth.json`, while a custom provider travels as the
non-secret `ORG_CUSTOM_PROVIDERS` contract in the pane argv plus a mode-0600
`.provider-credentials.json` the extension reads inside the Pi process.

A live model change is two-phase and durable. `model_change_preparations` records
the expected provider, model and runtime generation alongside the selected route
and a request hash; chiefd prepares, Pi applies, and chiefd commits only while
that route and fence are unchanged. A failed apply records terminal audit state
(`status IN ('prepared','applied','failed')`) without moving the person's durable
route.

There is no provider allowlist in the code, and its absence is a decision rather
than a gap. The change path validates identity, active employment, the runtime
generation and the prepare/outcome phase, and nothing else, so a company cannot
restrict which providers its agents may switch to. `877b07c3` removed that second
fence deliberately — the policy module, both enforcement points and all three
projection sites — on the reasoning that an agent owns its own model and that the
real precondition on a vendor is a transport contract (`baseUrl` and `api` in the
operator's root Pi catalog), not a policy list. The gate was deleted rather than
defaulted to allow-all because `provider.env` is written with flag `wx`: a stale
allowlist baked into a long-lived pane would otherwise outlive every redeploy,
whereas with no reader left it is inert.

## What authenticates a call today

This section is deliberately blunt, because the gap between the designed mechanism
and the wired one is large enough that describing the design as live would be a
security claim this product does not honour.

**Two producers build a managed process's environment, and which one owns which
variable is the boundary in miniature.** chiefd publishes only the facts it can
observe, in the launch catalog (`chiefd-host/src/converge_apply/cycle.rs`):
`ORG_LAUNCHER_ORGANIZATION`, `ORG_LAUNCHER_PERSON`, `ORG_LAUNCHER_ORG_DIR`,
`ORG_LAUNCHER_DATA_ROOT`, `ORG_LAUNCHER_ROOT`, `PI_CODING_AGENT_SESSION_DIR` and `HOME`,
plus `TEAM_LAUNCHER_BUN`, `BEACOND_URL` and
`ORG_LAUNCHER_RELOAD_HARD_CONTRACT` when each is present.

The client adds the rest at spawn (`chief-cli/src/actuate/spawn_cmd.rs`'s
`launch_command`): `COLORTERM`, `ORG_LAUNCHER_RUNTIME_SOCKET`,
`ORG_LAUNCHER_RUNTIME_SESSION`,
`ORGANIZATION_RUNTIME_GENERATION`, and `ORG_CUSTOM_PROVIDERS` when the person has
one. The socket and session used to arrive inside the catalog, and moving them
was not tidying: *"that made the backend assert a placement fact it cannot
observe and this client derives independently … They are injected HERE now, from
the socket and session this actuator is actually driving."* An in-pane
attestation wrapper — also the client's — then adds `ORG_LAUNCHER_PANE_ID` from
tmux's own `$TMUX_PANE`, refusing outright if it is missing or not `%<digits>`,
before `exec`ing the agent.

A person hosted by the API rather than by tmux gets a third, separately built
environment (`converge_apply/api_host_profile.rs`): `CHIEFD_LAUNCH_MODE`,
`COLORTERM`, `HOME`, `ORG_LAUNCHER_DATA_ROOT`, `ORG_LAUNCHER_ORGANIZATION`,
`ORG_LAUNCHER_ORG_DIR`, `ORG_LAUNCHER_PERSON`, `ORG_LAUNCHER_ROOT`,
`PI_CODING_AGENT_SESSION_DIR`, plus optional `NODE_EXTRA_CA_CERTS` and
`BEACOND_URL`. No runtime socket, no session, and no chiefd address — pinned by
`chiefd-api/tests/api_host_launch_profile_http.rs`, which asserts the wire
carries none of `ORG_CHIEFD_URL`, `ORG_LAUNCHER_RUNTIME_SOCKET`,
`ORG_LAUNCHER_RUNTIME_SESSION` or `OPENROUTER_API_KEY`.

A protected `org_*` tool call carries those identity fields in the request body
over loopback HTTP, and each route re-reads the person's committed runtime
generation and refuses a mismatch. That is a staleness fence, and a good one; it
is not proof that the caller is the person named. **Until #751/P7, nothing else
was.** The paragraphs that used to stand here described a full pane-attestation
design — bearer token, `SO_PEERCRED` uid, `/proc` ancestry to the claimed pane,
agreement with the pane's tmux tags — and then said, correctly, that it was
unwired: `CHIEFD_BEARER_TOKEN` appeared only at its own declaration, the pane
authenticator had no caller outside its own module, the extension attached no
credential, and the JWT stack was off by default. It is worth recording that the
document reached that finding by checking rather than by reading the design,
because the design read as live.

P7 resolved it in the only direction the client-agnostic mandate allows: the
pane-attestation design is **deleted** rather than wired, and the cryptographic
one is wired rather than described.

**What authenticates a call now.** Each person has a P-256 private key. The
Chief key is `<dir>/.chief/chiefd-identity.key.pem`; each non-Chief key is
`<dir>/.chief/agent/<id>/chiefd-identity.key.pem`. The host creates the key once
and enrols its public half into the company's `identities` table. A pane signs a daemon-issued challenge with it
(`POST /v1/auth/challenge` then `/v1/auth/token`) and presents the resulting HS256
bearer on every chiefd call, through the one transport `organization-intercom.ts`
constructs. Every `chiefd run` now builds that issuer — there is no longer a mode
in which a company serves with no way for anyone to prove anything — and the
verify-middleware checks any credential that is presented, refusing a bad one in
every stage.

**What the token is bound to.** Its `sub` is the identity, its `kid` is that
identity's current key fingerprint (so a rotation kills every token it ever held),
and — for a person — its `gen` is the runtime generation it was minted in. The
middleware compares `gen` against the person's CURRENT generation, so a copied
pi-home stops working at the next respawn or fresh session rather than
authenticating indefinitely. An unreadable generation denies rather than matching:
"this person has no incarnation" and "chiefd could not read the ledger" are
different facts and only the first is a real answer.

**What is still not enforced, deliberately.** Requiring a bearer on EVERY route
remains behind `CHIEFD_AUTH_ENABLED`, because `apps/web`, the operator CLI's own
HTTP client and the proof scripts hold no credential yet; turning it on would
refuse them. One route family requires a credential regardless of that switch:
`/v1/org/model/change` and `/v1/org/thinking/change`, which are fenced to the
person they name and whose only callers are the two agent tools. Everything else
still leans on the daemon binding loopback and beacond admitting one daemon per
company. `/v1/org/caller/authorize` stood beside them and is DELETED: its only
decision was a job-title classifier over CLI verb names, and with that gone it
answered "authorized" to every authenticated person — a guarantee in shape and
nothing in substance, with no client to read it.

## Where tmux lives, and how that is kept true

A standing architecture mandate says chiefd is the backend and the backend is
client-agnostic: it must not know about tmux and must not know about the web.
The one-line rule is "chiefd decides WHO runs; the client decides WHERE it is
displayed". **The mandate now holds**, and this document describes today.

It is measured rather than asserted.
`scripts/test/backend-tmux-boundary.test.mjs` scans every `.rs` file under
`chiefd-core`, `chiefd-host`, `chiefd-api` and `chiefd-daemon` and asserts that
no file names tmux **in code**, with **no exception list at all** — the 107-row
violation register it used to check against was deleted with the last violation
(#751/P10), because a register that can outlive its subject is the same rot as
an allowlist a file move orphaned. "An allowlist can only get less wrong; no
allowlist cannot get wrong."

Read the scope exactly, because it is narrower than "the backend never says the
word". Comments are stripped before the count, so a comment-only file passes and
is reported under `filesNamingTmuxInCommentsOnly` rather than failed. That is
deliberate: the backend carries tombstones explaining why tmux left, and
deleting them would delete the only record of the boundary's reason. So a grep
for `tmux` in a backend crate **will** return hits, and none of them is a
violation. The guard also checks the dependency direction both ways
(`backendCratesDependingOnCli`, `clientCratesDependingOnBackend`), refuses a
chiefd crate that carries tmux and is neither a scan root nor a declared client
(`cratesOutOfScope`), and fails a scan root that resolves fewer than ten tracked
`.rs` files (`blindScanRoots`) so it can never pass by seeing nothing. A
separate test asserts `chief-cli` still carries tmux, so the boundary always has
a live subject.

Where each concern went:

- **Placement is the client's.** The window-per-department rule and the session name
  `org-<slug>_` are computed in `chief-cli/src/placement`; both sides meet in a
  golden fixture rather than in a shared struct. The roster-facts route
  deliberately does not serve `paneDepartmentId`: it is a decision rather than
  data, and it is stale between reconciles, so a client that derives the window
  from the department tree is strictly more correct than one handed a stored
  answer.
- **Actuation is the client's.** `chief actuate <company>` observes its own
  tmux, posts the observation to `POST /v1/org/runtime/observed`, reads the
  person-scoped actions chiefd publishes at `POST /v1/org/runtime/actions`,
  fetches `POST /v1/org/runtime/launch-catalog` for the other half of a start,
  and applies. The action stream is computed at read time and never stored: a
  stored copy is a second answer to "what should happen now" that goes stale
  between passes. The classification of tmux's literal stderr strings —
  provably absent versus merely unproven — lives in
  `chief-cli/src/actuate/trust.rs`, and it is the fail-closed invariant of the
  whole system.
- **The observation carries people, never panes.** `ObservedPerson`
  (`chief-cli/src/actuate/report.rs`) is a person id, a generation, liveness, an
  optional pid and an optional start time — and no pane id. A dead entry is
  *reported* rather than dropped, "so chiefd can tell 'I looked and this is
  gone' from 'I did not look'." A pane id exists only so the client's own
  interpreter can name a target.
- **The `runtime` row publishes person → process handle.** Its `panes` map is
  keyed by PERSON, and the value is the pid the actuator reported as a string,
  or the **empty string** when the actuator proved the person alive without
  reading one. Every backend reader takes only the KEYS —
  `reconciler_facts.rs` calls `panes.into_keys()` — so the observed SET, which
  is the load-bearing half, stays exact while the value stays "the strongest
  true statement chiefd can make about a person's process". Read the empty
  string as *no pid was readable*, never as *no process*. The `windows` map
  beside it is deleted, not emptied: it was department → tmux window id, a
  display grouping the report never carried, and a map that is always empty is
  a dead mechanism rather than a fact. `runtime_windows` is dropped at open.
- **Caller authentication no longer reads a pane.** Until #751/P7 chiefd
  verified a managed caller by walking pid ancestry to the tmux pane it claimed
  and matching the tags stamped on it. A client-agnostic daemon cannot see a
  pane, so that proof is gone; a caller now signs a daemon-issued challenge
  with the P-256 identity key materialization wrote into its own pi-home.
- **A company with no attached client is un-actuated.** This is the consequence
  that must not be discovered late. Before P8 a person's Pi process was the
  child of a pane the daemon made, so somebody was always able to start a
  person. Now the client makes the pane. `ActuatorPresence` is therefore a
  first-class published state — `never-attached`, `present` or `lapsed` — and
  an empty action list always says why. `WithheldReason` has four variants, and
  the doc comment on it is the design: *"Every variant is a state, reported on
  the response. None of them is an error, and none of them is silence."* They
  are `no-actuator`, `observation-untrusted`, `breaker-tripped` (three
  consecutive failed apply cycles; only an explicit operator clear resumes) and
  `shadow`. A `lease_ms <= 0` never grants presence.

The one deliberate exception is `POST /v1/org/runtime/launch-catalog`, which
chiefd serves rather than surrenders: its core is a fail-closed read of the
daemon's own on-disk materialization state, and it stages each person's
provider credential. Materialization is the daemon's job, so the catalog is
published, not moved. Revisit only if that reasoning stops holding.

## Where Pi is, and what is never spawned by a bare name

There is **one** answer to "where is Pi?", and it is `resolve_pi_runtime` in
`chief-cli/src/preflight.rs` — a pure function whose every effect is a
parameter. Three rungs, in this order:

1. **The operator's pin**, the environment variable `TEAM_LAUNCHER_PI`, taken
   VERBATIM. It is not canonicalised, deliberately: canonicalising a relative
   pin here would silence the operator error `PreflightCode::PiNotAbsolute`
   exists to name.
2. **The recorded checkout's own pinned build**, `node_modules/.bin/pi` under
   the checkout root `ORG_LAUNCHER_ROOT` names. `bun run release` writes that
   root into a one-line plain-text file under `~/.chiefd`, whose name is spelled
   in exactly one place (`chief-cli/src/paths.rs`). Rung 2 delegates to
   `founder_pi::pi_binary` rather than re-deriving the path, "so the two cannot
   drift apart again" — they had already drifted once, which is
   how Founder started and `chief attach` refused on the same host.
3. **`PATH`**, last, and only when the first two answer nothing.

Every rung must also answer `--version` before it is accepted, so a path that
exists but cannot run is not an answer.

**The resolved Pi travels as a spawn argument, never as an inherited variable.**
`attach` resolves it in its own process — the one whose environment the
preflight actually measured — refuses a non-absolute result by name, and hands
it to the daemon as `CHIEFD_PI_BINARY`. A test pins the decision:
`the_pi_runtime_is_not_something_this_list_has_to_remember` asserts the
actuator's forwarded-environment list does not contain that variable, "because
the pane's pi binary is passed to the daemon, never inherited from a tmux
server". Panes run under a tmux server with a different `PATH`, and that is the
whole reason.

`scripts/test/spawn-program-absolute.test.mjs` holds both halves — **no bare
name** (a program literal a shipped process hands to a spawn must be absolute or
carry a register row with a written reason, an ISO date and a real detector) and
**one resolver** (no two production files may answer one product question). It
pins the resolver by file: `pi-pin-env` to `chief-cli/src/preflight.rs`,
`pi-checkout-path` to `chief-cli/src/founder_pi.rs`. Four registered exceptions
remain: three `tmux` rows and one `curl` row.

State its coverage boundary rather than trusting it further than it reaches. The
guard decides **literals**. A program that arrives as a variable — the pane's
`pi_binary`, which comes over the wire from chiefd — cannot be decided
statically, and is enforced at runtime instead: `chiefd` refuses a
non-absolute `--pi-binary`, and
`chief_cli::actuate::launch_catalog::LaunchCatalog::resolve` refuses a relative
`piBinary` by name before it can become argv.

## What the conformance corpus checks, and what it does not

The corpus is 229 JSON fixtures under `conformance/fixtures/<family>/`, and
**Rust is the runner**. There is no TypeScript harness: `run-ts.ts`,
`record-ts.ts`, `lib/` and `scenarios/` are dead code, and `lib/durable.ts` does
not parse at all — a merge kept both branches' `healthy()`, leaving an unclosed
`async function` with two exports nested inside its body.

Read the coverage in two halves, because they are not the same kind of check:

- **`tools` (137 fixtures)** records an HTTP seam — `tools.chiefd_calls` is the
  ordered `{path, body}` a tool posted — and a replayed one is posted at the
  **real axum router**, built by `router_with_supervision_live` over a real
  SQLite docstore and driven with `tower::ServiceExt::oneshot`. The only
  injected substitute is the caller identity the auth middleware would have
  resolved. **Only the 16 fixtures named in `REPLAYED_IN_RUST` are actually
  replayed**; the other 121 are quarantined, and
  `conformance_tools.rs::every_tools_fixture_is_replayed_in_rust_or_named_as_blocked`
  is the accounting that stops that number being forgotten rather than a claim
  that it is small.
- **`activity` (19), `assignment` (29) and `session-maintenance` (44)** replay
  against `chiefd_core`'s store **in process**. No HTTP is involved. A green
  run of these proves the store, not the route.

**The corpus machine-checks the chiefd half only, by design.** Argument
canonicalization, card rendering and the exact `message` sentence belong to
`packages/piing/extensions/organization-intercom.ts`, and
`packages/piing/test/toolcontract/OrganizationToolContract.test.ts` drives all 47
registered tools against a real daemon and asserts them.
`scripts/test/conformance-fixture-subject.test.mjs` makes the split enforceable:
a fixture that records `tools.chiefd_calls` is claiming a transport, and its own
name must appear in a Rust runner, so a transport claim with no subject fails on
the commit that adds it.

**There is no recorder, and a fixture is not machine-recorded.** The old
`bun conformance/record-ts.ts` cannot run. What replaced it is stronger in one
respect and weaker in another: a fixture's chiefd half is re-recorded by running
its Rust runner with `cargo test -- --nocapture`, which posts the recorded
request at the real router and prints `SERVED <fixture> <answer>`, so the value
comes off the live route and — unlike the old recorder — cannot agree with
itself, because it asserts against the fixture in the same pass. But that only
covers the fixtures a runner replays. **Every fixture written or edited since
the harness broke is hand-authored**, with nothing mechanically tying it to what
the product emits. The fixture key that recorded the argv a tool spawned
`apps/cli` with is deleted (#1044): the read could only ever answer `[]`, so of
the 74 fixtures carrying it, 38 pinned a non-empty argv and were false and 36
asserted the `[]` and could not fail.

## A person hosted by the browser

`apps/web` was the first proof that a client-agnostic chiefd API is achievable,
and it is now a full second host rather than a viewer. It names no tmux socket,
no session and no pane id anywhere in `apps/web/src`; its `PaneDescriptor.paneId`
is documented as "the personId this pane renders", and every occurrence of the
word tmux in that tree is prose explaining what the *other* client does. Window
names are rendered exactly as chiefd serves them, because the rule that rewrites
`.` and `:` exists only because tmux refuses them and it lives beside the tmux
command in `chief-cli/src/actuate/`; a second copy in the browser could only
disagree with the real actuator.

**The whole granted tool surface, not a subset.** chiefd's grant arrives on the
API-host profile, and `ExtensionTools.ts` adapts Pi's `ExtensionAPI`
registrations into `AgentTool[]` with no filter at all — *"a `continue` in this
loop would be the second source of truth this file exists to avoid."*
`selectTools` then walks the granted ids in chiefd's own order, refuses a shadow
of a built-in outright, and returns anything it could not build in
`ToolSelection.unavailable` rather than dropping it silently. A CEO's grant
currently sums to 60, but read that number as arithmetic and not as a contract:
it is derived in Rust from `person_tool_names`
(`converge_apply/resource_catalog.rs`) and **no test or constant anywhere pins
it**. The number that *is* pinned is a different one —
`ORGANIZATION_PROTECTED_TOOL_NAMES: [&str; 49]`.

**The lifecycle is driven, not sampled.** The intercom subscribes to
`mailbox/<personId>`, `session-maintenance` and `supervision` on chiefd's
`GET /v1/docs/watch` and drains on a `doc-change` frame — the same wake the tmux
process rides, over one multiplexed streaming `fetch()` per `url|slug` for the
whole process. A second driver is the harness's own event subscription. The
suite pins the absence of the alternative: *"wakes a person on a mailbox
doc-change, with no settle and no timer"*, asserting `setInterval` was never
called. `org_events` is not that channel — it is chiefd's internal per-slug seq
fence with no wire surface of its own.

**Context accounting, compaction, and session replacement are real.**
`ContextUsage.ts` measures the live branch and returns `tokens: null` when the
newest compaction has no usage after it, so "not yet measurable" never renders
as zero. The threshold is `shouldCompact` with `DEFAULT_COMPACTION_SETTINGS`,
both imported from `@earendil-works/pi-agent-core` — apps/web owns no compaction
constant of its own. Compaction is evaluated at `agent_settled`, and a pending
session replacement outranks it, because "compacting a transcript that is about
to be abandoned spends a summary on history nobody will read again."
`replaceSession` (`AgentHost.ts`) mints a fresh mode-0600 `.jsonl`, writes the
marker entry, **awaits** the outgoing lifecycle's `shutdown('new')`, and only
then restarts. It is served at an idle boundary only, one at a time, and refuses
with "Native session replacement was skipped because the person left its idle
boundary".

**The runtime snapshot has an HTTP surface.**
`GET /api/companies/:slug/people/:personId/runtime` reports how a person is
running right now, and `POST` on the same path states how they should run — one
route, because they are the same subject from two sides.
`personRuntimeReading` reads the snapshot the lifecycle already holds
(`lifecycle.contextUsage()`) and never recomputes it, so the reading a caller
gets is the one the host is acting on.

A company and a department can both be created from the browser. `POST
/api/companies` answers as `text/event-stream`, because genesis is multi-phase
and a single JSON response could only report the last phase.
