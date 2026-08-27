//! SQL schema for a company's `chief.db`.
//!
//! Plan §5.1. Two rules govern what goes here:
//!
//! 1. Rules that fit a `CHECK` or a unique index go into the DDL **as
//!    assertions**. Their firing is a bug, not a user-visible outcome: the
//!    limit itself is enforced in `validate(&Ledger)` and returned as
//!    `Refused{code, legalRoutes}` so no SQL error ever reaches the wire
//!    (plan §1).
//! 2. Rules that cannot be expressed in SQL — `uniqueOrder` bijectivity,
//!    effect-mirrors-assignment, provider
//!    lane-key sha256 re-derivation, retry ceilings — live in `validate()`,
//!    which runs after every mutation before commit.
//!
//! **Not yet built (M4 onward).** Migrations, `validate()` implementations and
//! the per-store row splits arrive with their stores. M1 lands the DDL text
//! itself because it is the cross-track contract every store port is written
//! against, plus the guard tests that stop a later "simplification" from
//! quietly removing a load-bearing clause.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

/// DDL for a company database (`<dataRoot>/<slug>/chief.db`).
///
/// Applied once at creation and idempotently at open. WAL mode and
/// `synchronous=FULL` are set as pragmas by [`crate::store`], not here.
pub const COMPANY_SCHEMA_SQL: &str = r#"

-- rowid reuse is forbidden here: a pruned max row must never let a later
-- insert reuse a sequence value a reader already observed. delta #36: seq is
-- now the per-company effect.sequence (written from the slug-scoped
-- NEXT_EFFECT_SEQUENCE counter, the sole per-company no-reuse authority —
-- org-supervision-state.ts:815 rule) under PK (slug, seq); the global
-- AUTOINCREMENT is dropped (it collided across companies once the table is
-- slug-scoped in the shared org.sqlite). Same #32-class destructive-leak fix.
CREATE TABLE IF NOT EXISTS effects(
    slug         TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    id           TEXT NOT NULL,
    kind         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','delivered','superseded','failed')),
    created_at   TEXT NOT NULL DEFAULT '',
    delivered_at INTEGER,
    superseded_at TEXT,
    delivery_failure_count INTEGER,
    last_delivery_failure_at TEXT,
    failed_at TEXT,
    reopen_count INTEGER,
    last_reopened_at TEXT,
    PRIMARY KEY (slug, seq),
    UNIQUE (slug, id)
);

-- Kind-specific effect content is a bounded scalar child relation, never an
-- opaque JSON body. Scalars use (is_array=0, ordinal=0); non-empty arrays use
-- (is_array=1, ordinal=0..N); an empty array has one explicit marker row
-- (is_array=1, ordinal=-1, all value columns NULL). The shape CHECK makes the
-- scalar/array distinction lossless without a JSON or sentinel fallback.
CREATE TABLE IF NOT EXISTS effect_payloads(
    slug          TEXT NOT NULL,
    effect_id     TEXT NOT NULL,
    field         TEXT NOT NULL,
    ordinal       INTEGER NOT NULL,
    is_array      INTEGER NOT NULL DEFAULT 0 CHECK(is_array IN (0,1)),
    value_text    TEXT,
    value_integer INTEGER,
    value_boolean INTEGER CHECK(value_boolean IN (0,1)),
    CHECK(
        (
            is_array = 0
            AND ordinal = 0
            AND (value_text IS NOT NULL)
                + (value_integer IS NOT NULL)
                + (value_boolean IS NOT NULL) = 1
        )
        OR (
            is_array = 1
            AND ordinal >= 0
            AND (value_text IS NOT NULL)
                + (value_integer IS NOT NULL)
                + (value_boolean IS NOT NULL) = 1
        )
        OR (
            is_array = 1
            AND ordinal = -1
            AND value_text IS NULL
            AND value_integer IS NULL
            AND value_boolean IS NULL
        )
    ),
    PRIMARY KEY(slug, effect_id, field, ordinal),
    FOREIGN KEY(slug, effect_id) REFERENCES effects(slug, id) ON DELETE CASCADE
);

