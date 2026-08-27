# Two-implementation store audit (#81)

Every durable store that exists on BOTH sides of the chiefd migration, with the
question that actually finds the bug: **who WRITES it, on each side?**

Four bugs on 2026-07-24 were one shape — chiefd migrated a store and one side
stayed behind (#79 mailbox, #67 supervision, #37/#442 documents vs
org_documents, #85 generation semantics). This is the sweep that catches the
rest before they fire.

## Read this first: the two physical stores, and the key that hides rows

There are **two different physical tables**, and confusing them is most of the
danger:

| | reached by | keyed by |
|---|---|---|
| native ledger `documents` | `Ledgers::put_document` / `document_body` / `remove_document` (`chiefd-core/src/ledger.rs:342,362,372`) | store name alone, inside a per-company `Ledgers` working set |
| shared `org_documents` | `chiefd_core::store::mutate_org_document` (`store/mod.rs:503-533`); what the TS `durableStore()` writes and `/v1/docs/*` serves | `(slug, store)` |

**The row key is composite and this is the false-absent trap.** On the TS side
the slug is always `documentKey(slug, root)` = `<slug>@<first 12 hex of
sha256(dataRoot)>` (e.g. `cobalt@a1b2c3d4e5f6`). Rust re-implements the
identical algorithm (`store/legacy_facts.rs:115-124`, used at `run.rs:1721,1737`).
**A query with a bare slug (`"cobalt"`) returns 0 rows** — which reads as
"absent" and is how #87 produced a confident false negative. Always compute the
composite key, and always state which TABLE you queried.

## Verdicts

| store | writers | verdict |
|---|---|---|
| `memory-review` | Rust ×3 + TS ×1, all on the same `org_documents` row and the same CAS | **SAFE** |
| `session-maintenance` | TS only — the Rust module has **zero production callers** and targets the OTHER table | **SAFE BY ACCIDENT — loaded gun** |
| `launch-intent` | TS only; chiefd reads it through a documented read-only bridge | **SAFE — but a dead native writer targets the other table** |
| `provider-admission` | TS only — the whole Rust admission surface is dead code | **SAFE** |
| `health` / `health-monitor` | BOTH sides collect and write, different table AND different name, no gate | **HAZARD + trap door — #93** |
| telegram INBOUND | chiefd writes into `org_documents` directly — a real bridge | **SAFE (the model)** |
| telegram GATEWAY STATE | chiefd writes native, the operator doorbell reads the frozen `org_documents` row | **HAZARD, possibly LIVE — #92** |
| `activity`, `supervision` | — | already boarded: #37/#442, #67 |
| mailbox | — | already boarded: #79 |

### 1. `memory-review` — SAFE

One shared `org_documents` row, one key algorithm, every writer on the same
generation-CAS loop.

- Rust writers: duty-#6 claim `chiefd/src/run.rs:782-804`; worker lease verify
  `chiefd/src/memory_worker.rs:649-661`; worker terminal commit
  `memory_worker.rs:688-696`. Sibling row `memory-records/<personId>` at
  `memory_worker.rs:488-489` (`store/memory_records.rs:62-64`).
- TS writer: exactly one path — `mutateMemoryReviewEntry`
  (`extensions/organization-intercom.ts:2521-2534`), producer
  `queueBackgroundMemoryReview` (`:1860`, called at `:11824`).
- Same store name, same key shape, one row per company with a
  `people[<personId>]` map.
- `store/memory_review.rs:14-23` documents an EARLIER revision of exactly this
  hazard (the row was once in the native ledger, starving the Rust claim) —
  already fixed. Worth reading as precedent.

**Resolved:** the unimported `org-memory-review-store.ts` façade and its
whole-document publish route were deleted. The copied extension now sends only
the named person's semantic patch to ChiefD.

### 2. `session-maintenance` — SAFE BY ACCIDENT, and it is a loaded gun

Single writer today (TS), so nothing is broken right now. But:

- The entire **2521-line** `chiefd-core/src/store/session_maintenance.rs` has
  **zero production callers**. Its only non-test references are `store/mod.rs:49`
  (the `pub mod`) and `store/mod.rs:129` (a polarity-registry entry). The
  `maint.*` verbs appear only in
  `chiefd-core/tests/conformance_session_maintenance.rs:337-487`.
- Its writers (`put` at `:900-907`, mutators at `:818`/`:853`, clear at `:897`)
  target the **native `documents`** ledger. TS writes **`org_documents`**
  (`src/organization/org-session-maintenance.ts:183`, mutating at `:404`,
  `:483-492`, and every verb from `queueSessionMaintenance:570` through
  `finishSessionMaintenance:918`).
- **There is no bridge.** `legacy_facts.rs` bridges runtime-owner,
  ceo-boot-lease, supervisor-state, runtime, launch-intent, supervision
  generations, mailbox pending and reflection-memory — *not* session-maintenance.

**So the moment anyone wires a `maint.*` RPC, both sides write disconnected
rows in different tables** — #79's shape exactly, in the subsystem that owns
fresh-session and compaction. The wiring will look correct, the tests will pass,
and the two sides will silently disagree.

### 3. `launch-intent` — SAFE now, same latent trap

- Sole writer is TS `src/organization/org-launch-intent.ts` (`:161`, `:190`,
  `:214`), called from the CLI, supervision transport, runtime, staffing
  lifecycle and units.
- chiefd reads it through the **documented read-only bridge**:
  `legacy_facts::read_launch_intent_person_ids` (`legacy_facts.rs:388-397`, key
  `"launch-intent"` at `:101`, composite slug via `document_key` at `:115`),
  wired at `converge_apply/cycle.rs:743-744`, projected `:750-760`, consumed as
  the fence at `:644`, installed from `chiefd/src/run.rs:1418`.
- Both sides validate the same body invariants (`organization == slug` AND
  `sessionName == tmuxSession`).

**Latent risk:** the native `launch_intent::add`/`clear`
(`store/launch_intent.rs:206,215`) write the native `documents` table, which
nothing bridges back. They have no production caller today. If one is ever
added, chiefd's own converge cycle would read a fence from `org_documents`
while writing one into `documents` — and launch-intent is the **mutual-exclusion
fence**, so the two sides would disagree about who is allowed to run.

### 4. `provider-admission` — SAFE, because the Rust half is entirely dead

- Rust writers exist (`store/admission.rs`: `acquire` :371, `release` :480,
  `reclaim` :495, `write_config` :191, `clear_config` :213) and have **no
  production callers** — only `tests/tier0_stores.rs`, `tests/polarity_matrix.rs`
  and a type reference in `polarity.rs:376`. The wire verbs `provider.reserve` /
  `provider.release` are declared (`wire/mod.rs:255,259`, auth allowlist
  `auth.rs:76-77`) but **no server handler dispatches them**.
- TS is the sole live authority: `org-provider-admission.ts:302` (acquire) and
  `:383` (release), driven from `cli.ts:965-1004`, invoked by the pane at
  `extensions/organization-intercom.ts:11385` / `:11342`.
- Worth recording for #59/#60 lore: **capacity does not come from either
  durable config on the live path.** TS reads it from the env
  (`organizationProviderPoolConfig`, `org-provider-admission.ts:97`) and chiefd
  only plumbs that env value into spawned panes
  (`converge_apply/spawn_cmd.rs:94-97`). Since `provider-admission-config` in the
  native table is written by nobody, **an operator setting it via chiefd would be
  a silent no-op** — not silent loss, but silently ineffective, which is its own
  trap.

### 5. `health` vs `health-monitor` — HAZARD, filed as #93

Both sides collect and write **independently**, into a different table AND
under a different name, with no bridge and — unlike telegram — **no ownership
gate**, so both run today. chiefd's half is written but read by nothing that
acts: no alert, no card, no escalation, only its own next cycle. The TS half is
what reaches humans.

**The trap door:** adding `chiefdOwnsHealthDuty()` — the obvious tidy-up,
mirroring the telegram gate that already exists — would silently stop every
health incident from reaching the operator. Nothing would error.

### 6. telegram — the same subsystem contains both the MODEL and the HAZARD

- **Inbound envelopes: SAFE, and this is the shape everything else should
  copy.** chiefd writes them **directly into `org_documents`**
  (`chiefd/src/telegram_delivery.rs:101-150`, wired `run.rs:1736-1748`) with
  `document_slug = legacy_facts::document_key` byte-identical to TS's
  `documentKey`, and the code says why: *"the SAME file the mounted docstore
  surface serves — one store, not two."* Store name
  `telegram-inbound/<personId>` — note the **slash**, matching on all three
  sides. Redelivery is idempotent; a conflicting same-id body refuses rather
  than overwrites.
- **Gateway state (cursor + contacts): HAZARD, filed as #92, possibly LIVE.**
  Same store NAME on both sides — a name-only check reads as "migrated
  cleanly" — but chiefd writes the native table while the operator doorbell
  (`org-operator-escalation-notify.ts:96-97`) reads contacts from the now-frozen
  `org_documents` row. With chiefd owning the duty, a chat that first messages
  the bot after cutover never appears in the map the doorbell reads, and it
  short-circuits at `:152-153` with no recipients.

**The asymmetry is the lesson**: one team bridged inbound properly and the
gateway state beside it was left behind. Migrating "a store" is not one
decision — every ROW SHAPE in it needs its own answer.

## The patterns worth naming

**Loaded gun** — mechanical to check for:

> A chiefd-native store module whose writers call `Ledgers::put_document`
> (native `documents`) for a store name that TypeScript writes into
> `org_documents`, with no entry in `legacy_facts.rs` bridging them.

Today: `session_maintenance.rs` and `launch_intent.rs`. Neither fires because
neither has a production caller; both fire the moment someone adds one, and the
addition will look entirely reasonable in review.

**Trap door** — the same shape, one step further along:

> A store where the two sides already both write, chiefd's half reaches no
> consumer that acts, and the natural tidy-up is to gate the TypeScript side
> off.

Today: `health`. Adding `chiefdOwnsHealthDuty()` silences every incident.

**Frozen reader** — the one that is possibly live:

> Same store NAME on both sides, different TABLE, and the surviving writer is
> not the one the consumer reads.

Today: telegram gateway state (#92). A name-only check reads as "migrated
cleanly", which is exactly why it survived.

**Silently ineffective** — worth knowing even though nothing breaks:

> A durable config that nothing writes and nothing reads, whose name implies it
> is the authority.

Today: none of the retired admission surface remains (the
`provider-admission-config` example retired with #748); a future store must
find its own example.

## ⚠️ What the name-based guard CANNOT see

The collision guard keys on a store NAME, so it only sees stores living in the
`documents` table. **It would NOT have caught #79** — chiefd stages mail in a
native RELATIONAL `mailbox` table, which has no store name at all, while the
pane drains a `mailbox/<personId>` DOCUMENT, keyed on a different id.

A guard with a blind spot that reads as total coverage is worse than no guard,
so the relational tables get their own registry
(`NATIVE_RELATIONAL_TABLES` in the same test file): every native table must be
declared with who owns it and whether TypeScript has a counterpart. Declaring a
table does not prove it is safe — it proves somebody LOOKED, and the test fails
when a new native table appears with nobody having answered the question, which
is precisely how #79 got in.

All fourteen current tables are declared: `documents`, `assignments`,
`effects`, `counters`, `fresh_session_transitions`, `mailbox` (the known fork),
`event_markers`, `reflections`, `provider_slots` / `provider_reservations`
(dead), `leases`, `host_actions`, `companies`, `lifecycle_intents`.

## Live measurements (deployer, read-only)

**#92 telegram gateway state — the fork is REAL, and it is LATENT, not live:**

    org_documents (slug='tribes-capital@cfa32a29d9cd', store='telegram-gateway')
      contacts {"<chat-id>":"<contact>"}   lastSuccessAt 2026-07-23T23:48:16   FROZEN ~8h40m
    documents     (native, no slug column, store='telegram-gateway', rev 3734)
      contacts {"<chat-id>":"<contact>"}   lastSuccessAt 2026-07-24T08:32:26   3s before the query

The structural claim is confirmed exactly — chiefd writes the native row
continuously while the `org_documents` row has not moved since the cutover —
but the contacts are byte-identical, because **no new chat has messaged the bot
since**. So the doorbell still has a recipient today. **The next new contact is
the one that goes missing**, and "a new person messages the bot" is an entirely
ordinary event, so this is a loaded gun rather than an outage.

**#69 schedule spellings — class retired for this company:** 16 `nextDueAt`
stamps, ZERO non-canonical, across both stores, with the pattern control run
first (and an initial `grep -c` miscount caught and corrected — it counted
lines, and the blob is one line, which would have made it "0 out of 1" and
worthless as a retirement).

## Two query traps worth knowing before you read any absence

1. **The live database is not where you would look.** This deployment runs BOTH
   tables inside `/root/.write-db/org.sqlite` via `CHIEFD_STORE_DB_PATH`;
   `<data_root>/<slug>/chief.db` exists but holds no telegram rows. Querying the
   "obvious" path returns empty and reads as absence.
2. **The composite key**, as above. Deployer's method is the one to copy: rather
   than querying by an assumed key, **list all candidate rows in BOTH tables and
   let the rows show you their own key shapes** — which is how the slug
   composite and the native table's missing slug column became observed facts
   rather than assumptions.

## Closed incidental finding

Deployer also spotted a stale native `telegram` store (revision 350, last
written 2026-07-21). That is explained in-tree and already fixed:
`store/telegram.rs:410` records that the store-name constant once drifted to
`"telegram"` — *"a key nothing on the TypeScript side ever writes to"* — found
live on 2026-07-21. The row is that drift's tombstone, not a third store.

## Recommended guard

An architectural test that fails when a store name appears BOTH in a
`Ledgers::put_document` call and in a TypeScript `storeName`, unless explicitly
listed as bridged. That converts "someone must remember" into "CI says no" —
the same move `observe_scaffolding_is_isolated` already makes for the
observe-mode scaffolding. It would have caught `session-maintenance`,
`launch-intent` and the telegram gateway fork; a companion check ("every
chiefd-native store has at least one production writer AND one consumer that
acts") would have caught `health` (and would have caught
`provider-admission-config` before #748 retired it).

## Two geometries, and only one is caught by comparing reads

Worth separating, because the detection method differs:

* **Two implementations of ONE STORE** — they diverge visibly. The same logical
  read returns different bytes on each side, so comparing reads finds them.
  Everything in the table above is this shape.
* **Two stores implementing ONE CONCEPT** — they never disagree about anything,
  because they never touch. Each is individually correct, both use the same
  word, and the gap is visible only to someone holding both halves at once.
  #22 (`blocked`) is this shape.

**The second class is the more dangerous one and it needs its own pass**, with
a different question: not "do these two reads agree" but **"what else in this
system claims this word?"** A read-comparison sweep returns clean on all of it.
That pass is not in this document and should be its own piece of work.

## Recorded decision: normalising `nextDueAt` on ingest (#69)

#69's ticket proposed a second fix — validate/normalise `nextDueAt` at
`ingest_external_document` so a foreign timestamp spelling can never ENTER the
ledger. It was deliberately NOT done as part of that read-path fix, and it
belongs here rather than there because it is a **two-implementations** decision:
the launcher validates with the lenient `Date.parse` and chiefd parses strictly,
so "what is a legal timestamp" currently has two answers.

The open question is semantics, not mechanism: on a foreign spelling, does
adoption **reject the whole ingest** (loud, but a single bad field discards a
whole supervision document) or **rewrite the field** (silent repair, but chiefd
then edits a document it does not own)? That is a product decision about who
owns the format, and it should be made with the same explicitness
`legacy_facts.rs` applies to each fact's polarity — not as a side effect.

Live evidence, so the priority is honest: a read-only scan of `tribes-capital`
found **16 `nextDueAt` stamps and ZERO non-canonical spellings**, across both
the native and `org_documents` copies — with the pattern control run first, so
the zero is real evidence rather than a broken query. So the ingest path has not
actually admitted a foreign spelling on this company. It remains a real gap for
any company whose ledger was authored elsewhere.

## Method note, for whoever audits the next store

Ask **who writes it**, not "is any invariant violated". Nothing here violates an
invariant — the two loaded guns have no callers at all, and the trap door is
two correct implementations that simply cannot see each other. An invariant
sweep returns clean on every finding in this document.

And always state **which table, with which key**. The composite slug
(`<slug>@<12 hex>`) and the two-table split mean a single query answers a
different question than the one you asked, and returns 0 rows while looking
authoritative.

## #85 characterisation: what `generation` in a read response actually means

Team-lead asked for the anomaly to be CHARACTERISED before any fix, and named
it precisely: `/v1/docs/read` returning `generation` 8138 → 8503 → 8911 —
monotonic, matching no store counter. It is explained, and it is **not** the
same defect as the reset-on-drop hazard.

### The mechanism

`generation` in a read response is **not one counter**. Which counter you get
depends on `(store, is-this-process's-own-company)`:

| case | what `generation` is | resets on store drop? |
|---|---|---|
| `supervision`, this process's own company | the **live CompanyDb supervision ledger revision** (`router.rs:358-372`) | **no** — it lives in CompanyDb |
| `org-manifest`, own company | the live manifest revision (`router.rs:333-347`) | no |
| everything else (all other stores, all foreign slugs) | the **`org_documents` ROW generation** | **yes** (#85: 2 → 1, proven) |

So 8138 → 8503 → 8911 is the **supervision ledger revision**: monotonic by
construction, matching no `org_documents` row counter because it is a different
counter entirely, and advancing in hundreds because every supervision commit
bumps it on a busy company. Nothing is wrong with those numbers.

That is by design and the design is coherent — `cas` fences on the same
authority (`router.rs:530-541` compares `expected` against
`supervision::read(...).revision`), which is exactly #440's stated goal: *"read
and write agree on one counter."*

### So the two anomalies are DIFFERENT defects — answering the question directly

- **The 8138→8911 sequence: not a defect at all.** It is the ledger revision,
  correctly served and correctly fenced.
- **#85's reset-on-drop: still a real hazard**, but it applies only to the
  ROW-generation cases — i.e. every store EXCEPT supervision/manifest on the
  local company. Supervision is immune because its number never comes from the
  droppable row.

### The finding this actually surfaces

**A caller cannot tell which counter it received.** The field is named
`generation` in both cases, is a bare integer in both cases, and the response
carries nothing distinguishing them. That has three consequences worth
recording:

1. **A generic conditional-read client is unsafe for a second reason**, on top
   of #85's reset: `ifGenerationNot` would compare values whose *semantics vary
   per row*. A client holding a supervision ledger revision and a client
   holding a row generation are holding different kinds of thing under one
   field name.
2. **`ifGenerationNot` is silently IGNORED for supervision on the local
   company** — the live-read path returns before the conditional branch is
   reached (`router.rs:355-378`), and the router's own comment says so. A
   caller would receive a full blob and no `unchanged`, with no indication its
   probe was not applied. That is not wrong today (nobody sends it), but a
   client written against the documented contract would silently get no
   optimisation and might reasonably conclude the document had changed.
3. **The TypeScript `DocumentGeneration` brand currently brands both counters
   under one name.** The brand still does its job — it stops a *runtime*
   generation being confused with a *docstore* generation — but it does not
   separate ledger-revision from row-generation. Worth knowing before anyone
   builds on it.

### Recommendation (characterisation only — no fix proposed here)

Before any conditional-read client is built, the response should say **which
counter it is returning** — e.g. a `generationKind: "row" | "ledger-revision"`
discriminator, or separate field names. Without that, `ifGenerationNot` is a
comparison between values a caller cannot verify are commensurable, and #85's
reset makes one of the two kinds unsafe to cache against at all.

## #89 characterisation: can the launch-intent fence actually diverge?

The fence is the highest-severity shape in this document — a mutual-exclusion
fence split across two stores would mean two sides disagreeing about **who is
allowed to run**, and both halves look correct in isolation. So: measured
before proposing anything.

### The answer: it cannot diverge today, because the native side is inert

**One writer.** The TypeScript `org-launch-intent.ts` writes the
`org_documents` `launch-intent` row (`:161` add, `:190` remove, `:214` clear),
called from the CLI, supervision transport, runtime, staffing lifecycle and
units.

**One reader, read-only, on the same row.** chiefd reads it through
`legacy_facts::read_launch_intent_person_ids` with the composite key, wired at
`converge_apply/cycle.rs:743` and consumed as the cycle's fence at `:818-826`
via `launch_fence_from_legacy` (`:754-769`).

**The native store is referenced by nothing in production.** Verified two
independent ways rather than one: `launch_intent::` appears in no file under
`chiefd/src`, `chiefd-host/src` or `chiefd-api/src`; and `LaunchIntentStore`
appears nowhere outside `store/launch_intent.rs` itself and its tests. So the
native `add`/`clear` (`:203`, `:215`) and the native `read` (`:143`) are all
unreachable from the daemon.

**And this is now enforced, not merely observed.** The #81 architectural guard
(`the_unbridged_native_stores_still_have_no_production_caller`) fails the build
the moment anyone calls `launch_intent::add`/`clear` — which is precisely the
event that would create the split. Verified to fire.

### Sizing when it WOULD diverge

At the first production caller of the native writers, and not before. There is
no gradual path: today one store holds the fence; the instant something writes
the native row, chiefd's converge would still read the `org_documents` row —
so the new fence would be invisible to the only code that enforces it.

### The no-store case fails safe, which is the third thing worth knowing

If no legacy store is wired, the projection is `None` (`cycle.rs:818-826`) and
the fence projection is **skipped entirely** — not defaulted to empty, and not
defaulted to open. That matches `legacy_facts.rs`'s stated polarity for this
fact: an unreadable fence means *skip the pass*, never *actuate from a
fabricated empty fence*.

### One real asymmetry, recorded rather than fixed

The Rust type makes a permissive fence **unconstructible** — `LaunchIntent` has
a single `Fenced` variant, and the module says so: *"a permissive variant is
not merely discouraged, it does not exist, so no code path — including one
written by someone who has never read this file — can obtain one."* The
TypeScript side achieves the same guarantee by convention plus a grep test
(inv c-1). Same rule, two strengths: one enforced by the compiler, one by a
test somebody must keep.

That asymmetry costs nothing today, but it is worth knowing which half is
load-bearing — **the enforcing half is currently the one that is inert.**