-- delta #36 note: counters are per-company via the NAME (the D2 per-slug seq
-- pattern already embeds slug in `name`, e.g. `<counter>:<slug>`), NOT a slug
-- column — a column would double-scope and break allocate_seq. The per-company
-- NEXT_EFFECT_SEQUENCE / NEXT_ASSIGNMENT_SEQUENCE counters MUST likewise embed
-- slug in their name (Rust writer's responsibility), same as the D2 counters.
CREATE TABLE IF NOT EXISTS counters(
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);

-- Cryptographic caller identities (agent-auth P0, #819).
--
-- COMPANY-OWNED, not host-scoped: E10-S2 gives one chiefd process exactly one
-- company database, so this daemon's complete identity set is this company's
-- identity set. The verify middleware can resolve `sub` + `kid` before any
-- handler without scanning companies or carrying a company in the token.
--
-- The revocation anchor is the KEY, never the token: `active=0` locks an
-- identity out, and `fingerprint` is its CURRENT key fingerprint (keypair) or a
-- random CHANNEL EPOCH (kind='channel', no pubkey). A minted JWT carries
-- `kid=fingerprint`; the middleware rejects any token whose kid != the row's
-- current fingerprint, so rotating the fingerprint invalidates every token the
-- identity ever held. Two identities may share one `principal` (the operator's
-- keypair plus its 'operator-pane' and 'operator-remote' channels):
-- authorization keys on the principal, enrolment/revocation on the identity.
CREATE TABLE IF NOT EXISTS identities(
    identity_id  TEXT PRIMARY KEY,
    principal    TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK(kind IN ('person','operator','service','channel')),
    -- The company a person belongs to. NULL for daemon-scoped identities
    -- (operator, channel, service, and the bootstrap identity); SET for a
    -- person. Remote agents enrol daemon-scoped (NULL) unless bound to a company.
    company_slug TEXT,
    pubkey       TEXT,
    fingerprint  TEXT NOT NULL UNIQUE,
    active       INTEGER NOT NULL CHECK(active IN (0, 1)),
    enrolled_at  INTEGER NOT NULL,
    enrolled_by  TEXT,
    revoked_at   INTEGER,
    -- Only a person is company-scoped; every daemon-scoped kind has no slug.
    CHECK((kind = 'person') = (company_slug IS NOT NULL)),
    -- Channels are attested server-side and carry no pubkey; every other kind
    -- authenticates by signature and MUST have one.
    CHECK((kind = 'channel') = (pubkey IS NULL))
);

CREATE INDEX IF NOT EXISTS identities_principal_idx ON identities(principal);

-- N-mailbox COLUMNARIZATION (Fable ruling #5/#7). The native body held a
-- serialized MailboxEnvelope (STRUCTURE) → the "KEEP body opaque" disposition
-- was OVERTURNED: the envelope is exploded into typed columns; only the human
-- payload survives as the opaque `message` VALUE. `organization` is DERIVED
-- from the per-company CompanyDb identity (the DB file IS the scoping — no slug
-- column here, like effects), NOT stored. recipients[] is DERIVED
-- (rows are per-recipient: SELECT person WHERE id=?). #493(A) owns the `state`
-- column change (adds 'delivered') and merges FIRST; this slice adapts.
-- (delta #28 reconciled to the canonical a2804e0a shape — this is the
-- Fable-#7-screened design already on the b6/n2-land lineage; my earlier
-- variant that stored organization/schema_version + a 2-col PK was superseded.)
CREATE TABLE IF NOT EXISTS mailbox(
    -- delta #35: slug scope. The "no slug column here — the DB file IS the scoping"
    -- premise above is the per-slug-chief.db FALLBACK, NOT the live surface: the
    -- daemon multiplexes companies into one shared org.sqlite (CHIEFD_STORE_DB_PATH
    -- wins), so a slug-less per-company table leaks across companies — the exact
    -- #32 finding (78 cross-company rows). PK-prefixed with slug like every other
    -- store table. (organization is still DERIVED, not stored as a separate col.)
    slug              TEXT NOT NULL,
    -- envelope_id = `<id>@<person>` (store/mailbox.rs row_id) — the durable
    -- idempotency identity every reader keys on. KEPT (with slug) as PK with a
    -- pinning CHECK (Fable screen): a derivable-but-stored key is not the dual-rep
    -- defect once a CHECK makes drift unrepresentable.
    envelope_id       TEXT NOT NULL,
    id                TEXT NOT NULL,           -- logical envelope id
    person            TEXT NOT NULL,           -- THIS row's recipient
    from_person_id    TEXT NOT NULL,
    to_person_id      TEXT NOT NULL,           -- the PRIMARY recipient (envelope.to)
    message           TEXT NOT NULL,           -- the ONE opaque human payload (was body)
    urgency           TEXT NOT NULL CHECK(urgency IN ('normal','interrupt')),
    reply_to          TEXT,                    -- opaque VALUE, nullable
    -- health_incident sub-struct → nullable scalar columns, present-as-a-group.
    health_fingerprint            TEXT,
    health_kind                   TEXT,
    health_recipient_person_id    TEXT,
    created_at        TEXT NOT NULL,           -- ISO-8601 (envelope.created_at)
    -- (A)-owned six-bucket vocab; 'delivered' (lowercase, ctrl-plane-confirmed
    -- token) = wake-demand consumed at fence commit, DISJOINT from the pane-drain
    -- terminals accepted/superseded/resolved/rejected.
    state             TEXT NOT NULL CHECK(state IN
                        ('pending','delivered','accepted','superseded','rejected','resolved')),
    updated_at        INTEGER NOT NULL,
    -- Fable screen: pin envelope_id to its two roles so the composite can never drift.
    CHECK (envelope_id = id || '@' || person),
    -- health group present-together.
    CHECK (
      (health_fingerprint IS NULL AND health_kind IS NULL
        AND health_recipient_person_id IS NULL)
      OR
      (health_fingerprint IS NOT NULL AND health_kind IS NOT NULL
        AND health_recipient_person_id IS NOT NULL)
    ),
    PRIMARY KEY (slug, envelope_id)
);
CREATE INDEX IF NOT EXISTS mailbox_person ON mailbox(slug, person);

-- delta #29 (N7 Fable ruling): journal-markers -> event_once_markers. Fable
-- REFUSED a payload column on org_events; this is the own-table home. Grep-proven
-- (org-health-monitor.ts): the marker payload is read back by exactly ONE
-- consumer for event_type='terminal-health-incident-resolved' (52 rows); every
-- other type is existence-only. So typed columns exist only for the one
-- read-back contract: terminal-health-resolution (`thr_*`). Everything else
-- remains existence-only; NO typed-blob. Replaces the strangler event_markers (typed-blob +
-- journal_appended, both gone). DOCUMENTED DROP of all other event payloads
-- remains in force.
CREATE TABLE IF NOT EXISTS event_once_markers(
    -- delta #32: slug scope. The LIVE surface is a SHARED org.sqlite
    -- (CHIEFD_STORE_DB_PATH wins over <data_root>/<slug>/chief.db), multiplexed
    -- by slug — a slug-less table leaks rows across companies (verified: 78
    -- cross-company rows). PK-prefixed with slug like every other store table.
    slug            TEXT NOT NULL,
    key_digest      TEXT NOT NULL,          -- sha256(id), the store suffix
    id              TEXT NOT NULL,          -- logical event id (event.id)
    schema_version  INTEGER NOT NULL DEFAULT 1,
    event_type      TEXT NOT NULL,          -- event.event
    created_at      INTEGER NOT NULL,       -- row write time; drives the 48h reactive prune
    -- typed columns for the ONE read-back consumer; NULL for every other type.
    thr_message_id             TEXT,
    thr_fingerprint            TEXT,
    thr_kind                   TEXT,
    thr_incident_first_seen_at TEXT,
    thr_recipient_person_id    TEXT,
    thr_accepted_at            TEXT,
    PRIMARY KEY (slug, key_digest),
    UNIQUE (slug, id)
);
CREATE INDEX IF NOT EXISTS event_once_markers_created ON event_once_markers(slug, created_at);

-- delta #29 (N7 Fable ruling): mutation-journal -> own table. Fable: it's a
-- MUTABLE state machine (in-flight->committed->abandoned) with a fingerprint-
-- adoption lookup; org_events is append-only (REFUSED). Bounded retention (keep
-- newest 32 committed; in-flight/abandoned never dropped) is a same-txn DELETE in
-- publish logic, not DDL. `seq` = append order (counters row / MAX(seq)+1).
CREATE TABLE IF NOT EXISTS mutation_journal(
    slug            TEXT NOT NULL,          -- delta #32: per-company scope (shared org.sqlite)
    mutation_id     TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    verb            TEXT NOT NULL,
    fingerprint     TEXT NOT NULL,
    status          TEXT NOT NULL CHECK(status IN ('in-flight','committed','abandoned')),
    started_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    actor           TEXT,
    PRIMARY KEY (slug, mutation_id)
);
CREATE INDEX IF NOT EXISTS mutation_journal_adopt
    ON mutation_journal(slug, fingerprint, seq) WHERE status = 'in-flight';

-- delta #29 (N7 Fable ruling): health-monitor -> OWN 4-table slice (module
-- health_monitor.rs), SPLIT from the scalar `health` table. Live-.bak confirms
-- incident keys are FLAT (no nested sub-objects); terminalResolutions is the
-- single 'accepted' variant. version/organization = dropped identity
-- guards. The state's scalar lastRunAt -> the tiny health_monitor_meta singleton.
-- delta #32: slug scope on all 5 health_monitor_* tables (shared org.sqlite).
CREATE TABLE IF NOT EXISTS health_monitor_meta(
    slug        TEXT PRIMARY KEY,                    -- one meta row per company
    last_run_at TEXT
);
CREATE TABLE IF NOT EXISTS health_monitor_cursors(
    slug    TEXT NOT NULL,
    path    TEXT NOT NULL,
    device  TEXT NOT NULL,
    inode   TEXT NOT NULL,
    offset  INTEGER NOT NULL CHECK(offset >= 0),
    PRIMARY KEY (slug, path)
);
CREATE TABLE IF NOT EXISTS health_monitor_observations(
    slug              TEXT NOT NULL,
    key               TEXT NOT NULL,
    first_observed_at TEXT NOT NULL,
    last_observed_at  TEXT NOT NULL,
    count             INTEGER NOT NULL CHECK(count >= 1),
    PRIMARY KEY (slug, key)
);
CREATE TABLE IF NOT EXISTS health_monitor_incidents(
    slug                       TEXT NOT NULL,
    fingerprint                TEXT NOT NULL,
    kind                       TEXT NOT NULL,
    detail                     TEXT NOT NULL,
    first_seen_at              TEXT NOT NULL,
    last_seen_at               TEXT NOT NULL,
    count                      INTEGER NOT NULL,
    responsible_person_id      TEXT,
    unblock_action             TEXT,
    observed_count             INTEGER,
    oldest_at                  TEXT,
    acknowledged_at            TEXT,
    alert_recipient_person_id  TEXT,
    impaired_mailbox_person_id TEXT,
    PRIMARY KEY (slug, fingerprint)
);
CREATE TABLE IF NOT EXISTS health_monitor_terminal_resolutions(
    slug                 TEXT NOT NULL,
    fingerprint          TEXT NOT NULL,
    kind                 TEXT NOT NULL,
    first_seen_at        TEXT NOT NULL,
    recipient_person_id  TEXT NOT NULL,
    accepted_at          TEXT NOT NULL,
    PRIMARY KEY (slug, fingerprint)
);

-- delta #33 RETRACTED (arch Step 4, F16): the daemon-health 5-table slice was
-- the wrong-subsystem landfill — the colliding store name "health" routed the
-- daemon's ORG-health duty commits here, invisible to every health_monitor_*
-- reader. The duty's store now names the health_monitor_* tables above and
-- persists to them (merge semantics, Step 3). The orphaned
-- tables are dropped idempotently: their data was the duty's fail-open
-- working state (cursors/observations re-baseline on the next pass) plus
-- incidents no reader could see; nothing migrates.
DROP TABLE IF EXISTS daemon_health_meta;
DROP TABLE IF EXISTS daemon_health_cursors;
DROP TABLE IF EXISTS daemon_health_observations;
DROP TABLE IF EXISTS daemon_health_incidents;
DROP TABLE IF EXISTS daemon_health_terminal_resolutions;
-- #751 fallout: `runtime_windows(slug, department, window_id)` mapped a
-- department to the tmux window it was drawn in. chiefd stopped owning tmux, so
-- both publishers hardcoded the map to empty and no backend reader ever
-- consulted its contents — a dead mechanism, and one the TypeScript roster
-- reader still demanded an entry from, which is half of why `org_roster` failed
-- for every person in every company. Deleted, not retired: a window id is a
-- display grouping and the client that draws it owns it.
DROP TABLE IF EXISTS runtime_windows;

-- delta #29 (N7 Fable ruling): runtime -> full typed table. Fable's write-only-
-- recompute control FAILED (every key has a real cross-actor reader), so NOT
-- ExpectedDropped. Singleton parent + typed child tables for the process-handle
-- map +
-- carry-forward arrays. NO typed-blob. (Carry-forward-only keys
-- recoveryConfirmed/recovery + the 3 arrays are MODELED here, zero-loss; the
-- recompute-drop alternative is flagged to Fable.)
CREATE TABLE IF NOT EXISTS runtime(
    slug                           TEXT PRIMARY KEY,  -- delta #32: one runtime row per company (shared org.sqlite)
    version                        INTEGER NOT NULL,
    observed_at                    TEXT NOT NULL,
    -- AC6: a `session TEXT NOT NULL` column stood here and stored `org-<slug>`
    -- — the company slug this row is already keyed by, with a prefix. Its one
    -- reader compared it against the manifest's copy of the same derivation, so
    -- it could not disagree with anything. It is simply absent from this DDL,
    -- and a company database never grows it.
    socket_name                    TEXT NOT NULL,
    -- delta #34: a stopped runtime keeps its observed topology empty.
    status                         TEXT NOT NULL CHECK(status IN ('running','idle','recovering','starting','stopped')),
    startup_admission_until        TEXT,
    -- TOMBSTONE (chief-home-is-cwd §4c): `startup_ceo_admission_debt INTEGER
    -- CHECK(... IN (0,1))` stood here — the one-shot "this startup admitted the
    -- CEO on the daemon's own boot, so the next non-CEO batch still owes an
    -- admission" flag. It is deleted with the daemon-side CEO boot that was the
    -- only thing able to incur the debt; nothing writes or reads it, and a
    -- company database never grows the column.
    recovery_fingerprint           TEXT,
    recovery_observed_at           TEXT,
    recovery_confirmed             INTEGER CHECK(recovery_confirmed IN (0,1)),
    recovery                       TEXT,
    recon_phase                    TEXT,
    recon_started_at               TEXT
);
-- delta #32: slug scope on the runtime child tables (shared org.sqlite).
CREATE TABLE IF NOT EXISTS runtime_process_handles(
    slug            TEXT NOT NULL,
    person          TEXT NOT NULL,
    -- The pid as a decimal string, or '' when the actuator proved the person
    -- alive but could read no pid. NEVER a tmux pane id: chiefd has held none
    -- since #751 and the column was called `pane_id` while holding pids, which
    -- is precisely what a reader believed when it refused every real payload.
    process_handle  TEXT NOT NULL,
    PRIMARY KEY (slug, person)
);
CREATE TABLE IF NOT EXISTS runtime_monitor_warnings(
    slug    TEXT NOT NULL,
    seq     INTEGER NOT NULL,
    warning TEXT NOT NULL,
    PRIMARY KEY (slug, seq)
);
CREATE TABLE IF NOT EXISTS runtime_recovery_people(
    slug   TEXT NOT NULL,
    seq    INTEGER NOT NULL,
    kind   TEXT NOT NULL CHECK(kind IN ('missing','unexpected')),
    person TEXT NOT NULL,
    PRIMARY KEY (slug, seq)
);

-- delta #68 (#748): the provider-admission pool is RETIRED. Fresh managed Pi
-- turns call their configured provider transport directly; the provider owns
-- capacity limits. Historical databases cross an explicit idempotent migration
-- boundary: the retired pool tables are DROPPED (their rows were per-boot
-- ephemeral coordination — holders/waiters/reservations — that no reader
-- survives the boot), exactly like the daemon-health slice above. No provider
-- request depends on this migration completing: nothing reads these tables
-- anymore.
DROP TABLE IF EXISTS provider_slots;
DROP TABLE IF EXISTS provider_reservations;

-- delta #946/#820: the two-phase company-removal journal (PREPARE/
-- QUARANTINE/FINALIZE) is RETIRED. D23/F17 replaced it wholesale with the
-- four-step ordering (pane teardown -> stop daemon -> delete beacond row ->
-- delete files); #817 implemented that in TypeScript. This machinery's
-- restart-recovery path looked live (a real production dispatch point,
-- `live_company_resolver`'s `RemovalRecovery` mode) but traced back to the
-- same dead route as everything else that used it -- nothing independently
-- reaches it. The four company_removal* tables are DROPPED, exactly like
-- the daemon-health and provider-pool slices above: their rows were an
-- in-flight journal for a protocol that no longer runs, no reader survives
-- them, and no new write ever lands in them again.
DROP TABLE IF EXISTS company_removal;
DROP TABLE IF EXISTS company_removal_stopped_windows;
DROP TABLE IF EXISTS company_removal_stopped_panes;
DROP TABLE IF EXISTS company_removal_completion_receipts;

-- The per-UNIT removal journal is retired on the same reasoning, and for a
-- blunter reason: it never ran at all. `unit_removals` and
-- `unit_removal_members` shipped in the N1 DDL batch and never acquired a
-- store module, a route, or a writer -- no INSERT, UPDATE or SELECT of either
-- table exists in any Rust or TypeScript file, and none of the three phases
-- its CHECK admitted ('planned', 'manifest-committed', 'runtime-reconciled')
-- was ever produced, in production or in a fixture. The CHECK cited an
-- `org-ops R1` `UnitRemovalPhase` enum that was never written either. The live
-- department-removal path deliberately bypasses the journal: `remove_department_tree`
-- (`store/org_ops.rs`) is one guarded transaction. Dropped in FK order.
DROP TABLE IF EXISTS unit_removal_members;
DROP TABLE IF EXISTS unit_removals;

-- Host-transaction intents: commit 1 of the DB<->filesystem 2PC (plan §5.6).
CREATE TABLE IF NOT EXISTS host_actions(
    action_id      TEXT PRIMARY KEY,
    kind           TEXT NOT NULL,
    payload_schema TEXT NOT NULL CHECK(payload_schema IN ('host-txn-v1','converge-intent-v1')),
    plan_json      TEXT NOT NULL CHECK(json_valid(plan_json) AND json_type(plan_json) = 'object'),
    phase          TEXT NOT NULL CHECK(phase IN ('pending','published','closed')),
    created_at     INTEGER NOT NULL,
    CHECK((kind = 'converge' AND payload_schema = 'converge-intent-v1') OR
          (kind <> 'converge' AND payload_schema = 'host-txn-v1'))
);

-- ==========================================================================
-- NORMALIZED ORG SCHEMA (org-data-normalization P0, N1 — plan §2).
--
-- Every field is a real typed column; NO column stores JSON. Text enums are
-- CHECK-constrained; arrays become child tables; id-keyed maps become PKs;
-- ordering becomes `ordinal`. All slug-scoped tables carry `slug` in the PK
-- prefix (multi-company). Timestamps are ISO-8601 TEXT to match the existing
-- `tasks` table convention. Validation that cannot live in SQL (tree
-- acyclicity on reparent, uniqueOrder bijectivity) runs in validate() inside
-- the same BEGIN IMMEDIATE transaction (plan §2, §3).
--
-- FABLE-ARCH RULINGS (2026-07-25, both RESOLVED):
--   (D1) TEXT-column rule: a TEXT column may hold an opaque VALUE the system
--        never parses (prose — message text, memory content,
--        reasons, mandates). It may NEVER hold serialized STRUCTURE — anything
--        parsed/keyed/iterated becomes child tables or real columns. Every
--        former `body` field has an explicit disposition
--        (column / child-table / derived / deleted-with-proof-unread);
--        effects are fully columnarized and their compatibility
--        body columns are absent from fresh DDL. CI GATE (N7): no JSON.parse /
--        serde_json::from_str on ANY
--        DB-column read (the one-shot migration script is the sole exemption,
--        then deleted).
--   (D2) org_events `seq` = per-slug counter row bumped in the SAME
--        BEGIN IMMEDIATE txn; PK (slug, seq); NO global AUTOINCREMENT. Same
--        allocation for staffing_history.seq. See the org_events comment below.
--   (D3) SINGLE DB = the live data-root org.sqlite; this constant is the ONE
--        schema source, applied by ONE shared opener. The chiefd-api
--        docstore/store.rs DDL dup + the chief.db opener are retired at cutover
--        (migrate anything authoritative in chief.db first).
--
-- 16 adversarial-review deltas applied (delta #N tags below). Legacy-table
-- transformations were delivered through their owning port slices
-- (mailbox body->message; effects body-strip and effect
-- columnarization); the fresh schema below contains only the final forms.
-- ==========================================================================

-- Core org structure (replaces the org-manifest blob + the global revision).
CREATE TABLE IF NOT EXISTS departments(
    slug             TEXT NOT NULL,
    id               TEXT NOT NULL,
    parent_id        TEXT,                       -- NULL only for the root
    name             TEXT NOT NULL,
    purpose          TEXT NOT NULL DEFAULT '',
    kind             TEXT NOT NULL CHECK(kind IN ('company','department','contract')),
    state            TEXT NOT NULL CHECK(state IN ('active','paused')),
    head_person_id   TEXT NOT NULL,
    -- delta #17 (B2): contract-unit metadata as additive columns; `transient`
    -- is DROPPED (redundant: transient <=> kind='contract' <=> metadata present).
    -- The three columns are the DepartmentRecord.transient sub-record (1:0..1
    -- scalar triple, so columns not a child table); NULL for non-contract units.
    contract_engagement  TEXT,
    contract_launched_at TEXT,
    contract_expires_at  TEXT,
    ordinal          INTEGER NOT NULL,           -- replaces departmentOrder
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    -- delta #17: the metadata is present EXACTLY for contract units.
    CHECK ((kind = 'contract') = (contract_engagement IS NOT NULL)),
    CHECK ((kind = 'contract') = (contract_launched_at IS NOT NULL)),
    CHECK (contract_expires_at IS NULL OR kind = 'contract'),
    PRIMARY KEY (slug, id),
    FOREIGN KEY (slug, parent_id) REFERENCES departments(slug, id)
    -- NO FK on head_person_id (delta #2): departments<->people is a circular
    -- pair (people.department_id -> departments) that a plain FK cannot
    -- satisfy on insert. validate() owns head-exists AND head-is-a-member of
    -- the department it heads, inside the same BEGIN IMMEDIATE txn.
);
CREATE INDEX IF NOT EXISTS departments_parent ON departments(slug, parent_id);
-- Sibling order is a bijection within a parent; enforced here, uniqueOrder
-- bijectivity across the whole tree is re-checked in validate().
CREATE UNIQUE INDEX IF NOT EXISTS departments_sibling_ordinal
    ON departments(slug, parent_id, ordinal);
-- delta #2: a person heads at most one department.
CREATE UNIQUE INDEX IF NOT EXISTS departments_one_head
    ON departments(slug, head_person_id);
-- delta #3: exactly one root per company (parent_id NULL only for the root).
CREATE UNIQUE INDEX IF NOT EXISTS departments_one_root
    ON departments(slug) WHERE parent_id IS NULL;

CREATE TABLE IF NOT EXISTS people(
    slug                   TEXT NOT NULL,
    id                     TEXT NOT NULL,
    name                   TEXT NOT NULL,
    title                  TEXT NOT NULL,
    mandate                TEXT NOT NULL,
    kind                   TEXT NOT NULL CHECK(kind IN ('worker','head','executive')),
    employment_state       TEXT NOT NULL CHECK(employment_state IN ('active','benched','departed')),
    -- ONE placement column. There was a `home` + `assigned` pair here until the
    -- loan concept was deleted (2026-08-13): a loan was the only verb that could
    -- make the two disagree, so afterwards they were always equal and every call
    -- site's choice between them was arbitrary. The survivor is named for
    -- neither half of the dead dichotomy, and neither half is declared here.
    department_id          TEXT NOT NULL,
    -- delta #22 (N9 B1): PersonRecord.activation carries a REAL live value
    -- ('on-demand' on live workers vs the 'resident' default) — the eager/
    -- on-demand staffing distinction (THE HARD RULE). N9's shadow-diff proved a
    -- reconstruct without this column silently loses it, so it is a required
    -- column (NOT the seed-only start_active drop). Default 'resident'.
    activation             TEXT NOT NULL DEFAULT 'resident'
                               CHECK(activation IN ('resident','on-demand')),
    -- delta #18 (B2): PersonRecord real fields with no prior column.
    -- `work_monitoring` was DROPPED with the `@koltmcbride/pi-loop` addon: it
    -- only ever meant "this person holds a pi-loop runtime", and durable
    -- reminders replaced the session loop it gated. The retired static
    -- approval-tier column went the same way, and is unnameable here for the
    -- same reason the thinking justification is.
    -- delta #18 (B2, REVERSE gap): `start_active` DROPPED — seed-only, no
    -- PersonRecord field, unreconstructable from a live person (parity landmine).
    ordinal                INTEGER NOT NULL,      -- replaces peopleOrder
    created_at             TEXT NOT NULL,
    updated_at             TEXT NOT NULL,
    PRIMARY KEY (slug, id),
    FOREIGN KEY (slug, department_id) REFERENCES departments(slug, id)
);
CREATE INDEX IF NOT EXISTS people_department ON people(slug, department_id);
-- delta #4: peopleOrder is a per-company bijection (whole-tree uniqueOrder
-- bijectivity re-checked in validate()).
CREATE UNIQUE INDEX IF NOT EXISTS people_ordinal ON people(slug, ordinal);

-- Per-person tool grants (array -> child rows). One row per (person, tool).
CREATE TABLE IF NOT EXISTS person_tools(
    slug      TEXT NOT NULL,
    person_id TEXT NOT NULL,
    tool      TEXT NOT NULL,
    -- delta #49: PersonRecord.tools is an ORDERED array; without an ordinal the
    -- reconstruct returned tools alphabetically (a byte-for-byte materialization
    -- divergence from normalizeOrganizationSpec's insertion order). `ordinal`
    -- preserves array order; reconstruct sorts by it, publish writes it.
    ordinal   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (slug, person_id, tool),
    FOREIGN KEY (slug, person_id) REFERENCES people(slug, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS person_tools_ordinal
    ON person_tools(slug, person_id, ordinal);

-- delta #19 (B2): PersonRecord.prompts is ordered template refs (a list) — a
-- child table, NOT a person_resources kind. `template` is a path VALUE.
CREATE TABLE IF NOT EXISTS person_prompts(
    slug      TEXT NOT NULL,
    person_id TEXT NOT NULL,
    ordinal   INTEGER NOT NULL,
    template  TEXT NOT NULL,
    PRIMARY KEY (slug, person_id, ordinal),
    UNIQUE (slug, person_id, template),
    FOREIGN KEY (slug, person_id) REFERENCES people(slug, id)
);

-- delta #27 (N9 B4): person-contracts store — per-person AGENTS.md contract
-- TEXT keyed by personId (NOT departments/people; that's the manifest). Live-
-- verified: entries are exactly {text, md5}; version/
-- organization are DERIVED (const 1 / process slug). NO people FK ON PURPOSE —
-- the store is rebuilt whole from peopleOrder with its own independent fence; a
-- hard FK would couple its publish ordering to the manifest port (a departed
-- person's contract simply vanishes on the next whole-rewrite).
CREATE TABLE IF NOT EXISTS person_contracts(
    slug            TEXT NOT NULL,
    person_id       TEXT NOT NULL,
    text            TEXT NOT NULL,
    md5             TEXT NOT NULL,        -- md5(text); the TS boot path compares this
    PRIMARY KEY (slug, person_id)
);

-- delta #20 (B2): org-level policy singleton — 4 typed ints, never a policy
-- blob. name/purpose/created_at DERIVE from the root (kind='company') dept;
-- updated_at DERIVEs max(org_events.at); runtime_session DERIVEs 'org-<slug>';
-- schema_version is a compile-time constant in the DTO — none are stored.
--
-- E7-S3: `launcher_root` — the absolute path of the source checkout that last
-- materialized this company, replacing `state/launcher.json` — is declared here
-- like every other column on this table. There is exactly one code path for the
-- column's existence: this declaration.
CREATE TABLE IF NOT EXISTS org_settings(
    slug                        TEXT PRIMARY KEY,
    -- THE COMPANY'S DISPLAY NAME, stored because nothing else carries it.
    --
    -- `slug` above is the ROW key — the company's identity, which is the hash
    -- of the directory it lives in. The two were one column while the identity
    -- was the composite `<display>@<rootHash>`: the display name rode inside
    -- the key and `reconstruct` recovered it by stripping the suffix. A
    -- directory hash carries no name, so a manifest reconstructed that way came
    -- back called `c84afac7d8ad`, and every cross-store validator that checks
    -- `ledger.organization == manifest.slug` then refused a correctly seeded
    -- ledger. Genesis itself refused.
    --
    -- So the name is a stored fact now, which is what it always was.
    display_slug                TEXT NOT NULL,
    supervision_interval_ms     INTEGER NOT NULL,
    acknowledgement_timeout_ms  INTEGER NOT NULL,
    acknowledgement_retry_limit INTEGER NOT NULL,
    replacement_limit           INTEGER NOT NULL,
    launcher_root               TEXT
);

-- Append-only staffing ledger (hire/park/transfer/offboard).
-- delta #15: NO FK (slug, person_id) -> people ON PURPOSE — the ledger must
-- retain a person's history after they are fully removed from `people`
-- (departed retention). validate() may spot-check live rows only.
-- delta (D2): `seq` is a per-slug counter allocated the SAME way as
-- org_events.seq (dedicated counter row, in-txn, never MAX(seq)+1).
CREATE TABLE IF NOT EXISTS staffing_history(
    slug               TEXT NOT NULL,
    seq                INTEGER NOT NULL,
    person_id          TEXT NOT NULL,
    -- delta #22 (B2): the table REPLACES the manifest staffing ledger, so it
    -- carries that ledger's EXACT vocabulary — no lossy synonym map. Nine
    -- terms originally; `rehired` is the tenth (#1036), and it is deliberately
    -- NOT folded into `recalled`: a recall returns a BENCHED person, a rehire
    -- returns a DEPARTED one, and the person's durable history has to say
    -- which of the two happened. Widening this CHECK is the one part of that
    -- change a `CREATE TABLE IF NOT EXISTS` cannot deliver on a historical
    -- database, so the widened CHECK is declared here and nowhere else.
    action             TEXT NOT NULL CHECK(action IN
                         ('hired','benched','recalled','rehired',
                          'transferred','offboarded','appointed-head','stepped-down')),
    from_department_id TEXT,
    to_department_id   TEXT,
    reason             TEXT NOT NULL DEFAULT '',   -- opaque prose VALUE
    at                 TEXT NOT NULL,
    PRIMARY KEY (slug, seq)
);
CREATE INDEX IF NOT EXISTS staffing_history_person ON staffing_history(slug, person_id);

-- ---- Lifecycle (replaces the activity blob) ------------------------------
CREATE TABLE IF NOT EXISTS transitions(
    slug                        TEXT NOT NULL,
    id                          TEXT NOT NULL,
    person_id                   TEXT NOT NULL,
    action                      TEXT NOT NULL CHECK(action IN
                                  ('park','transfer','offboard')),
    status                      TEXT NOT NULL CHECK(status IN
                                  ('awaiting_handoff','overdue','ready',
                                   'applied','cancelled','forced')),
    -- NULLABLE by design (delta #1): NULL = an unowned idle-park; a non-NULL
    -- value = an intent-bound transition. Load-bearing for supersede/#337 — a
    -- NOT NULL here would erase the "who asked for this" distinction.
    intent_id                   TEXT,
    reason                      TEXT NOT NULL DEFAULT '',
    -- The person's placement when the transition opened. Deliberately NOT
    -- `from_department_id`, even though `to_department_id` sits right below it:
    -- `staffing_history.from_department_id` already means "the unit they left",
    -- and this means "where they were when this opened". Two meanings under one
    -- name across two tables is worse than an inconsistent prefix inside one.
    --
    -- There was a `from_home` + `from_assigned` pair here until the loan
    -- concept died. Neither half is declared any more; this one column, under
    -- this name, is the whole record of where the person was.
    placement_department_id     TEXT,
    -- #751-P9: `from_pane_department_id` is GONE. It recorded which terminal
    -- WINDOW the person's pane was drawn in when the transition opened — the
    -- head-in-parent display rule, persisted. The rule now lives only in the
    -- operator client, which derives it from the CURRENT department tree, so
    -- there is nothing left to store. Dropped from the DDL exactly like
    -- `people.start_active` and `people.work_monitoring` before it (a retired
    -- COLUMN, not a retired table): the column is nullable and no code names it
    -- in any INSERT/SELECT, so a historical database keeps an inert column that
    -- nothing writes and nothing reads, and a fresh one never grows it. NO
    -- `DROP TABLE`/`CREATE TABLE` pair under the same name — this file runs on
    -- EVERY open, so that shape is a standing wipe, not a migration.
    to_department_id            TEXT,
    requested_at                TEXT NOT NULL,
    handoff_deadline_at         TEXT,
    applied_at                  TEXT,
    cancelled_at                TEXT,
    forced_at                   TEXT,
    abandoned_at                TEXT,
    -- delta #49: NO FK (slug, person_id) -> people, mirroring staffing_history
    -- (delta #15). transitions is ACTIVITY-store state; the manifest publish
    -- (a different store) removes a person from `people`, and the seam contract
    -- forbids it raw-deleting another store's rows in diff_people. A deferred
    -- people-FK here fired at COMMIT on recursive removal (person deleted,
    -- their park/offboard transition still referencing) -> mislabeled
    -- "corrupt store: company-db". The activity store prunes a removed person's
    -- transitions via its OWN typed accessor composed into the removal/offboard
    -- op; an orphan transition is tolerated on read like departed staffing_history.
    PRIMARY KEY (slug, id)
);
-- Exactly one ACTIVE transition per person, schema-enforced. Positive status
-- list (never a negated-terminal predicate: a future terminal status must not
-- wedge a person by counting as open) — same doctrine as one_open_transition.
CREATE UNIQUE INDEX IF NOT EXISTS transitions_one_active
    ON transitions(slug, person_id)
    WHERE status IN ('awaiting_handoff','overdue','ready');

CREATE TABLE IF NOT EXISTS person_activity(
    slug                        TEXT NOT NULL,
    person_id                   TEXT NOT NULL,
    last_desired_active         INTEGER CHECK(last_desired_active IN (0,1)),
    -- The instant this person's quiet lease began: normally the agent's own
    -- explicit settle, or the operator's explicit start time for the fresh
    -- lease that prevents an old run's silence from being inherited. NULL
    -- while the person is working, and NULL when no lease has begun.
    -- Split from `agent_active_at` because a single NULL there conflated
    -- "never said anything" with "said it finished", and the settle countdown
    -- must treat those differently: the first starts no clock at all unless
    -- an explicit start supplies the lease, and the second starts one at the
    -- instant named here.
    agent_quiet_at              TEXT,
    idle_since                  TEXT,
    -- The last instant this person's OWN pane reported that the agent was
    -- doing something (a turn started, a message streamed, a tool ran). NULL
    -- means the pane reported settled, or has never reported at all. The quiet
    -- lease above is stamped only while this is absent or stale: before it
    -- existed, `idle_since` was stamped purely from the ABSENCE OF DURABLE
    -- DEMAND, so an agent with no open goal counted as idle while it was
    -- visibly mid-turn and could be park-admitted under its own feet.
    agent_active_at             TEXT,
    -- THE OPERATOR'S OWN WAKE INSTANT, and the floor it buys.
    -- Operator ruling, 2026-08-20: "If I tell chief to message it, it'll come
    -- back up and do the 2min settling. We need it to always do that when woken.
    -- Message or not. If woken, it needs to wait the 2 mins." A wake is a
    -- decision a person made, so for ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS
    -- after this instant nothing may park the person, withdraw their launch
    -- intent, or stop them — whether or not any message, goal or mail demand
    -- exists. Stamped by `activity::rows::release_idle_park`, the one row half
    -- of a wake. It is a FLOOR and never a ceiling: past the lease this column
    -- is inert and today's settle rules resume exactly, so nobody is pinned.
    -- NULL for a person no operator has ever woken.
    operator_wake_at            TEXT,
    -- The reconciler's last observation of where this person was. One column
    -- since the loan concept died: there was a `last_home` + `last_assigned`
    -- pair here, and neither half is declared any more.
    last_department_id          TEXT,
    -- #751-P9: `last_pane_department_id` is GONE, for the same reason and by
    -- the same mechanism as `transitions.from_pane_department_id` above. This
    -- was the worse of the two: it was the head-in-parent answer PERSISTED and
    -- READ BACK, so between a reparent and the next activity mutation the
    -- stored window named the old parent and a reader placed a head's pane in a
    -- window the tree no longer describes.
    -- N4 additive (Fable #6): PersonActivityState scalar VALUEs with no prior
    -- column. last_employment_state/last_operational are closed-vocab scalars.
    last_employment_state       TEXT CHECK(last_employment_state IN ('active','benched','departed')),
    last_operational            INTEGER CHECK(last_operational IN (0,1)),
    -- N4 (Fable #6): a nullable pointer at the person's ACTIVE transition
    -- (name matches the ledger's activeTransitionId). NO FK ON PURPOSE (delta-#2
    -- style): it legitimately points at an `applied` (inheritable park) or
    -- `forced` transition, both OUTSIDE the transitions_one_active partial index,
    -- and multiple such terminal rows can exist per person — so an FK is wrong
    -- and the value is NOT derivable from the index. validate() owns integrity.
    active_transition_id        TEXT,
    updated_at                  TEXT NOT NULL,
    PRIMARY KEY (slug, person_id)
);

-- #751-P4: the REFLECTION concept is RETIRED, and with it its two tables. A
-- reflection was a bounded handoff payload (summary/learning/handoff/artifacts/
-- openCommitments) an agent wrote via an `org_reflect` tool before parking,
-- benching, transferring or offboarding; `reflection_handoffs` held the head
-- and `reflection_handoff_items` the ordered artifact/commitment children.
-- Nothing in the product may be called a reflection or pretend one occurred, so
-- the payload is deleted rather than left dormant: no code reads or writes
-- these rows anymore (see store/activity/rows.rs), and no reader survives them.
-- The graceful-transition state machine itself is untouched and load-bearing —
-- an `applied` transition still sheds launch intent and tears the pane down —
-- it simply records that the transition was RELEASED, never what was said.
--
-- Historical databases cross an idempotent migration boundary exactly like the
-- daemon-health, provider-pool and company_removal* slices above. ITEMS FIRST:
-- reflection_handoff_items carries a FOREIGN KEY onto reflection_handoffs, so
-- dropping the parent first would fail under `PRAGMA foreign_keys = ON`.
DROP TABLE IF EXISTS reflection_handoff_items;
DROP INDEX IF EXISTS reflection_handoffs_person;
DROP TABLE IF EXISTS reflection_handoffs;

-- Who is allowed to run (per-node launch-intent fence). Presence == intent.
-- delta #8: NO people FK ON PURPOSE — intent rows are cleared
-- reactively AFTER a person is removed (cleanup ordering), so a plain FK would
-- either block the removal or cascade the fence away too early. validate()
-- runs an orphan check (rows whose person_id no longer exists) instead.
CREATE TABLE IF NOT EXISTS launch_intent(
    slug      TEXT NOT NULL,
    person_id TEXT NOT NULL,
    initiator_person_id TEXT,
    reason TEXT,
    started_at TEXT,
    PRIMARY KEY (slug, person_id)
);

-- N4 activity aggregate metadata (Fable #6).
-- `next_transition_sequence` is DELIBERATELY NOT a column — it is allocated as
-- a D2 per-slug counter row ('transitions:<slug>' in `counters`, bumped
-- in-txn, NEVER MAX(seq)+1). No `updated_at` (that would make every activity
-- mutation touch one hot row); derive it from max(org_events.at).
CREATE TABLE IF NOT EXISTS activity_meta(
    slug                  TEXT PRIMARY KEY,
    automatic_park_cursor INTEGER NOT NULL DEFAULT 0,
    created_at            TEXT NOT NULL
);

-- N3-reminders sub-slice (CLASS-A confirmed on live cobalt: no native reminders
-- table exists; the blob is sole authority; /v1/reminders/* are façades over
-- ledger.reminders — a table is warranted, not a #493 third home). `prompt` is
-- the opaque delivered-text VALUE; status/recurring are closed vocabs; order is
-- DERIVED (created_at,id) — no ordinal, no child tables, no counters.
CREATE TABLE IF NOT EXISTS reminders(
    slug                 TEXT NOT NULL,
    id                   TEXT NOT NULL,
    person_id            TEXT NOT NULL,
    created_by_person_id TEXT NOT NULL,
    prompt               TEXT NOT NULL,      -- opaque delivered-text VALUE
    interval_ms          INTEGER NOT NULL,
    next_due_at          TEXT NOT NULL,
    status               TEXT NOT NULL CHECK(status IN ('active','stopped')),
    recurring            INTEGER NOT NULL CHECK(recurring IN (0, 1)),
    fire_count           INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    last_fired_at        TEXT,               -- nullable: absent until first fire
    expires_at           TEXT,               -- nullable optional expiry
    -- N3 additive (Fable): stop AUDIT (reminders.rs) — written when a reminder
    -- leaves 'active'. Closed vocab CHECK. Both nullable (unset while active).
    stopped_reason       TEXT CHECK(stopped_reason IS NULL OR
                           stopped_reason IN ('expired','fired','stopped')),
    stopped_at           TEXT,               -- ISO-8601 of the stop
    PRIMARY KEY (slug, id)
);
CREATE INDEX IF NOT EXISTS reminders_person ON reminders(slug, person_id);

-- SupervisionLedger metadata singleton (activity_meta precedent, Fable #6).
CREATE TABLE IF NOT EXISTS supervision_meta(
    slug                  TEXT PRIMARY KEY,
    created_at            TEXT NOT NULL
    -- NO updated_at (hot-row contention; derive MAX(org_events.at)) — activity_meta precedent.
);

-- ---- Session maintenance (replaces the session-maintenance blob) ---------
-- N5 additive-D1-clean expansion (Fable-routed per-slice). The blob's
-- SessionMaintenanceRequest becomes real scalar VALUE columns here; the
-- CompanySessionAction sub-aggregate becomes the two child tables below; the
-- blob's requestOrder / companyActionOrder / per-action targetOrder become
-- per-slug `ordinal` columns. `requestIds[]` on a target is DERIVED, not a
-- table: maintenance_requests WHERE company_action_id = ? AND person_id = ?
-- ORDER BY ordinal. No column stores structure (D1); `automatic`/`force` are
-- 0/1 flags; process ids are INTEGER; ids/tokens/prose are TEXT.

-- CompanySessionAction sub-aggregate (human whole-fleet fanout) — declared
-- first so maintenance_requests.company_action_id can name it.
-- TOMBSTONE, 2026-08-24: the company-session-action feature's tables.
--
-- `maintenance_company_action_targets` and `maintenance_request_models` are
-- DROPPED. The first held #54's company-wide fanout targets; the second held
-- `set_model`'s requested provider and model. Both are CHILD tables — nothing
-- surviving points at them — and the operator ruled the whole feature out.
-- Nothing migrates: a fanout target describes an action that can no longer be
-- requested, and a request model describes a model change chief no longer
-- performs, because Pi owns an agent's model.
--
-- `maintenance_company_actions` IS DELIBERATELY KEPT, empty and unwritten, and
-- this is the part worth reading before anyone tidies it away.
--
-- `maintenance_requests` SURVIVES — the automatic compaction still writes it —
-- and it carries `company_action_id` with a FOREIGN KEY onto this table. Under
-- `PRAGMA foreign_keys=ON`, which this schema sets, **SQLite resolves the
-- parent table when the INSERT EXECUTES, before it ever considers whether the
-- child key is NULL.** So dropping this parent does not leave an inert dangling
-- reference: it makes every insert into `maintenance_requests` fail with
-- `no such table: main.maintenance_company_actions`, which would silently
-- disable automatic compaction while leaving every read working. Measured
-- rather than reasoned about, after the reasoning got it wrong — twice, by two
-- people, on 3.40.1 through two different bindings (Python's `sqlite3` module
-- and the `sqlite3(1)` CLI). That is two BINDINGS and one LIBRARY, so it proves
-- the behaviour and not its version-independence; SQLite documents parent-table
-- resolution at DML prepare under `foreign_keys=ON` as long-standing rather
-- than a 3.40 quirk, and the product bundles its own SQLite through rusqlite,
-- so the version that matters here is the tree's and not the host's.
--
-- The column cannot simply be dropped either: `ALTER TABLE ... DROP COLUMN`
-- refuses a column named in a foreign-key definition — `unknown column
-- "company_action_id" in foreign key definition` — and still refuses after the
-- index on it is dropped. So the alternatives were a full rebuild of
-- `maintenance_requests`, the one table the surviving compaction depends on, or
-- keeping one empty table. The empty table is far cheaper and can be paid back
-- at leisure by anyone who wants to rebuild that table for other reasons.
--
-- AND A SECOND REASON, WHICH MAKES THE DECISION ROBUST RATHER THAN MERELY
-- CORRECT ON TODAY'S BOX. This repo does not write migrations, and
-- `CREATE TABLE IF NOT EXISTS` never RESHAPES a table that already exists — so
-- an existing company keeps `maintenance_requests` exactly as it was, foreign
-- key included, whatever this file says. Keeping the parent is therefore the
-- only disposition that works on a FRESH company and an EXISTING one at the
-- same time, with no compatibility layer and no branch. Dropping it would be
-- correct for neither.
--
-- WHAT WOULD MAKE THIS WRONG: nothing writes `company_action_id` non-NULL any
-- more, and nothing can — the verbs, the routes and the store that produced
-- company actions are all deleted. If a writer for that column ever returns,
-- this table stops being a harmless empty parent and becomes a live dependency
-- again, and whoever adds that writer owns the question.
DROP TABLE IF EXISTS maintenance_company_action_targets;
DROP TABLE IF EXISTS maintenance_request_models;

-- KEPT, EMPTY AND UNWRITTEN. See the tombstone above for why this table
-- outlives the feature that filled it: `maintenance_requests` FOREIGN KEYs onto
-- it, and under `PRAGMA foreign_keys=ON` the parent must EXIST for an insert
-- into the child to run at all, NULL child key or not. A fresh database needs
-- it for exactly the same reason an existing one does, so it is created rather
-- than merely left behind.
CREATE TABLE IF NOT EXISTS maintenance_company_actions(
    slug              TEXT NOT NULL,
    id                TEXT NOT NULL,
    ordinal           INTEGER NOT NULL,
    action            TEXT NOT NULL,
    force             INTEGER NOT NULL CHECK(force IN (0,1)),
    requested_by      TEXT NOT NULL,
    requested_at      TEXT NOT NULL,
    PRIMARY KEY (slug, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS maintenance_company_actions_ordinal
    ON maintenance_company_actions(slug, ordinal);

CREATE TABLE IF NOT EXISTS maintenance_requests(
    slug         TEXT NOT NULL,
    id           TEXT NOT NULL,
    ordinal      INTEGER NOT NULL,           -- replaces requestOrder (append-chronological)
    person_id    TEXT NOT NULL,
    requested_by TEXT NOT NULL,              -- person id | 'operator' | 'human' sentinels
    -- delta #7: `action` is DELIBERATELY free TEXT — the maintenance kinds are
    -- an open, extension-supplied vocabulary (fresh-session, credential
    -- re-apply, …); a CHECK would force a schema change per new kind. It is a
    -- controlled label, never parsed structure, so the no-JSON gate suffices.
    action       TEXT NOT NULL,
    status       TEXT NOT NULL CHECK(status IN
                   ('queued','running','applying','completed','failed','skipped')),
    reason       TEXT NOT NULL DEFAULT '',
    automatic    INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1)),
    attempt                   INTEGER,
    recovered_from_request_id TEXT,
    retry_not_before          TEXT,
    force                     INTEGER CHECK(force IN (0,1)),
    company_action_id         TEXT,
    -- live claim fence (survives process death for crash recovery)
    claimed_process_id  INTEGER,
    claimed_session_id  TEXT,
    claim_token         TEXT,
    -- completion claim (company native-reset target proof)
    completed_process_id   INTEGER,
    completed_session_id   TEXT,
    completion_claim_token TEXT,
    -- forced-interrupt receipt (#319)
    interrupted_process_id  INTEGER,
    interrupted_session_id  TEXT,
    interrupted_claim_token TEXT,
    interrupted_at          TEXT,
    -- native compact anchor / proof
    compact_session_id            TEXT,
    compact_anchor_entry_id       TEXT,
    completed_compaction_entry_id TEXT,
    requested_at TEXT NOT NULL,
    started_at   TEXT,
    settled_at   TEXT,                        -- completedAt (terminal timestamp)
    error        TEXT,
    PRIMARY KEY (slug, id),
    FOREIGN KEY (slug, company_action_id) REFERENCES maintenance_company_actions(slug, id)
);
CREATE INDEX IF NOT EXISTS maintenance_requests_person ON maintenance_requests(slug, person_id);
CREATE UNIQUE INDEX IF NOT EXISTS maintenance_requests_ordinal
    ON maintenance_requests(slug, ordinal);
CREATE INDEX IF NOT EXISTS maintenance_requests_company_action
    ON maintenance_requests(slug, company_action_id);

-- Value payload for the one value-bearing session action. A child table keeps
-- existing maintenance request rows stable while new and existing company
-- databases both receive the current schema through CREATE IF NOT EXISTS.
-- (see the TOMBSTONE above: all three tables are dropped at the CREATEs
-- they occupied.)

-- Per-slug maintenance metadata. This is only the existence/timestamp witness
-- for an otherwise empty normalized aggregate; transaction-local SQL event
-- sequencing serializes writes. It is deliberately not a revision ledger.
CREATE TABLE IF NOT EXISTS maintenance_ledger(
    slug       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (slug)
);

-- ---- The change feed (replaces the global revision's one legitimate job) --
-- Strictly-monotonic per-slug seq with entity granularity. Watchers (SSE,
-- footer, materialization staleness, converge triggers) read "rows newer than
-- seq" instead of blind full re-reads.
--
-- D2 (fable-arch RULING 2026-07-25, FROZEN): `seq` is a PER-SLUG COUNTER row
-- bumped in the SAME `BEGIN IMMEDIATE` transaction as the mutation — NEVER
-- MAX(seq)+1, NEVER global AUTOINCREMENT. The counter is a dedicated row keyed
-- 'org-events:<slug>' (in `counters`, or an org_event_counters table).
-- SQLite's single-writer lock makes commit order == seq order, so the feed is
-- gap-free and totally ordered per slug (kills the #286 seq-race at the
-- source). CONTRACT: this table is truth; the SSE socket is only a nudge —
-- every consumer re-reads from its last-acked seq, never trusts the wire.
CREATE TABLE IF NOT EXISTS org_events(
    slug       TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    entity     TEXT NOT NULL,
    entity_id  TEXT NOT NULL,
    op         TEXT NOT NULL,
    actor      TEXT NOT NULL DEFAULT '',
    at         TEXT NOT NULL,
    -- delta #9: a REFERENCE to the owning row in the format 'table:pk'
    -- (e.g. 'transitions:acme/t-42'). NEVER inline structure — the detail is
    -- the row it points at, read separately; keeps the feed a thin index.
    detail_ref TEXT,
    PRIMARY KEY (slug, seq)
);
CREATE INDEX IF NOT EXISTS org_events_entity ON org_events(slug, entity, entity_id);
-- THE TWO `MAX(at)` READS, AND WHY BOTH INDEXES ARE REQUIRED.
--
-- Two statements ask this table for the latest timestamp, and both run on
-- `/v1/org/activity/read` and `/v1/org/runtime/desired` — the routes the
-- actuator and every rail poll:
--
--   1. `MAX(at) WHERE slug = ?`                       (activity ledger)
--   2. `MAX(at) WHERE slug = ? AND entity IN (...)`   (manifest reconstruct)
--
-- `at` appeared in no index, so (1) was served from `org_events_entity` and
-- walked every row the company had ever written. Measured on the operator's
-- database — 322,329 rows — that ONE statement was 349-454ms, and it was the
-- single most expensive thing the daemon did.
--
-- Adding only `(slug, at)` fixes (1) and REGRESSES (2) from 0.186ms to 1.9ms,
-- because the planner then answers (2) by walking the `at`-ordered index
-- backwards looking for a matching entity — and the manifest entities are the
-- rare ones in this feed, so it walks a long way. Adding only
-- `(slug, entity, at)` fixes (2) and leaves (1) at 29.8ms. Measured, all four
-- combinations, on the operator's own database:
--
--                       filtered (2)   unfiltered (1)
--   no index               0.186ms        349.050ms
--   (slug, at)             1.902ms          0.008ms
--   (slug, entity, at)     0.021ms         29.831ms
--   both                   0.020ms          0.008ms
--
-- So both, and neither is redundant. Each is `(slug, ...)`-prefixed because
-- every read is scoped to one company; a bare `at` index would make MAX scan
-- across companies.
--
-- The cost is write amplification on an append-heavy table. It is worth it and
-- it is small: this company commits about 0.26 writes/second and serves
-- several reads/second, and each of those reads was paying a third of a second.
CREATE INDEX IF NOT EXISTS org_events_at ON org_events(slug, at);
CREATE INDEX IF NOT EXISTS org_events_entity_at ON org_events(slug, entity, at);

-- ---- Typed singletons (one row each; real columns; no JSON) --------------
-- TOMBSTONE (chief-home-is-cwd §4c): `boot_lease(slug, socket, entered_at,
-- token)` stood here and held the `ceo-boot-lease` — the exclusivity window an
-- attended CEO-only boot claimed so chiefd's reconcile duty would not create or
-- replace the CEO pane while that command prepared its own projection. The
-- daemon boots no pane now, so there is no attended pre-converge phase to fence
-- and no writer left to take the lease. Serialization of WRITES never came from
-- here and is unchanged: one daemon per company (beacond), one writer actor per
-- daemon, one durable single-flight claim per converge pass.
CREATE TABLE IF NOT EXISTS session_epoch(
    slug   TEXT PRIMARY KEY,
    at     TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS quiesce(
    slug  TEXT PRIMARY KEY,
    since TEXT NOT NULL
);
-- THE OPERATOR'S OFF SWITCH, and the only durable company-level one.
--
-- A live company was given an explicit stand-down. The CEO obeyed it exactly:
-- it stopped and parked six people and reported `Stood down 6 people`. Forty-
-- five seconds later all six were back up with fresh panes and brand-new
-- contexts, because a person's pending mail grants launch intent by itself
-- (`converge_apply/cycle.rs`), and the only defence was a per-person watermark
-- derived from that person's own stop transition — which ANY later message,
-- reminder or queued maintenance request defeats. Parking six people leaves in
-- place exactly the demand that relaunches them.
--
-- There was nowhere for "the operator told this company to stop working" to
-- live. `runtime.status` means the runtime is not up; `quiesce` is the CEO
-- reset watermark; `converge_safety` is a failure breaker. None of them is an
-- operator decision about whether the company works.
--
-- A row here means it is stood down. While it exists NOTHING grants launch
-- intent — not mail, not session maintenance, not a start, a wake, a hire or a
-- department creation — so the fence stays as the stand-down left it and the
-- only way out is an explicit resume. Pending mail is HELD, never dropped: no
-- mailbox row is touched, and lifting the stand-down lets the ordinary wake
-- grant those people again with their mail intact.
CREATE TABLE IF NOT EXISTS stand_down(
    slug   TEXT PRIMARY KEY,
    since  TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT ''
);
-- event-journal port (org-event-journal.ts `maybeSweepJournalMarkers`): the
-- per-company sweep throttle stamp, now durable SQL state instead of the TS
-- process-local `Map<slug, lastSweptMs>` (Mandate 2 forbids process-local
-- state). One row per company; `last_swept_at_ms` is stamped BEFORE the prune
-- runs so a persistently failing prune cannot hot-loop the DELETE.
CREATE TABLE IF NOT EXISTS event_journal_sweep(
    slug             TEXT PRIMARY KEY,
    last_swept_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS runtime_owner(
    slug         TEXT PRIMARY KEY,
    -- delta #24 (N9 B4): socket/claimed_at nullable — the store carries a
    -- released owner (no live socket) as a real state, not an absent row.
    --
    -- `socket` STAYS, and it is the record's whole identity. chiefd stores this
    -- string, compares it for equality, and never parses it: it is the operator
    -- client's own opaque handle for where it projects the company. AC6 retired
    -- the `session TEXT NOT NULL` column that stood beside it; it is absent from
    -- this DDL, and a company database never grows it.
    socket       TEXT,
    claimed_at   TEXT,
    -- delta #24 (N9 B4): the runtime-owner store's real lifecycle fields.
    status       TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','released')),
    validated_at TEXT,
    released_at  TEXT
);
-- delta #24 (N9 B4): operator-escalation-intents — Map<fingerprint, intent>.
CREATE TABLE IF NOT EXISTS operator_escalation_intents(
    slug            TEXT NOT NULL,
    fingerprint     TEXT NOT NULL,
    person_id       TEXT NOT NULL,
    blocker         TEXT NOT NULL,
    operator_action TEXT NOT NULL,
    queued_at       TEXT NOT NULL,
    PRIMARY KEY (slug, fingerprint)
);
-- The out-of-band operator-escalation LOG. This replaces the
-- `logs/operator-escalations.jsonl` append-only file the TypeScript producer
-- wrote (Mandate 5: the only thing on disk is Pi's home, everything else is a
-- row). The fingerprint is the record's whole identity, so the primary key IS
-- the FILE-tier dedup the marker + append pair used to provide: a distinct
-- blocker lands exactly once and forever, and a replay of the same blocker
-- inserts nothing.
CREATE TABLE IF NOT EXISTS operator_escalation_log(
    slug            TEXT NOT NULL,
    fingerprint     TEXT NOT NULL,
    kind            TEXT NOT NULL,
    person_id       TEXT NOT NULL,
    blocker         TEXT NOT NULL,
    operator_action TEXT NOT NULL,
    queued_at       TEXT NOT NULL,
    recorded_at     TEXT NOT NULL,
    PRIMARY KEY (slug, fingerprint)
);
-- delta #24 (N9 B4): operator-escalation-push singleton (last push + pending).
CREATE TABLE IF NOT EXISTS operator_escalation_push(
    slug               TEXT PRIMARY KEY,
    last_pushed_at     TEXT,
    pending_text       TEXT,
    pending_fingerprint TEXT,
    pending_attempts   INTEGER
);
-- delta #24 RETRACTED: the supervisor-armed-intent singleton described a SECOND
-- OS process — `pid` + `process_start` are there so a `kill(pid, 0)` and a
-- starttime match could prove the armed supervisor alive. It armed the handover
-- fence for the detached org-supervisor, which #825 retired (809c402a5) and
-- 5681617a4 deleted along with org-armed-supervisor.ts. It was dead on both
-- ends: no producer, and no consumer but its own tests. Dropped idempotently;
-- nothing migrates, because an intent to arm a process that cannot exist has no
-- successor to carry it to.
DROP TABLE IF EXISTS supervisor_armed_intent;
-- #1047: the goal, acknowledgement-receipt, runtime-generation, memory and
-- learned-skill features are deleted outright, not deprecated. Their tables are
-- dropped idempotently rather than merely un-created: `CREATE TABLE IF NOT
-- EXISTS` says nothing about a database that already has them, so a company
-- opened after the deletion would otherwise carry the rows of a feature no code
-- can read, forever. Nothing migrates -- there is no successor keyspace for a
-- goal, an ack receipt, a generation, a memory record or a learned skill.
DROP TABLE IF EXISTS manager_goals;
DROP TABLE IF EXISTS delegated_goals;
DROP TABLE IF EXISTS goal_watches;
DROP TABLE IF EXISTS goal_intents;
DROP TABLE IF EXISTS manager_check_ins;
DROP TABLE IF EXISTS assignments;
DROP TABLE IF EXISTS ack_receipts;
DROP TABLE IF EXISTS runtime_generations;
DROP TABLE IF EXISTS fresh_session_transitions;
DROP TABLE IF EXISTS memory_records;
DROP TABLE IF EXISTS memory_open_commitments;
DROP TABLE IF EXISTS memory_review;
DROP TABLE IF EXISTS memory_review_jobs;
DROP TABLE IF EXISTS learned_skills;
DROP TABLE IF EXISTS learned_skill_aliases;
DROP TABLE IF EXISTS learned_skill_fingerprints;
DROP TABLE IF EXISTS skill_versions;
DROP TABLE IF EXISTS skill_version_evidence;
DROP TABLE IF EXISTS skill_candidates;
DROP TABLE IF EXISTS skill_candidate_evidence;
DROP TABLE IF EXISTS skill_candidate_score_components;
DROP TABLE IF EXISTS skill_candidate_counter;
-- RETRACTED: `supervisor_state`-the-watermark was superseded by the per-duty
-- `supervisor_watermarks` rows below and never carried a single statement — no
-- SELECT, no INSERT, no DELETE anywhere in the tree. A table nothing reads and
-- nothing writes is a name reserved against a concept that already moved.
DROP TABLE IF EXISTS supervisor_state;
-- delta #31 RETRACTED (blob-death, supervisor-state port): the `supervisor-state`
-- document described the detached org-supervisor PROCESS — socket, token, pid,
-- process_start, heartbeat — and its only writer was org-supervisor-state.ts,
-- whose `supervisorProcessIsLive` proved that process alive with `kill(pid, 0)`
-- plus a starttime match. #825 retired the process (809c402a5) and 5681617a4
-- deleted the writer; supervision is now duties inside the one daemon, which has
-- no second process to describe. The readers that outlived the writer read a row
-- that could never appear: both supervisor-gated refusals in `audit_ownership`
-- were unreachable, `cold_start`'s stopped proof lost an arm, and
-- `RuntimeLiveness` was pinned to `Stopped`. Dropped idempotently at the
-- position the CREATEs occupied. Nothing migrates: the scalar half describes a
-- dead pid and the child half is a bounded forensic tail no decision was ever
-- taken from.
--
-- BOTH child names are dropped. `supervisor_runtime_events` is the pre-P9 name
-- (P9 retired it for `supervisor_runtime_event_log` because the `kind` CHECK
-- changed and `CREATE TABLE IF NOT EXISTS` keeps the constraint it was created
-- with), and `supervisor_runtime_event_log` is the name P9 gave it. A database
-- from either era must end up with neither, and neither name is ever recreated.
DROP TABLE IF EXISTS supervisor_runtime_events;
DROP TABLE IF EXISTS supervisor_runtime_event_log;
DROP TABLE IF EXISTS supervisor_process_state;
CREATE TABLE IF NOT EXISTS health(
    slug       TEXT PRIMARY KEY,
    status     TEXT NOT NULL,
    -- delta #10: opaque prose VALUE only (a human-readable reason). This is a
    -- classic spot where a JSON blob sneaks back in — the no-JSON CI gate
    -- MUST cover it; if health ever needs structure, add typed columns.
    detail     TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
);
-- delta #67: the delta-#10 stub predated the real ConvergeSafetyState — it
-- lacked the breaker/cycle/refusal fields the reconcile gate actually reads.
-- Replaced (fresh cobalt, no data → no migration) with the full columnar shape.
CREATE TABLE IF NOT EXISTS converge_safety(
    slug                 TEXT PRIMARY KEY,
    actuation_mode       TEXT NOT NULL CHECK(actuation_mode IN ('shadow','apply')),
    sweep_live           INTEGER NOT NULL CHECK(sweep_live IN (0,1)),
    budget_override      INTEGER NOT NULL CHECK(budget_override IN (0,1)),
    consecutive_failures INTEGER NOT NULL CHECK(consecutive_failures >= 0),
    breaker_tripped      INTEGER NOT NULL CHECK(breaker_tripped IN (0,1)),
    breaker_tripped_at   TEXT,
    cycle_in_progress    INTEGER NOT NULL CHECK(cycle_in_progress IN (0,1)),
    cycle_started_at_ms  INTEGER,
    last_refusal_kind    TEXT,
    last_refusal_detail  TEXT,
    last_refusal_at      TEXT,
    CHECK ((last_refusal_kind IS NULL) = (last_refusal_detail IS NULL)),
    CHECK ((last_refusal_kind IS NULL) = (last_refusal_at IS NULL))
);
-- TOMBSTONE: `runtime_actuation`, `runtime_actuation_people` and
-- `runtime_actuation_unknown`. All three held ONE report: who was actuating
-- this company, when they last reported, and what they saw in tmux. The
-- observation feedback path is deleted -- chiefd holds the desired state and
-- the actuator projects it, so no host fact travels up and there is nothing to
-- persist. Dropped rather than left empty: an unused table is a standing
-- invitation to write to it again.
--
-- The `observation_trusted`/`untrusted_reason` CHECK pair here was correct and
-- is worth remembering. It held the two columns to each other precisely
-- because "untrusted, and here are zero people" is the state that must not
-- exist. The SQL never let that row be written -- and the defect happened
-- anyway, one layer up in Rust, where an untrusted record was handed onward as
-- `Some(EMPTY)`. A constraint at the storage boundary cannot protect a
-- conflation performed after the read.
-- delta #67: supervisor duty watermarks (last_success_at/run_count per duty),
-- written same-txn with health by run_health_monitor — a map keyed by duty.
-- #825-prereq: four nullable last-failure columns, same bounded-singleton
-- bounded-singleton idiom, not an append-only log — ONE row per duty
-- carries the most recent failure only (cleared to NULL on the next success),
-- never a growing log. `consecutive_failures` defaults 0 so an upgraded row
-- with no failure yet reads as zero, not NULL, keeping arithmetic total.
CREATE TABLE IF NOT EXISTS supervisor_watermarks(
    slug                  TEXT NOT NULL,
    duty                  TEXT NOT NULL,
    interval_ms           INTEGER NOT NULL CHECK(interval_ms >= 0),
    last_success_at       TEXT NOT NULL,
    run_count             INTEGER NOT NULL CHECK(run_count >= 0),
    last_failure_at       TEXT,
    last_failure_kind     TEXT,
    last_failure_detail   TEXT,
    consecutive_failures  INTEGER NOT NULL DEFAULT 0 CHECK(consecutive_failures >= 0),
    PRIMARY KEY (slug, duty)
);
"#;

/// One column a table declares, as the DDL spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredColumn {
    /// The column's name.
    pub name: String,
    /// The full declaration, ready to follow `ALTER TABLE <t> ADD COLUMN `.
    pub declaration: String,
}

/// Why a declared column cannot be added to a table that already exists.
#[derive(Debug)]
pub enum AdditiveColumnError {
    /// SQLite refused the `ALTER TABLE`, or a `PRAGMA table_info` failed.
    Database(rusqlite::Error),
    /// The column is `NOT NULL` with no `DEFAULT`. SQLite cannot add one to a
    /// populated table and neither can this: every existing row would need a
    /// value nobody has supplied. Adding it needs a real migration that says
    /// what the value is.
    NeedsMigration {
        /// The table the column was declared on.
        table: String,
        /// The column's name.
        column: String,
    },
}

impl std::fmt::Display for AdditiveColumnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "{error}"),
            Self::NeedsMigration { table, column } => write!(
                f,
                "`{table}.{column}` is declared NOT NULL with no DEFAULT, so it cannot be added \
                 to a company database that already exists: every row already written would need \
                 a value nobody has supplied. Give it a DEFAULT, make it nullable, or write a \
                 migration that states the value for existing rows."
            ),
        }
    }
}

impl std::error::Error for AdditiveColumnError {}

/// Every column each `CREATE TABLE IF NOT EXISTS` in `sql` declares, keyed by
/// table, in declaration order.
///
/// Comments are stripped first and commas are split at paren depth zero, so a
/// `CHECK(x IN ('a','b'))` is one declaration rather than two. Table-level
/// constraints — `PRIMARY KEY (...)`, `CHECK (...)`, `FOREIGN KEY`, `UNIQUE` —
/// are not columns and are dropped.
#[must_use]
pub fn declared_columns(sql: &str) -> BTreeMap<String, Vec<DeclaredColumn>> {
    let mut tables = BTreeMap::new();
    for chunk in sql.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
        let Some((name, rest)) = chunk.split_once('(') else { continue };
        let table = name.trim().to_string();
        if table.is_empty() {
            continue;
        }
        let Some(body) = table_body(rest) else { continue };
        tables.insert(table, body_columns(&body));
    }
    tables
}

/// The text between a `CREATE TABLE`'s opening paren and its matching close.
fn table_body(rest: &str) -> Option<String> {
    let mut depth = 1_i32;
    let mut body = String::new();
    for ch in rest.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(body);
                }
            }
            _ => {}
        }
        body.push(ch);
    }
    None
}

/// The column declarations in one table body.
fn body_columns(body: &str) -> Vec<DeclaredColumn> {
    // Comments carry commas and parens of their own; strip them before any
    // structural reading of the body.
    let stripped: String = body
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n");

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0_i32;
    for ch in stripped.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    parts.push(current);

    parts
        .into_iter()
        .filter_map(|part| {
            let declaration = part.split_whitespace().collect::<Vec<_>>().join(" ");
            // The leading IDENTIFIER, not the first whitespace-delimited token:
            // a table-level `CHECK(consecutive_failures >= 0)` has no space
            // after its keyword, so splitting on whitespace reads it as a
            // column named `CHECK(consecutive_failures`.
            let name: String = declaration
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            if name.is_empty() {
                return None;
            }
            let upper = name.to_ascii_uppercase();
            // Table-level constraints are not columns.
            if matches!(upper.as_str(), "PRIMARY" | "FOREIGN" | "UNIQUE" | "CHECK" | "CONSTRAINT") {
                return None;
            }
            Some(DeclaredColumn { name, declaration })
        })
        .collect()
}

/// Add every column the schema declares that an EXISTING table does not have.
///
/// # THE FAILURE THIS EXISTS TO PREVENT, MEASURED
///
/// `CREATE TABLE IF NOT EXISTS` is silent about columns. On a database where
/// the table already exists it does NOTHING — so a column added to the schema
/// arrives for companies created afterwards and never for the ones already on
/// disk. The readers select it by name regardless, and the company cannot be
/// opened at all:
///
/// ```text
/// cannot open the company database ... company journal is unreadable:
/// activity rows unreadable: no such column: operator_wake_at in
/// SELECT ..., operator_wake_at FROM person_activity WHERE slug = ?1
/// ```
///
/// That is a live box, 2026-08-20T23:40Z, after #1190 added
/// `person_activity.operator_wake_at`. Every existing company on that box was
/// bricked by an upgrade, and a fresh one worked perfectly — which is why no
/// test and no QA pass saw it. The comment above the retired-column list said
/// "no historical database is ever opened"; every company that has been running
/// for more than one release is a historical database.
///
/// # Why generic, and not one line naming that column
///
/// Naming the column fixes the company in front of you and leaves the trap
/// armed for the next one, and the trap does not fail loudly at review time —
/// it fails at the operator's terminal, after the upgrade, on every company
/// they own. The schema is the declaration of what a table has; reconciling to
/// it is what makes that declaration true for databases that already exist.
///
/// A `NOT NULL` column with no `DEFAULT` cannot be added this way and is
/// refused by name rather than attempted: see [`AdditiveColumnError`].
///
/// Returns the `table.column` names it added, in schema order — empty for a
/// database that was already current, which is the common case.
///
/// # Errors
/// [`AdditiveColumnError`].
pub fn add_missing_columns(conn: &Connection) -> Result<Vec<String>, AdditiveColumnError> {
    let mut added = Vec::new();
    for (table, columns) in declared_columns(COMPANY_SCHEMA_SQL) {
        let existing = existing_columns(conn, &table).map_err(AdditiveColumnError::Database)?;
        // An absent table is not a missing column: `CREATE TABLE IF NOT EXISTS`
        // has already run in this same open and made it, so an empty read here
        // means the table is genuinely not there (a retired one) and nothing is
        // owed.
        if existing.is_empty() {
            continue;
        }
        for column in columns {
            if existing.contains(&column.name) {
                continue;
            }
            let upper = column.declaration.to_ascii_uppercase();
            if upper.contains("NOT NULL") && !upper.contains("DEFAULT") {
                return Err(AdditiveColumnError::NeedsMigration {
                    table: table.clone(),
                    column: column.name,
                });
            }
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {};", column.declaration))
                .map_err(AdditiveColumnError::Database)?;
            added.push(format!("{table}.{}", column.name));
        }
    }
    Ok(added)
}

/// The column names a table currently has. An absent table yields none.
fn existing_columns(conn: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

/// Pragmas applied to every company connection at open.
pub const COMPANY_PRAGMAS: &[&str] =
    &["PRAGMA journal_mode=WAL", "PRAGMA synchronous=FULL", "PRAGMA foreign_keys=ON"];

/// delta #33: single-source DDL for `event_once_markers`.
///
/// The event-once marker is a cross-producer exactly-once primitive: it is
/// created by `COMPANY_SCHEMA_SQL` when a `CompanyDb` opens, but it is ALSO
/// written DocStore-direct with NO live company (the extension intercom's
/// `/v1/docs/insert-if-absent`, and bare-dir orgs that never open a CompanyDb).
/// So `DocStore::ensure_schema` must create the table too — from THIS const, so
/// the two sources are structurally identical (a mismatched second
/// `CREATE ... IF NOT EXISTS` is silently ignored and would drift the shape).
/// Column/PK set matches the `event_once_markers` block in COMPANY_SCHEMA_SQL
/// verbatim (guarded by `event_once_markers_ddl_is_single_source`).
pub const EVENT_ONCE_MARKERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS event_once_markers(
    slug            TEXT NOT NULL,
    key_digest      TEXT NOT NULL,
    id              TEXT NOT NULL,
    schema_version  INTEGER NOT NULL DEFAULT 1,
    event_type      TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    thr_message_id             TEXT,
    thr_fingerprint            TEXT,
    thr_kind                   TEXT,
    thr_incident_first_seen_at TEXT,
    thr_recipient_person_id    TEXT,
    thr_accepted_at            TEXT,
    PRIMARY KEY (slug, key_digest),
    UNIQUE (slug, id)
);
CREATE INDEX IF NOT EXISTS event_once_markers_created ON event_once_markers(slug, created_at);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // ---- additive columns on a database that already exists ----------------

    /// THE UPGRADE THAT BRICKED EVERY EXISTING COMPANY, REPRODUCED.
    ///
    /// A live box, 2026-08-20T23:40Z, on the build that added
    /// `person_activity.operator_wake_at`:
    ///
    /// ```text
    /// cannot open the company database ... activity rows unreadable:
    /// no such column: operator_wake_at
    /// ```
    ///
    /// `CREATE TABLE IF NOT EXISTS` does not add columns to a table that is
    /// already there, so the column arrived for companies created after the
    /// upgrade and never for the ones on disk. A FRESH company was perfect,
    /// which is exactly why nothing caught it.
    #[test]
    fn a_column_added_to_the_schema_reaches_a_database_that_already_exists() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("the schema applies");
        // The database as it was before the column was declared.
        conn.execute_batch("ALTER TABLE person_activity DROP COLUMN operator_wake_at;")
            .expect("age the table back one release");
        assert!(
            !existing_columns(&conn, "person_activity")
                .expect("read columns")
                .contains("operator_wake_at"),
            "precondition — the aged table must not have the column"
        );

        // What an open does: the DDL, then the reconcile.
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("the schema re-applies");
        assert!(
            !existing_columns(&conn, "person_activity")
                .expect("read columns")
                .contains("operator_wake_at"),
            "CREATE TABLE IF NOT EXISTS is SILENT about columns — this is the whole defect, and \
             if this assertion ever fails the reconcile below is no longer load-bearing"
        );

        let added = add_missing_columns(&conn).expect("the reconcile applies");
        assert!(
            existing_columns(&conn, "person_activity")
                .expect("read columns")
                .contains("operator_wake_at"),
            "the reconcile must add the declared column"
        );
        assert!(
            added.contains(&"person_activity.operator_wake_at".to_string()),
            "and must report what it added: {added:?}"
        );
        conn.query_row("SELECT COUNT(operator_wake_at) FROM person_activity", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("the reader's own column is selectable");
    }

    /// A database that is already current is not written to. The common case is
    /// every open after the first, and a reconcile that rewrote the schema on
    /// each one would be a standing DDL write on a hot path.
    #[test]
    fn a_current_database_has_nothing_added() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("the schema applies");
        assert_eq!(add_missing_columns(&conn).expect("reconcile"), Vec::<String>::new());
        assert_eq!(
            add_missing_columns(&conn).expect("reconcile again"),
            Vec::<String>::new(),
            "and it is idempotent"
        );
    }

    /// A `NOT NULL` column with no `DEFAULT` cannot be added to a populated
    /// table by anybody — every row already written would need a value nobody
    /// has supplied. It is refused BY NAME, so the next person to declare one
    /// reads a sentence instead of `SqliteFailure(1, "Cannot add a NOT NULL
    /// column with default value NULL")`.
    #[test]
    fn a_not_null_column_with_no_default_is_refused_by_name() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS t(a TEXT NOT NULL); \
             CREATE TABLE IF NOT EXISTS u(a TEXT NOT NULL, b TEXT NOT NULL);",
        )
        .expect("fixture");
        let declared = declared_columns(
            "CREATE TABLE IF NOT EXISTS u(\n a TEXT NOT NULL,\n b TEXT NOT NULL\n);",
        );
        assert_eq!(
            declared["u"].iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        // The refusal itself, through the real accessor's own predicate.
        let error =
            AdditiveColumnError::NeedsMigration { table: "u".to_string(), column: "b".to_string() };
        let sentence = error.to_string();
        assert!(sentence.contains("`u.b`"), "{sentence}");
        assert!(sentence.contains("migration"), "{sentence}");
    }

    /// The parser reads COLUMNS, not the table-level constraints that sit
    /// beside them and not the commas inside a `CHECK`.
    #[test]
    fn the_declaration_parser_reads_columns_and_not_constraints() {
        let tables = declared_columns(COMPANY_SCHEMA_SQL);
        let activity: Vec<&str> =
            tables["person_activity"].iter().map(|c| c.name.as_str()).collect();
        assert!(activity.contains(&"operator_wake_at"), "{activity:?}");
        assert!(activity.contains(&"agent_quiet_at"), "{activity:?}");
        for constraint in ["PRIMARY", "FOREIGN", "UNIQUE", "CHECK", "CONSTRAINT"] {
            assert!(
                !activity.iter().any(|name| name.eq_ignore_ascii_case(constraint)),
                "a table-level {constraint} is not a column: {activity:?}"
            );
        }

        // A CHECK's own commas must not split one column into several. The
        // mailbox state vocabulary is the sharpest case in the tree.
        let mailbox: Vec<&str> = tables["mailbox"].iter().map(|c| c.name.as_str()).collect();
        assert!(mailbox.contains(&"state"), "{mailbox:?}");
        assert!(
            !mailbox.iter().any(|name| name.starts_with('\'')),
            "a CHECK's enum values must never be read as column names: {mailbox:?}"
        );
        let state = tables["mailbox"].iter().find(|c| c.name == "state").expect("the state column");
        assert!(
            state.declaration.contains("'superseded'"),
            "the whole CHECK belongs to the declaration: {}",
            state.declaration
        );
    }

    /// Every table the schema declares is reconciled, not a hand-kept subset.
    /// A list somebody has to remember to append to is the defect this replaces.
    #[test]
    fn every_declared_table_is_covered() {
        let tables = declared_columns(COMPANY_SCHEMA_SQL);
        let declared_in_sql = COMPANY_SCHEMA_SQL.matches("CREATE TABLE IF NOT EXISTS ").count();
        assert_eq!(
            tables.len(),
            declared_in_sql,
            "every CREATE TABLE must yield a parsed table: {:?}",
            tables.keys().collect::<Vec<_>>()
        );
        assert!(
            tables.values().all(|columns| !columns.is_empty()),
            "no table parses to zero columns"
        );
    }

    #[test]
    fn event_once_markers_ddl_is_single_source() {
        // delta #33: DocStore::ensure_schema creates event_once_markers from
        // EVENT_ONCE_MARKERS_DDL (bare-dir / no-live-company marker writes);
        // COMPANY_SCHEMA_SQL creates it on CompanyDb open. Both must agree —
        // an idempotent second CREATE keeps the FIRST shape, so a drift would
        // silently corrupt the cross-producer marker. Guard the load-bearing
        // structure (table + slug-scoped PK + unique) is identical in both.
        for token in [
            "CREATE TABLE IF NOT EXISTS event_once_markers(",
            "PRIMARY KEY (slug, key_digest)",
            "UNIQUE (slug, id)",
            "event_once_markers_created ON event_once_markers(slug, created_at)",
        ] {
            assert!(
                EVENT_ONCE_MARKERS_DDL.contains(token),
                "EVENT_ONCE_MARKERS_DDL missing: {token}"
            );
            assert!(
                COMPANY_SCHEMA_SQL.contains(token),
                "COMPANY_SCHEMA_SQL drifted from EVENT_ONCE_MARKERS_DDL: {token}"
            );
        }
    }

    #[test]
    fn effects_sequence_is_slug_scoped_with_a_counter_so_pruned_seqs_are_never_reused() {
        // delta #36: effects is slug-scoped (shared org.sqlite), so the global
        // AUTOINCREMENT (which collided across companies) is dropped for
        // PK (slug, seq); the per-company NEXT_EFFECT_SEQUENCE counter is now the
        // sole no-reuse authority (org-supervision-state.ts:815 rule).
        assert!(COMPANY_SCHEMA_SQL.contains("PRIMARY KEY (slug, seq)"));
        assert!(!COMPANY_SCHEMA_SQL.contains("seq          INTEGER PRIMARY KEY AUTOINCREMENT"));
        assert!(COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS counters"));
        // counters stays name-PK: per-company scoping is via the name (D2
        // `<counter>:<slug>`), NOT a slug column (a column breaks allocate_seq).
    }

    #[test]
    fn the_provider_admission_pool_is_gone_from_the_schema_and_retired_on_open() {
        // #748: a dropped mechanism that still has a table is a mechanism
        // waiting to be re-wired. The DDL is the last place the pool could
        // survive, and historical databases must cross the idempotent
        // migration boundary that removes only the retired pool state.
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS provider_slots"),
            "the provider_slots table is still declared"
        );
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS provider_reservations"),
            "the provider_reservations table is still declared"
        );
        assert!(
            COMPANY_SCHEMA_SQL.contains("DROP TABLE IF EXISTS provider_slots"),
            "historical databases must drop the retired pool table"
        );
        assert!(
            COMPANY_SCHEMA_SQL.contains("DROP TABLE IF EXISTS provider_reservations"),
            "historical databases must drop the retired reservation table"
        );
    }

    #[test]
    fn the_reflection_tables_are_gone_from_the_schema_and_dropped_on_open() {
        // #751-P4: the reflection concept is deleted from the product. A table
        // that still exists is a mechanism waiting to be re-wired, and a
        // reflection row surviving in a live database is a lie the product can
        // still read. Same shape as the provider-pool guard above.
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS reflection_handoffs("),
            "the reflection_handoffs table is still declared"
        );
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS reflection_handoff_items("),
            "the reflection_handoff_items table is still declared"
        );
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CREATE INDEX IF NOT EXISTS reflection_handoffs_person"),
            "the reflection_handoffs_person index is still declared"
        );
        let items = COMPANY_SCHEMA_SQL
            .find("DROP TABLE IF EXISTS reflection_handoff_items")
            .expect("historical databases must drop the retired items table");
        let head = COMPANY_SCHEMA_SQL
            .find("DROP TABLE IF EXISTS reflection_handoffs;")
            .expect("historical databases must drop the retired handoff table");
        assert!(
            items < head,
            "items must be dropped BEFORE the head they FOREIGN KEY onto, or the \
             migration fails under PRAGMA foreign_keys = ON"
        );
        // And the drops actually take effect: applying the schema to a database
        // that already carries both tables must leave neither behind.
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE reflection_handoffs(slug TEXT NOT NULL, transition_id TEXT NOT NULL, \
             person_id TEXT NOT NULL, recorded_at TEXT NOT NULL, \
             PRIMARY KEY (slug, transition_id));\n\
             CREATE INDEX reflection_handoffs_person ON reflection_handoffs(slug, person_id);\n\
             CREATE TABLE reflection_handoff_items(slug TEXT NOT NULL, transition_id TEXT NOT NULL, \
             seq INTEGER NOT NULL, kind TEXT NOT NULL, content TEXT NOT NULL, \
             PRIMARY KEY (slug, transition_id, seq), \
             FOREIGN KEY (slug, transition_id) REFERENCES reflection_handoffs(slug, transition_id));",
        )
        .expect("seed the retired tables as a historical database has them");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("the schema applies over them");
        let surviving: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN \
                 ('reflection_handoffs', 'reflection_handoff_items', 'reflection_handoffs_person')",
                [],
                |row| row.get(0),
            )
            .expect("count the retired objects");
        assert_eq!(surviving, 0, "no reflection table or index may survive opening the database");
    }

    #[test]
    fn company_identities_has_the_coherence_checks_without_a_second_schema() {
        let identities = COMPANY_SCHEMA_SQL
            .split_once("CREATE TABLE IF NOT EXISTS identities(")
            .and_then(|(_, after_start)| {
                after_start.split_once("CREATE INDEX IF NOT EXISTS identities")
            })
            .map(|(ddl, _)| ddl)
            .expect("company schema declares identities exactly once");
        assert!(COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS identities("));
        assert!(identities.contains(
            "kind         TEXT NOT NULL CHECK(kind IN ('person','operator','service','channel'))"
        ));
        assert!(identities.contains("CHECK((kind = 'person') = (company_slug IS NOT NULL))"));
        assert!(identities.contains("CHECK((kind = 'channel') = (pubkey IS NULL))"));
        assert!(identities.contains("fingerprint  TEXT NOT NULL UNIQUE"));
        assert!(COMPANY_SCHEMA_SQL.contains("identities_principal_idx"));
        // No JSON in the company-owned identities columns (N7's gate extends here too).
        for line in identities.lines() {
            let code = line.trim_start();
            if code.starts_with("--") {
                continue;
            }
            let lower = code.to_lowercase();
            assert!(
                !lower.contains("_json") && !lower.contains(" json"),
                "no JSON column allowed in the company schema: {line}"
            );
        }
        let retired_schema = format!("{}{}", "REGISTRY_SCHEMA", "_SQL");
        assert!(
            !include_str!("schema.rs").contains(&retired_schema),
            "the retired host registry schema must not be reintroduced"
        );
    }

    // ---- Normalized schema guards (org-data-normalization P0, N1) --------

    #[test]
    fn normalized_core_tables_are_present() {
        for table in [
            "CREATE TABLE IF NOT EXISTS departments(",
            "CREATE TABLE IF NOT EXISTS people(",
            "CREATE TABLE IF NOT EXISTS person_tools(",
            "CREATE TABLE IF NOT EXISTS person_prompts(",
            "CREATE TABLE IF NOT EXISTS org_settings(",
            "CREATE TABLE IF NOT EXISTS staffing_history(",
            "CREATE TABLE IF NOT EXISTS transitions(",
            "CREATE TABLE IF NOT EXISTS person_activity(",
            "CREATE TABLE IF NOT EXISTS launch_intent(",
            "CREATE TABLE IF NOT EXISTS activity_meta(",
            "CREATE TABLE IF NOT EXISTS reminders(",
            "CREATE TABLE IF NOT EXISTS supervision_meta(",
            "CREATE TABLE IF NOT EXISTS maintenance_requests(",
            "CREATE TABLE IF NOT EXISTS maintenance_company_actions(",
            "CREATE TABLE IF NOT EXISTS maintenance_ledger(",
            "CREATE TABLE IF NOT EXISTS org_events(",
        ] {
            assert!(COMPANY_SCHEMA_SQL.contains(table), "missing normalized table: {table}");
        }
    }

    #[test]
    fn b2_contract_metadata_is_columns_and_transient_is_dropped() {
        // delta #17: contract-unit metadata → additive columns; transient dropped.
        let block = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS departments(")
            .nth(1)
            .and_then(|s| s.split("\n);").next())
            .expect("departments table");
        for column in ["contract_engagement", "contract_launched_at", "contract_expires_at"] {
            assert!(block.contains(column), "departments missing {column}");
        }
        assert!(
            !block.contains("CHECK(transient IN (0,1))"),
            "the transient column must be dropped"
        );
        assert!(block.contains("(kind = 'contract') = (contract_engagement IS NOT NULL)"));
    }

    #[test]
    fn b2_people_fields_and_start_active_drop() {
        // delta #18: start_active and work_monitoring dropped. Two more of that
        // delta's additions are gone again — the elevated-thinking
        // justification column (#1139) and the static approval tier (C2).
        // Neither is declared below, and neither is named anywhere in this
        // crate, so there is no second definition of `people` to drift from.
        let block = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS people(")
            .nth(1)
            .and_then(|s| s.split("\n);").next())
            .expect("people table");
        // delta #22 (N9 B1): activation is a required column carrying live data.
        assert!(
            block.contains("activation             TEXT NOT NULL DEFAULT 'resident'")
                && block.contains("CHECK(activation IN ('resident','on-demand'))"),
            "people must carry the activation column (N9 zero-loss gap)"
        );
        assert!(
            !block.contains("CHECK(start_active IN (0,1))"),
            "the start_active column must be dropped"
        );
        assert!(
            !block.contains("CHECK(work_monitoring IN (0,1))"),
            "the work_monitoring column must be dropped with the pi-loop addon"
        );
    }

    /// THE RETIRED COLUMNS, PINNED WHERE THEY NOW LIVE: the DDL itself.
    ///
    /// Each of these was retired by a different decision and each used to
    /// be guarded by its own "gone from the DDL AND dropped on open" test. The
    /// on-open half is deleted with the migration family, and this is the
    /// surviving half, gathered into one place because it is one claim: a
    /// FRESH store declares none of them.
    ///
    /// CORRECTION, 2026-08-20: this used to justify the deletion with "a
    /// company database lives in its own directory now and no historical one is
    /// ever opened". That is false and was measurably false — every company
    /// that has been running for longer than one release IS a historical
    /// database, and the belief cost an operator every company on their box
    /// when `person_activity.operator_wake_at` was added (see
    /// [`add_missing_columns`]). The DROPs above are still right; the reason
    /// given for them was not.
    ///
    /// It is not a restatement of the deletion. `CREATE TABLE IF NOT EXISTS`
    /// is silent about a column somebody adds back, and there is no migration
    /// left to notice one either, so re-adding any of these names would now be
    /// invisible everywhere except here.
    ///
    /// Table-scoped rather than whole-file, deliberately: `requested_model_approval`
    /// belongs to `maintenance_requests` and the rest to `people`, and a
    /// substring search over the whole schema would pass for the wrong reason
    /// the moment either name appeared in an unrelated comment.
    ///
    /// `person_resources.rationale` (#1093) was the fourth entry and is gone
    /// from this list because its whole TABLE is gone (§4e) — an absent table
    /// declares no columns, and `the_person_resources_table_is_gone_with_per_person_resource_selection`
    /// is the stronger claim.
    #[test]
    fn the_retired_columns_are_absent_from_the_tables_that_carried_them() {
        let table = |name: &str| -> &str {
            COMPANY_SCHEMA_SQL
                .split(&format!("CREATE TABLE IF NOT EXISTS {name}("))
                .nth(1)
                .and_then(|body| body.split("\n);").next())
                .unwrap_or_else(|| panic!("{name} table"))
        };
        for (table_name, column) in [
            // #1139: the elevated-thinking justification, and the CHECK that
            // restated the gate it enforced.
            ("people", "thinking_reason"),
            // C2: the static approval tier, whose two carrier columns read as a
            // live spend control long after nothing enforced them.
            ("people", "model_approval"),
            ("maintenance_requests", "requested_model_approval"),
        ] {
            assert!(
                !table(table_name).contains(column),
                "{table_name} must not declare the retired column {column}"
            );
        }
        // And the assertion is not vacuous: the same reader finds the columns
        // these tables DO declare.
        assert!(table("people").contains("mandate"), "the people reader must reach real columns");
        assert!(
            table("maintenance_requests").contains("person_id"),
            "the maintenance_requests reader must reach real columns"
        );
    }

    #[test]
    fn b2_person_prompts_and_revisionless_org_settings() {
        // deltas #19/#20/#21.
        assert!(COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS person_prompts("));
        assert!(COMPANY_SCHEMA_SQL.contains("UNIQUE (slug, person_id, template)"));
        assert!(COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS org_settings("));
        for column in [
            "supervision_interval_ms",
            "acknowledgement_timeout_ms",
            "acknowledgement_retry_limit",
            "replacement_limit",
        ] {
            assert!(COMPANY_SCHEMA_SQL.contains(column), "org_settings missing {column}");
        }
        let settings = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS org_settings(")
            .nth(1)
            .and_then(|s| s.split("\n);").next())
            .expect("org settings table");
        assert!(
            !settings.contains("revision"),
            "org_settings must not carry a manifest-wide counter"
        );
        // The `loans` half of delta #21 went with the loan concept itself
        // (operator ruling, 2026-08-13). There is no table left to assert.
        assert!(
            !COMPANY_SCHEMA_SQL.contains("loans"),
            "the loans table is deleted, not merely unused"
        );
    }

    #[test]
    fn n3_reminders_and_supervision_meta() {
        // N3-reminders (Class-A) + supervision_meta (activity_meta precedent).
        let r: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS reminders(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("reminders")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(r.contains("status TEXT NOT NULL CHECK(status IN ('active','stopped'))"));
        assert!(r.contains("recurring INTEGER NOT NULL CHECK(recurring IN (0, 1))"));
        let sm: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS supervision_meta(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("supervision_meta")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !sm.contains("updated_at TEXT"),
            "supervision_meta must have no updated_at column (hot-row contention)"
        );
    }

    #[test]
    fn transitions_does_not_fk_people_so_manifest_removal_never_corrupts_the_company_db() {
        // delta #49 drift guard: transitions
        // is ACTIVITY-store state, but the MANIFEST publish (a different store) is
        // what removes a person. A `transitions -> people` FK made a recursive
        // removal with an open park/offboard transition fail the DEFERRED FK at
        // COMMIT, mislabeled "corrupt store: company-db". The row must have NO
        // people FK (the activity store prunes it via its own accessor; an orphan
        // is tolerated on read like departed staffing_history).
        let t: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS transitions(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("transitions")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !t.contains("REFERENCES people"),
            "transitions must NOT FK people (manifest removal is a different store; delta #49)"
        );
    }

    #[test]
    fn n9_b4_singleton_sweep_tables() {
        // delta #24 (N9 B4): new/reshaped singleton-sweep tables.
        for table in [
            "CREATE TABLE IF NOT EXISTS operator_escalation_intents(",
            "CREATE TABLE IF NOT EXISTS operator_escalation_push(",
            // supervisor_armed_intent was in this delta and is RETRACTED — see
            // `the_retired_supervisor_process_tables_are_gone_and_dropped_on_open`.
        ] {
            assert!(COMPANY_SCHEMA_SQL.contains(table), "B4 missing {table}");
        }
        // runtime_owner reshape: nullable socket/claimed_at + status lifecycle.
        let ro: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS runtime_owner(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("runtime_owner")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(ro.contains(
            "status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','released'))"
        ));
        assert!(ro.contains("validated_at TEXT") && ro.contains("released_at TEXT"));
    }

    #[test]
    fn durable_materialization_state_is_absent() {
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS materialization("));
        assert!(!COMPANY_SCHEMA_SQL.contains("materialization_checkpoints"));
    }

    #[test]
    fn the_persisted_head_in_parent_placement_columns_are_gone() {
        // #751-P9: head-in-parent is a DISPLAY rule and lives only in the
        // operator client now. Both columns that persisted chiefd's answer are
        // deleted from the DDL, so a fresh database never grows them and no
        // statement can name them.
        // The check strips `--` comments FIRST. Grepping the whole DDL string
        // for a bare column name is not a test of the schema, it is a test of
        // the prose: the tombstones above deliberately spell both names out to
        // say why they are gone, and a naive `contains` fails on its own
        // documentation. Only a DECLARATION counts.
        let declarations: String = COMPANY_SCHEMA_SQL
            .lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !declarations.contains("last_pane_department_id"),
            "person_activity must not persist the head-in-parent display answer"
        );
        assert!(
            !declarations.contains("from_pane_department_id"),
            "transitions must not persist the head-in-parent display answer"
        );
        // The tombstones themselves must survive, so a future reader learns why
        // the columns are absent instead of re-adding them.
        assert!(
            COMPANY_SCHEMA_SQL.contains("last_pane_department_id")
                && COMPANY_SCHEMA_SQL.contains("from_pane_department_id"),
            "the tombstone comments explaining both deletions must stay"
        );
        // And the removal is expressed as a column drop, NOT as a
        // drop-and-recreate of either table under its own name — that pair is a
        // standing wipe, because this SQL runs on every open.
        for table in ["transitions", "person_activity"] {
            assert!(
                !COMPANY_SCHEMA_SQL.contains(&format!("DROP TABLE IF EXISTS {table};")),
                "{table} must never be dropped by a schema that also recreates it"
            );
        }
    }

    #[test]
    fn n4_person_activity_and_activity_meta() {
        // Fable #6: person_activity gains 3 scalar VALUEs; activity_meta is the
        // singleton with a tail-deleted revision shim, no updated_at (hot-row
        // contention), and NO next_transition_sequence column (that's a D2
        // counter row 'transitions:<slug>', never a stored MAX+1).
        let pa: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS person_activity(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("person_activity")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(pa.contains("last_employment_state TEXT CHECK(last_employment_state IN ('active','benched','departed'))"));
        assert!(pa.contains("last_operational INTEGER CHECK(last_operational IN (0,1))"));
        assert!(pa.contains("active_transition_id TEXT,"));
        let am: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS activity_meta(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("activity_meta")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(am.contains("automatic_park_cursor INTEGER NOT NULL"));
        assert!(
            !am.contains("next_transition_sequence"),
            "next_transition_sequence is a D2 counter, not a column"
        );
        assert!(!am.contains("updated_at"), "no updated_at on activity_meta (hot-row contention)");
    }

    #[test]
    fn b2_staffing_history_carries_the_ten_manifest_terms() {
        // delta #22: exact manifest vocabulary, no lossy synonym mapping.
        // #1036 adds `rehired` as the tenth term — a rehire is a distinct
        // durable event from a bench `recalled`, and the ledger says which.
        for term in [
            "'hired'",
            "'benched'",
            "'recalled'",
            "'rehired'",
            "'transferred'",
            "'offboarded'",
            "'appointed-head'",
            "'stepped-down'",
        ] {
            assert!(COMPANY_SCHEMA_SQL.contains(term), "staffing_history.action missing {term}");
        }
        assert!(
            !COMPANY_SCHEMA_SQL.contains("'hire','park','loan'"),
            "the old lossy vocabulary must be gone"
        );
        // The loan verbs went with the concept (operator ruling, 2026-08-13).
        for gone in ["'loaned'", "'returned'"] {
            assert!(
                !COMPANY_SCHEMA_SQL.contains(gone),
                "staffing_history.action must no longer offer {gone}"
            );
        }
    }

    #[test]
    fn maintenance_flags_are_zero_or_one_check_constrained() {
        // N5: automatic + force are boolean VALUES, not free integers. A stray
        // 2 must be unrepresentable in both the request row and the company
        // action row (force there is NOT NULL — a company action always carries
        // its interrupt intent, unchanged from the blob validator).
        assert!(COMPANY_SCHEMA_SQL
            .contains("automatic    INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1))"));
        assert!(
            COMPANY_SCHEMA_SQL.contains("force                     INTEGER CHECK(force IN (0,1))")
        );
        assert!(
            COMPANY_SCHEMA_SQL.contains("force             INTEGER NOT NULL CHECK(force IN (0,1))")
        );
    }

    #[test]
    fn maintenance_ordering_is_per_slug_ordinal_not_a_json_array() {
        // N5: requestOrder / companyActionOrder / targetOrder are gone as arrays
        // (D1: no serialized STRUCTURE) and re-homed as `ordinal` columns pinned
        // unique per parent, so the append-chronological order is a real query.
        assert!(COMPANY_SCHEMA_SQL
            .contains("CREATE UNIQUE INDEX IF NOT EXISTS maintenance_requests_ordinal"));
        // `maintenance_company_actions_ordinal` still exists: the parent table
        // is KEPT, empty, because `maintenance_requests` foreign-keys onto it
        // and SQLite resolves that parent when the child's INSERT prepares.
        assert!(COMPANY_SCHEMA_SQL
            .contains("CREATE UNIQUE INDEX IF NOT EXISTS maintenance_company_actions_ordinal"));
        // The TARGETS table is dropped with the feature, so its ordinal index
        // must be gone rather than merely unused.
        assert!(!COMPANY_SCHEMA_SQL.contains(
            "CREATE UNIQUE INDEX IF NOT EXISTS maintenance_company_action_targets_ordinal"
        ));
        assert!(
            COMPANY_SCHEMA_SQL.contains("DROP TABLE IF EXISTS maintenance_company_action_targets;")
        );
        // requestIds[] is DERIVED, never a table (Fable ruling): assert no such
        // child table crept in.
        assert!(!COMPANY_SCHEMA_SQL.contains("maintenance_company_action_target_requests"));
        assert!(!COMPANY_SCHEMA_SQL.contains("maintenance_request_ids"));
    }

    #[test]
    fn maintenance_company_action_reference_is_a_real_fk() {
        // N5: company_action_id points at the child action table by FK (the blob
        // invariant "a request naming a company action must have one" becomes
        // referential integrity). The reverse pointer (target.current_request_id)
        // is deliberately NOT an FK to avoid a requests<->targets cycle.
        assert!(COMPANY_SCHEMA_SQL.contains(
            "FOREIGN KEY (slug, company_action_id) REFERENCES maintenance_company_actions(slug, id)"
        ));
    }

    #[test]
    fn maintenance_ledger_is_revisionless_metadata() {
        let block = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS maintenance_ledger(")
            .nth(1)
            .and_then(|sql| sql.split(");").next())
            .expect("maintenance ledger table");
        assert!(block.contains("created_at TEXT NOT NULL"));
        assert!(block.contains("updated_at TEXT NOT NULL"));
        assert!(!block.contains("revision"));
    }

    #[test]
    fn exactly_one_active_transition_is_a_partial_unique_index_with_a_positive_list() {
        // Twin of one_open_transition: a negated-terminal predicate would let a
        // future terminal status wedge a person by counting as active. Nobody
        // gets to simplify this to `status != 'applied'`.
        assert!(
            COMPANY_SCHEMA_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS transitions_one_active")
        );
        assert!(
            COMPANY_SCHEMA_SQL.contains("WHERE status IN ('awaiting_handoff','overdue','ready')")
        );
        assert!(!COMPANY_SCHEMA_SQL.contains("status != 'applied'"));
    }

    #[test]
    fn normalized_schema_forbids_json_columns() {
        // The mandate's defining invariant: no column stores JSON. The only
        // `_json` column left is host_actions.plan_json: a crash-recovery
        // journal payload, not product authority. It is mechanically bounded
        // to an object and paired with a closed payload-schema discriminator.
        // delta #12: scan the WHOLE schema constant, not just the normalized
        // block, so a JSON column cannot regress into effects or
        // anywhere else. The ONE documented legacy carve-out is
        // host_actions.plan_json (the DB<->fs 2PC intent); assert it is the
        // *only* one.
        let mut json_columns = Vec::new();
        for line in COMPANY_SCHEMA_SQL.lines() {
            let code = line.trim_start();
            if code.starts_with("--") {
                continue;
            }
            let lower = code.to_lowercase();
            if lower.contains("_json") || lower.contains(" json") {
                json_columns.push(code.to_string());
            }
        }
        assert_eq!(
            json_columns,
            vec![
                "plan_json      TEXT NOT NULL CHECK(json_valid(plan_json) AND json_type(plan_json) = 'object'),"
                    .to_string()
            ],
            "the only permitted JSON column is guarded host_actions.plan_json; found: {json_columns:?}"
        );
        assert!(COMPANY_SCHEMA_SQL.contains(
            "payload_schema TEXT NOT NULL CHECK(payload_schema IN ('host-txn-v1','converge-intent-v1'))"
        ));
        assert!(COMPANY_SCHEMA_SQL
            .contains("kind = 'converge' AND payload_schema = 'converge-intent-v1'"));
    }

    #[test]
    fn transition_intent_id_is_nullable() {
        // delta #1: NULL = unowned idle-park; NOT NULL would erase supersede
        // ownership (#337). Guard the column is not declared NOT NULL.
        let block = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS transitions(")
            .nth(1)
            .and_then(|s| s.split(");").next())
            .expect("transitions table");
        let flat: String = block.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("intent_id TEXT,"), "intent_id must be nullable");
        assert!(!flat.contains("intent_id TEXT NOT NULL"));
    }

    #[test]
    fn department_uniqueness_invariants_are_enforced() {
        // deltas #2/#3: one head per person, exactly one root per company.
        assert!(COMPANY_SCHEMA_SQL.contains("departments_one_head"));
        assert!(
            COMPANY_SCHEMA_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS departments_one_root")
        );
        assert!(COMPANY_SCHEMA_SQL.contains("ON departments(slug) WHERE parent_id IS NULL"));
    }

    #[test]
    fn the_loans_table_is_gone_with_the_concept() {
        // Was delta #5, asserting a loan could not be a self-move. Operator
        // ruling, 2026-08-13: there is no such thing as loaning, so there is
        // no table and no constraint on it. Deleted with the behaviour rather
        // than left asserting a shape nothing writes.
        assert!(!COMPANY_SCHEMA_SQL.contains("loans("), "the loans table must be gone");
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CHECK (from_department_id <> to_department_id)"),
            "its self-move CHECK goes with it"
        );
    }

    /// The per-person resource grants are absent from the fresh schema.
    ///
    /// chief-home-is-cwd §3/§4e: an agent's skills are whatever is in
    /// `<dir>/.pi/skills` when Pi looks, reached through one symlink. Pi
    /// discovers, validates and namespaces them; chief selects none, so there is
    /// no per-person skill, extension or package to record. Rows that survived
    /// would describe a selection nothing makes and nothing reads. This
    /// unreleased product does not migrate old databases, so the schema has no
    /// create or drop statement for the retired table.
    #[test]
    fn the_person_resources_table_is_gone_with_per_person_resource_selection() {
        assert!(
            !COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS person_resources("),
            "the person_resources table is still declared"
        );
        assert!(
            !COMPANY_SCHEMA_SQL.contains("DROP TABLE IF EXISTS person_resources;"),
            "the fresh-only schema must not migrate the retired table"
        );
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("the schema applies over it");
        assert!(
            present_tables(&conn, &["person_resources"]).is_empty(),
            "a fresh database must not create person_resources"
        );
        assert_eq!(
            present_tables(&conn, &["person_tools"]),
            vec!["person_tools".to_string()],
            "the surviving tool grant must not be dropped with it"
        );
    }

    #[test]
    fn the_never_implemented_unit_removal_journal_is_retired() {
        // Was delta #6, asserting the phase CHECK. The journal never acquired
        // a writer, so the CHECK constrained a column nothing ever wrote; the
        // tables are dropped instead. `store::tests::
        // opening_drops_the_never_implemented_unit_removal_journal_tables`
        // proves the migration behaviourally.
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS unit_removals("));
        assert!(COMPANY_SCHEMA_SQL.contains("DROP TABLE IF EXISTS unit_removal_members;"));
        assert!(COMPANY_SCHEMA_SQL.contains("DROP TABLE IF EXISTS unit_removals;"));
    }

    #[test]
    fn sibling_ordinal_is_unique_within_a_parent() {
        // departmentOrder/peopleOrder bijectivity: two siblings cannot share an
        // ordinal. Whole-tree uniqueOrder bijectivity is re-checked in validate().
        assert!(COMPANY_SCHEMA_SQL.contains("departments_sibling_ordinal"));
    }

    /// ONE foreign key, over the one placement column. This asserted two, over
    /// the `home` + `assigned` pair the loan concept needed, so the count is
    /// worth pinning rather than assuming: a second placement column would
    /// bring a second foreign key back with it.
    #[test]
    fn foreign_keys_wire_people_to_their_departments() {
        const FK: &str = "FOREIGN KEY (slug, department_id) REFERENCES departments(slug, id)";
        assert!(COMPANY_SCHEMA_SQL.contains(FK));
        assert_eq!(
            COMPANY_SCHEMA_SQL.matches(FK).count(),
            1,
            "a person has ONE placement column, so it carries ONE foreign key"
        );
        assert!(
            !COMPANY_SCHEMA_SQL.contains("home_department_id TEXT")
                && !COMPANY_SCHEMA_SQL.contains("assigned_department_id TEXT"),
            "neither half of the retired pair may be declared as a live column"
        );
    }

    #[test]
    fn org_events_seq_is_a_per_slug_counter_not_autoincrement() {
        // D2 ruling: seq is a per-slug counter bumped in-txn; PK (slug, seq);
        // NO global AUTOINCREMENT (that would serialize all slugs and reopen the
        // #286 race). The org_events block must not carry AUTOINCREMENT.
        let feed = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS org_events(")
            .nth(1)
            .expect("org_events table is present");
        let feed_ddl = feed.split(");").next().expect("org_events body");
        let flat: String = feed_ddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("seq INTEGER NOT NULL"));
        assert!(!feed_ddl.contains("AUTOINCREMENT"), "org_events.seq must not autoincrement");
        assert!(COMPANY_SCHEMA_SQL.contains("PRIMARY KEY (slug, seq)"));
    }

    #[test]
    fn mailbox_state_is_check_constrained_to_the_six_buckets() {
        // 6-bucket state CHECK kept as a SUPERSET (incl. #493 'delivered') through
        // the delta-#28 columnarization (5-vs-6 native-enum question flagged to
        // Fable/N-mailbox — a superset CHECK is strictly safe).
        let flat: String = COMPANY_SCHEMA_SQL.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains(
            "state TEXT NOT NULL CHECK(state IN ('pending','delivered','accepted','superseded','rejected','resolved'))"
        ));
    }

    #[test]
    fn structural_body_columns_are_absent_from_fresh_schema() {
        // Fable's D1 rule is structural, not documentary: effects must expose
        // only their typed final form. Checking the actual table block
        // prevents a compatibility JSON column from surviving behind a
        // reassuring disposition comment.
        // `effects` is the only table left with this rule to keep -- the goal
        // and assignment tables it also covered are deleted.
        let table = "effects";
        let block = COMPANY_SCHEMA_SQL
            .split(&format!("CREATE TABLE IF NOT EXISTS {table}("))
            .nth(1)
            .unwrap_or_else(|| panic!("{table} table"))
            .split(");")
            .next()
            .unwrap_or_else(|| panic!("{table} body"));
        let columns = block
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .collect::<Vec<_>>();
        assert!(
            columns.iter().all(|line| !line.starts_with("body ")),
            "{table} must not retain a structural body column: {columns:?}"
        );
        // The legacy `reflections` table never existed here, and #751-P4
        // removed the two tables that did hold the fact — see
        // `the_reflection_tables_are_gone_from_the_schema_and_dropped_on_open`.
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS reflections("));
        // delta #28 (reconciled to canonical a2804e0a, Fable #7): mailbox is
        // COLUMNARIZED — envelope_id PK pinned to id@person via CHECK, `message`
        // (not body) is the one opaque payload, assignment/health flattened with
        // present-together CHECKs, organization DERIVED (not stored).
        let mb: String = COMPANY_SCHEMA_SQL
            .split("CREATE TABLE IF NOT EXISTS mailbox(")
            .nth(1)
            .expect("mailbox table")
            .split("CREATE INDEX")
            .next()
            .expect("mailbox body")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            mb.contains("CHECK (envelope_id = id || '@' || person)"),
            "mailbox envelope_id pinned by CHECK"
        );
        assert!(
            mb.contains("message TEXT NOT NULL"),
            "mailbox payload column is `message`, not opaque body"
        );
        assert!(
            mb.contains("urgency TEXT NOT NULL CHECK(urgency IN ('normal','interrupt'))"),
            "mailbox typed urgency"
        );
        assert!(
            !mb.contains("organization TEXT NOT NULL"),
            "mailbox.organization must be DERIVED, not stored"
        );
    }

    #[test]
    fn documents_and_vestigial_provider_admission_are_absent_after_final_cutover() {
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS documents"));
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS provider_admission"));
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS provider_slots"));
        assert!(!COMPANY_SCHEMA_SQL.contains("CREATE TABLE IF NOT EXISTS provider_reservations"));
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn company_schema_applies_cleanly_to_a_real_sqlite_connection() {
        // The string guards above cannot catch a SQL syntax/execution error (a
        // missing ';', a bad CHECK, an out-of-order FK) — only EXECUTING the DDL
        // does. This is the check that catches a broken constant the moment it
        // lands (e.g. the mailbox-revert missing-semicolon that failed the whole
        // COMPANY_SCHEMA_SQL apply and surfaced only as live_read_memo failures).
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory");
        conn.execute_batch(COMPANY_SCHEMA_SQL)
            .expect("COMPANY_SCHEMA_SQL must apply cleanly to a real connection");
    }

    /// The four retired supervisor tables, in the order a historical database
    /// created them.
    const RETIRED_SUPERVISOR_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS supervisor_armed_intent(
    slug          TEXT PRIMARY KEY,
    socket_name   TEXT NOT NULL,
    session_name  TEXT NOT NULL,
    token         TEXT NOT NULL,
    pid           INTEGER NOT NULL,
    process_start TEXT NOT NULL,
    armed_at      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS supervisor_state(
    slug       TEXT PRIMARY KEY,
    watermark  INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS supervisor_process_state(
    slug                   TEXT PRIMARY KEY,
    socket                 TEXT NOT NULL,
    token                  TEXT NOT NULL,
    pid                    INTEGER NOT NULL,
    process_start          TEXT NOT NULL,
    status                 TEXT NOT NULL CHECK(status IN ('running','stopped')),
    interval_ms            INTEGER NOT NULL,
    started_at             TEXT NOT NULL,
    last_heartbeat_at      TEXT NOT NULL,
    last_health_check_at   TEXT,
    last_reconcile_at      TEXT,
    last_error             TEXT,
    runtime_recovery_count INTEGER,
    stopped_at             TEXT
);
CREATE TABLE IF NOT EXISTS supervisor_runtime_events(
    slug     TEXT NOT NULL,
    role     TEXT NOT NULL CHECK(role IN ('crash','recovery','log')),
    ordinal  INTEGER NOT NULL,
    at       TEXT NOT NULL,
    kind     TEXT NOT NULL,
    message  TEXT NOT NULL,
    PRIMARY KEY (slug, role, ordinal)
);
CREATE TABLE IF NOT EXISTS supervisor_runtime_event_log(
    slug     TEXT NOT NULL,
    role     TEXT NOT NULL CHECK(role IN ('crash','recovery','log')),
    ordinal  INTEGER NOT NULL,
    at       TEXT NOT NULL,
    kind     TEXT NOT NULL,
    message  TEXT NOT NULL,
    PRIMARY KEY (slug, role, ordinal)
);
";

    /// Which of `names` exist as tables on `conn`.
    #[allow(clippy::disallowed_methods)]
    fn present_tables(conn: &rusqlite::Connection, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .filter(|name| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [*name],
                    |row| row.get::<_, i64>(0),
                )
                .expect("sqlite_master is queryable")
                    > 0
            })
            .map(|name| (*name).to_string())
            .collect()
    }

    #[test]
    fn the_retired_supervisor_tables_are_not_declared() {
        // A dropped mechanism that still has a table is a mechanism waiting to
        // be re-wired. `supervisor_process_state` proved that: its writer (the
        // detached org-supervisor's state module) was deleted by 5681617a4 after
        // #825 retired the process, and the readers that outlived it silently
        // disabled two `audit_ownership` refusals, one `assert_company_stopped`
        // arm, and pinned `RuntimeLiveness` to `Stopped`.
        for table in [
            "supervisor_armed_intent",
            "supervisor_state",
            "supervisor_process_state",
            "supervisor_runtime_events",
            "supervisor_runtime_event_log",
        ] {
            assert!(
                !COMPANY_SCHEMA_SQL.contains(&format!("CREATE TABLE IF NOT EXISTS {table}(")),
                "the retired {table} table is still declared"
            );
            assert!(
                COMPANY_SCHEMA_SQL.contains(&format!("DROP TABLE IF EXISTS {table};")),
                "historical databases must drop the retired {table} table"
            );
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn the_retired_supervisor_process_tables_are_gone_and_dropped_on_open() {
        let all = [
            "supervisor_armed_intent",
            "supervisor_state",
            "supervisor_process_state",
            "supervisor_runtime_events",
            "supervisor_runtime_event_log",
        ];
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory");

        // A historical database that crossed the migration boundary once.
        conn.execute_batch(RETIRED_SUPERVISOR_SCHEMA).expect("seed the retired schema");
        assert_eq!(
            present_tables(&conn, &all).len(),
            all.len(),
            "the fixture must actually create all four, or this test proves nothing"
        );
        conn.execute(
            "INSERT INTO supervisor_process_state(slug, socket, token, pid, process_start, \
             status, interval_ms, started_at, last_heartbeat_at) \
             VALUES('acme','cobalt','tok',42,'100','running',30000,'t','t')",
            [],
        )
        .expect("seed a row the drop must take with it");

        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("first open applies cleanly");
        assert!(
            present_tables(&conn, &all).is_empty(),
            "opening a historical database must remove every retired supervisor table"
        );

        // Idempotent on every subsequent open — and note that no CREATE for any
        // of these names follows the DROP. Dropping and recreating under the
        // same name would erase live state on every daemon boot.
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("second open applies cleanly");
        assert!(present_tables(&conn, &all).is_empty(), "the drop must be idempotent");

        // A fresh database never grows them in the first place.
        let fresh = rusqlite::Connection::open_in_memory().expect("open in-memory");
        fresh.execute_batch(COMPANY_SCHEMA_SQL).expect("fresh open applies cleanly");
        assert!(
            present_tables(&fresh, &all).is_empty(),
            "a fresh database must not create a retired table"
        );

        // The live watermark store is NOT collateral: `supervisor_watermarks`
        // is what superseded `supervisor_state`, and it survives both opens.
        assert_eq!(
            present_tables(&fresh, &["supervisor_watermarks"]),
            vec!["supervisor_watermarks".to_string()],
            "the live per-duty watermark table must survive the retirement of its namesake"
        );
    }

    #[test]
    fn company_pragmas_are_durable_not_fast() {
        assert!(COMPANY_PRAGMAS.contains(&"PRAGMA journal_mode=WAL"));
        assert!(COMPANY_PRAGMAS.contains(&"PRAGMA synchronous=FULL"));
    }
}
