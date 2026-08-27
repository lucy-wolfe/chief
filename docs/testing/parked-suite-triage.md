# Parked-suite triage map

Status: **DECIDED, DRIFT-GUARDED**. This is the human-narrative counterpart
to `docs/testing/parked-suite-triage.json` (the authority — one row per
committed legacy test file). Every one of the 408
parked `bun:test` files (277 unit + 130 e2e + 1 manual), plus 47 support
files the corpus depends on, has exactly one row in the JSON: a
`disposition`, a `target`, a `reason`, an owning `story`, and a `status`.
`scripts/test/parked-suite-triage.test.mjs` (`node --test`, wired as
`bun run test:triage-map`) fails the build if a committed file has no row, a
row names an uncommitted path, a disposition sits outside the closed enum, a
path is duplicated, a `retire:rust` target doesn't exist under
`apps/chiefd/`, a row's lane disagrees with the corpus's own e2e/manual/unit
routing, or this narrative's cluster counts drift from the JSON's.

Corpus SHA referenced throughout: `de72d660` (pre-move; `services/chiefd/…`
paths in inherited context are pre-E1-S1 citations — E1-S1 already moved
that tree to `apps/chiefd/…`, #758). At the time this map was generated,
E0-S2 (#753) had already renamed three meta files to `.parked` and E1-S1 had
already landed, so the live corpus on disk is 274 `*.test.ts` unit files +
274's-missing-3 accounted for below, not 277 — see "Reading the corpus"
below for why the map still counts 277.

## How to read the corpus once E4-S1 breaks its imports

Every parked test imports `../src/**`. The day `E4-S1-move-legacy-src`
relocates `src/**` into `apps/cli/src/legacy/`, the parked corpus stops
resolving as runnable code — its *content* stays readable at any git SHA,
but nobody will be able to `bun test` it to check "what did this actually
assert?" anymore. Read a parked file at the SHA where it last resolved:

```bash
git show de72d660:tests/org-row-stores.test.ts
```

**Standing rule:** new coverage is written with its change, never
back-ported from this corpus. A file marked `port:*` below is a *starting
point to read*, not a source to copy-paste; the ported test asserts today's
contract, using this corpus only as a reminder of what used to matter.

## Why the map exists before the ports

- **The corpus is already not typechecked.** E0-S1 narrowed
  `tsconfig.legacy.json` to `src/` + `extensions/` in wave 0 (ruling D15), so
  `bun run typecheck` says nothing about `tests/**` from now on. While a file
  is parked, neither the compiler nor any runner is watching it — this
  written disposition is what stands between "parked" and "silently
  deleted". Coverage returns to the typecheck package-by-package as rows
  flip to `ported` (a ported suite sits under its own package's
  `tsconfig.vitest.json`).
- **The 96-file `org-engine` cluster is the coverage that guards ~30k LOC of
  TS business logic scheduled for deletion as chiefd absorbs it.** Every
  `retire:rust` row names the Rust target that replaces the assertion, so
  deleting the TS logic during the port is a bounded, reviewable step, not a
  coverage cliff.

## The mechanical classifier (deleted, #1035)

`scripts/test/classify-parked-suites.mjs` (deleted) reproduced the corpus's
import-derived clustering (first-match precedence: `meta` →
`park-realdaemon` → `chiefing` → `cli` → `piing` → `org-engine` →
`extensions` → `other`) by reading every committed `tests/**/*.test.ts` and
following its import specifiers. It is **deleted**: #1035 removed 135 of
those files, so the script's first action — `readFileSync` on the first path
`git ls-files tests` reports at the snapshot — now throws `ENOENT`, and there
is no corpus left for it to cluster. The map does not need regenerating: rule
1 scopes completeness to the snapshot at `capturedAt`, which is frozen, and
the `cluster` field of every row is already recorded here and guarded by rule
6. What follows is the classifier's own last output, preserved as the record
of how the clustering was derived.

At the time this map was generated it produced (277 unit files total,
including the 3 already parked by rename):

| Cluster | Files (classifier) | Files (this map, unit lane) |
|---|---:|---:|
| meta | 15 | **15** |
| park-realdaemon | 33 | **36** |
| chiefing | 31 | **31** |
| cli | 23 | **24** |
| piing | 20 | **20** |
| org-engine | 97 | **96** |
| extensions | 15 | **15** |
| other | 43 | **40** |
| **total** | 277 | **277** |

**Known drift between the classifier and this map, and why it's expected
(not a bug in either):**

- **`cli` (23 → 24), `piing` (20 → 20), and `org-engine` (97 → 96):** the
  classifier buckets `tests/org-intercom.test.ts` under `org-engine` (its
  imports lean on general `src/organization/*` modules), but the epic's own
  review — and this map — treats it as part of the piing/extension domain by
  *subject* (the 42 `org_*` tool surface an extension calls), so its row
  carries `cluster: "piing"` with disposition `split`. Conversely,
  `tests/slugify-truncation-boundary.test.ts` is owned by the canonical CLI
  legacy identifier helper, so its row moves from the classifier's `piing`
  bucket to `cluster: "cli"` with disposition `port:cli`. The two overrides
  cancel in the `piing` total while moving one file from `org-engine` to
  `cli`. The classifier's job is to reproduce a reproducible starting point,
  not the final word; every override like these lives as a row-level `reason`
  in the JSON, exactly as issue #834 anticipated ("the classifier is a
  starting point because imports are not intent").
- **`park-realdaemon` (33 → 36) and `other` (43 → 40):** three files that
  physically live inside `tests/e2e/harness/` and self-test a sibling module
  there via a bare relative import (`./chiefd-binary-path`, `./person`) —
  `chiefd-binary-path.test.ts`, `chiefd-prebuilt-binary.test.ts`,
  `person-refusal-guard.test.ts` — don't match the classifier's "imports
  something under `tests/e2e/harness/`" rule (a relative sibling import
  never mentions that path segment), so the classifier drops them in
  `other`. This map moves the three into `park-realdaemon` (`cluster` in the
  JSON), because they test a PROTECTED harness module in place and park with
  it. The e2e cluster therefore holds 3 plain `.test.ts` self-tests that ran
  in the unit lane.
- The epic's separately-published reference table (issue #839, measured
  independently) lists slightly different `cli`/`piing`/`org-engine`/`other`
  counts (25/24/94/40) than this run of the classifier does (23/20/97/43
  before the two overrides above). Both tables describe the same 277-file
  corpus; the difference is precedence-rule nuance in exactly which files
  land in `cli` vs `org-engine` vs `other` under repeated reclassification,
  not a missing or double-counted file — every file is still accounted for
  exactly once, which is what the guard actually enforces (dispositions, not
  bucket labels, are the load-bearing decision).

## Disposition vocabulary — the two values added by #1020

The original nine dispositions share one property nobody noticed until a large
set of rows needed a value: **every one of them asserts that work is owed.**
There was no way to record a file that is fine, and no way to record a file
that is broken for a reason nobody decided. Both gaps were found by trying to
set real rows and discovering that every available value would have been a
false statement.

| disposition | means | `target` |
|---|---|---|
| `keep:active` | **The file is healthy and should simply run. Nothing is owed.** Added because a healthy file previously had to claim a debt it did not have — the closest available values (`retire:meta`, `park:e2e`) both assert an obligation. Found on files that are tests *of* a harness rather than tests that *use* one, swept into `park:e2e` by their directory rather than by any judgement about them. | the lane that runs it |
| `migrate:paths` | **A real test whose relative imports name a layout that no longer exists** — the pre-monorepo repo-root `src/` tree, now under `apps/cli/src/legacy/`. The test is not obsolete and no decision was made about it; its module paths simply did not follow a repository move. | the destination the imports should point at |

### Why `migrate:paths` is not `retire:obsolete`

**`retire:obsolete` must keep meaning a decision someone made.** A test whose
imports moved is **unmigrated**, not obsolete, and the two call for opposite
responses: one is repaired, the other is deleted.

Collapsing them is not a labelling nicety. **A row marked `retire:obsolete`
reads as settled, so nobody opens it again** — filing an unmigrated test that
way launders a maintenance gap into a deliberate retirement, permanently, and
the evidence that it was merely unmigrated is exactly what the retirement
erases. The file survives in git; the fact does not.

**This is why the vocabulary changes before any row is re-triaged**, and not
after. Re-triaging first forces every unmigrated file into a value that
destroys the reason to revisit it, and that loss is not recoverable from the
corpus afterwards.

## Corpus totals this map covers

| Lane | Files | Disposition shape |
|---|---:|---|
| unit | **277** | see cluster table above; `port:*` (chiefing/cli/piing), `retire:*` (rust/meta/obsolete), `park:e2e`, `split` |
| e2e | **130** | all `park:e2e`, with the E2E corpus |
| manual | **1** | `park:e2e`, with the E2E corpus |
| support | **47** | helpers (12), fixtures (6), migration drivers (2), the bunfig preload (1), harness modules (18), fresh-org support (4), manual capture-proof scripts (4) |
| **total rows** | **455** | |

## Cluster narratives (unit lane)

### `chiefing` — 31 files → `port:chiefing` (27) / `port:cli` (2) / `port:piing` (1) / `retire:meta` (1) / `split` (1)

The chiefd-client wire-level surface: docstore, row stores, task/reminder/
mailbox/ack/goal-intent/memory-record/telegram-inbound stores, SSE watcher,
person-contracts rows, agent identity/JWT/enroll, and the shim re-export.
27 of the 31 port into `packages/chiefing/test/` under E9-S3, real per-file
targets named in the JSON (e.g. `tests/org-row-stores.test.ts` →
`packages/chiefing/test/resources/RowStores.test.ts`). Four are confirmed
overrides, read individually rather than assumed from the classifier bucket:

- `tests/triber-launcher-flow.test.ts` — only imports `org-durable-store`
  (hence the `chiefing` bucket) but its `describe()` blocks
  (`parseCreateAndBootArgs`, `runCreateAndBoot`,
  `runTriberLauncherBootstrap`) are launcher create/boot flow coverage →
  `port:cli`.
- `tests/structural-root-gate-270.test.ts` — `#270
  resolveInstallerStructuralRoot` (transient manifest-read failure vs
  genuine non-root) is a gate that lives in the intercom extension →
  `port:piing`, **not** `port:cli` as this map originally had it. Corrected
  against #835's Contract table.
- `tests/org-journal-marker-retention.test.ts` — `#492`'s throttled
  marker-sweep policy is caller-side; chiefing owns only the leaf
  `pruneEventOnceMarkers` primitive → `port:cli`. Corrected against #835's
  Contract table (this map originally had it as `port:chiefing`).
- `tests/no-production-generic-document-routes.test.ts` — `#582`'s
  repo-wide source guard is a repo invariant, not a package test → moved to
  `retire:meta`, target `scripts/test/no-generic-document-routes.test.mjs`.
  Corrected against #835's Contract table (this map originally had it as
  `port:chiefing`).
- `tests/shim.test.ts` — actually locks two unrelated things (674 lines):
  generated-schema export drift (stays chiefing,
  `test/GeneratedSchemas.test.ts`) and tool catalog/alias/breaker behavior
  (belongs to piing) → `split`, not ported whole as this map originally had
  it. Corrected against #835's Contract table.

**Reconciliation note (post-merge follow-up, `revamp/tests-ci/triage-map-corrections`):**
issue #835 (E9-S3, filed after this map's first pass) publishes the
authoritative per-file destination table for the chiefing cluster, including
consolidating most staffing-verb files into two shared destinations —
`test/resources/StaffingPeople.test.ts` (hire/offboard/transfer/bench/
shutdown/model-authority-transport) and `test/resources/StaffingDepartments.test.ts`
(create/pause/appoint-head/reparent) — rather than one file per verb. Every
`target` path in the chiefing cluster below has been realigned to that table;
each changed row's `reason` cites `#835` so a reader can tell a reconciliation
from an invention.

Two files need a Bun→Node rewrite during the port (vitest runs under Node,
not Bun — see epic #839's Conventions section):
`sse-watcher-multiplex.test.ts` (→ `test/sse/SseHub.test.ts`, not
`SseWatcher.Multiplex.test.ts` as this map originally had it) and
`durable-store-in-process-fetch.test.ts`.

### `cli` — 24 files → `port:cli`

Launcher/triber command surface: chiefd-cli, org control/target/caller-auth
commands, triber attach/ls/reset/stop/cli, `sql-location-resolution`,
`hire-model-authority` (CLI half; the transport half stays in `chiefing`),
and the slug-truncation boundary owned by the canonical CLI legacy
identifier helper.
Plus one chiefing override added in the post-merge reconciliation:
`tests/org-journal-marker-retention.test.ts` (`#492`'s caller-side sweep
policy, `apps/cli/test/JournalMarkerRetention.test.ts`) — its `cluster`
field stays `"chiefing"`, same reasoning as the piing-cluster note above.
Executed by E4 (no per-file sub-story filed yet at the time of this map;
each row's `story` field names the epic and should be tightened to the
specific E4 story once E4 files one for the test-port work).

### `piing` — 20 files (16 direct + 2 `split` + 2 `retire:obsolete`) → `port:piing` / `split` / `retire:obsolete`

`tests/theme.test.ts` and `tests/theme-adaptive-resolution.test.ts` are the
two `retire:obsolete` rows, and they got there the way this vocabulary
intends rather than as a laundered debt: both WERE ported, and both ports
were later deleted with their subject. Chief generates no theme file for
anybody — `makeTheme`, `makeThemeVariant`, the mode palette and
`organizationPersonThemeFileNames` are gone, and with them the
`operator`/`ceo` appearance split that decided who got a generated theme —
so there is no per-person theme document left for either suite to assert
about. `status: "ported"` asserts a live destination (rule 7), so a row whose
destination was deleted has to say what actually happened instead.

Pi runtime, theme, provider, and catalog surface: `ModelPolicy`,
`ProviderProjection`, `CapabilityPolicy`, `ResourceCatalog`, theme
adaptation, accent-identity uniqueness, provider credential/auth.json
projection. Two files are the epic's named multi-thousand-line carve-outs,
**not** ported directly by E9-S4:

- `tests/org-intercom.test.ts` (15,933 lines, the 42 `org_*` tool surface)
- `tests/team-ui.test.ts` (2,841 lines)

Both are `split`: broken up per-tool-family / per-component when the
extension sources land in piing (E3-S6 / E4-S7 / E4-S8). **Follow-up filed:
#843** — covers both carve-outs, with E3-S6 / E4-S7 / E4-S8 named as the real
prerequisites (landing the extension sources) before the split can execute.
Both rows' `story` fields point at #843.

This cluster also gains two chiefing overrides in the post-merge
reconciliation above: `tests/structural-root-gate-270.test.ts`
(`port:piing`, `packages/piing/test/extensions/StructuralRootGate.test.ts`)
and the catalog/alias/breaker half of `tests/shim.test.ts` (`split`,
executed alongside E9-S3's chiefing-side schema-drift half). Their `cluster`
field stays `"chiefing"` (that's where the classifier found them and where
the rest of their sibling rows live) — the JSON's `disposition` field, not
`cluster`, is what actually routes the work, which is why the cluster-count
table above counts by `cluster` while this narrative calls out
disposition-level overrides explicitly.

### `extensions` — 15 files → `port:piing`

Extension-behavior coverage that isn't part of the intercom/team-ui
carve-outs: attached-input observability, card-style vocabulary, footer
staleness, model-change orchestration, session-maintenance receipts,
staleness refusal, tavily-search, zipbox-tribe-addons. Ports alongside the
piing cluster proper under E9-S4, same package.

### `org-engine` — 96 files → `retire:rust`

TS business logic (supervision/goals, staffing, locks, mailbox, tmux
runtime, materialize, health/diagnostics) that chiefd's Rust core absorbs.
Per issue #834 step 4, this cluster uses **per-group** Rust targets where a
group maps cleanly to one Rust test file, rather than a unique target per
file — every row still names a target that exists today under
`apps/chiefd/`:

| Group (files) | Rust target |
|---|---|
| Locking/contention/mutation-journal (11) | `apps/chiefd/crates/chiefd-daemon/tests/single_writer_admission.rs` |
| Supervision ledger / goals / session-maintenance (17) | `apps/chiefd/crates/chiefd-core/tests/live_control_session_maintenance.rs` |
| Activity/memory row-repository (4) | `apps/chiefd/crates/chiefd-core/tests/conformance_activity.rs` |
| Staffing/reorg/transfer/lifecycle (9) | `apps/chiefd/crates/chiefd-api/tests/org_department_reparent_http.rs` |
| Mailbox/intercom changefeed (7) | `apps/chiefd/crates/chiefd-api/tests/normalized_changefeed_http_surface.rs` |
| Task command/notification/work-item (5) | `apps/chiefd/crates/chiefd-api/tests/tasks_http_surface.rs` |
| Supervisor ownership / duty scheduling (3) | `apps/chiefd/crates/chiefd-core/src/actor/writer.rs` |
| Telegram gateway / inbound seam (2) | `apps/chiefd/crates/chiefd-api/tests/org_row_seam_b4.rs` |
| tmux/runtime/rendering (13) | `apps/chiefd/crates/chiefd-host/tests/tmux_trust_rules.rs` |
| Store/materialize/health/diagnostics/contracts (remainder, 25) | `apps/chiefd/crates/chiefd-api/tests/docstore_http_surface.rs` |

The last row is a deliberately conservative default rather than a precise
per-behavior citation — as each group is actually ported during E7, its
row's target should be tightened to the specific Rust test (or new test)
that carries the assertion forward, and its `status` flipped to `retired`.
This map's job is to make that a bounded, reviewable list, not to
pre-write the Rust tests.

### `park-realdaemon` — 36 files → `park:e2e`

Real-daemon/real-tmux soaks that ran in the *unit* lane by history (they
import `tests/e2e/harness/*`), not because they're hermetic. The
`scripts/ci-shard.ts` `WEIGHTS` table confirms multi-second-to-multi-minute
real-process runtimes (`org-staffing-lifecycle` 272s,
`operator-only-person-transfer` 270s, `org-runtime` 243s, `org-units` 173s,
etc.). All park with the E2E corpus, including the 3 harness self-tests that
live inside `tests/e2e/harness/` itself (see "Known drift" above).

### `meta` — 15 files → `retire:meta` (13) / `retire:obsolete` (2)

Files pinning the pre-monorepo repo/CI layout. Three (`ci-workflow`,
`package-scripts`, `ci-shard-sweep`) were already parked by rename (E0-S2,
#753) with `status: "parked"`. Every row names its replacement: the 4
`env-*-lint` files → `lucy/no-process-env` + each app's `src/common/env.ts`
convention (E0-S3); `busy-vocabulary-hygiene` / `company-vocabulary` →
`none — dropped` (glossary enforcement moves to the terminology rules
themselves, chief/CLAUDE.md §Terminology, not a standalone corpus test);
`sse-watcher.test.ts` → `retire:obsolete`, superseded once E2 leaves exactly
one `SseWatcher` copy; `canonical-chiefd-root.test.ts` → the single
binary-path constant E9-S1 introduces; `conformance-corpus.test.ts` →
`retire:rust`, pointing at the Rust conformance replay
(`conformance_activity.rs` / `conformance_assignment.rs`), with a named gap
for the session-maintenance/tools fixture families still needing a Rust
replay target.

### `other` — 40 files → mixed

Deploy-command guards (6, → `port:cli`), retired-transport/retired-field
tripwires (`durable-store-curl-stderr-leak`, `organization-revision-tripwire`,
`revisionless-memory-append-producers` → `retire:obsolete`), repo-invariant
guards that belong in `scripts/test/` rather than `tests/` going forward
(`sql-only-state.test.ts`, `sql-only-operator-scripts.test.ts`,
`sse-poll-conversion-grep-gate.test.ts` → `retire:meta`), the deleted
`tmux-single-writer-p1` evidence lane's remaining two files (→
`retire:obsolete`, E0-S2), and a handful of genuine piing/cli ports
(`ifgenerationnot-extension-callers`, `launcher-theme-materialize`,
`pi-home-files`, `pi-startup-nonblocking`, `skill-invocation-hygiene`,
`org-intercom-sse-seam-gate` → `port:piing`; `org-task-cli`,
`tmux-socket-teardown`, `triber-fallthrough`, `orphanable-spawner`,
`promote-chiefd-guards`, `release-chiefd-installs-single-command` →
`port:cli`; `sse-watch-conformance` → `port:chiefing`).

**`tests/sql-only-state.test.ts`** has been ported into
`scripts/test/sql-only-state.test.mjs` as a standalone `node --test`
Mandate-2 guard, the same shape as this story's own guard, and its row's
`status` is `ported`. #976: the `.ts` twin was then deleted outright — it
ran under no configuration (`typecheck.sh` excludes `tests/**`, no vitest
config includes it) and its silent divergence from the wired `.mjs` caused a
real red-canonical incident (#899). Its two assertions the `.mjs` did not
already cover were migrated first, each demonstrated failing for the right
reason, before the file was removed.

## Support files (47 rows, `lane: "support"`)

Not tests, but the corpus depends on them, so the map accounts for them:
`tests/helpers/` (12 non-test modules), `tests/fixtures/` (6),
`tests/migration/` (2 — `crashsafe-holder.ts` and
`supervisor-handoff-driver.ts`, both still executed today by Rust:
`chiefd-locktest`'s `crash_safe_lock_interop.rs` and `chiefd-e2e`'s
`supervisor_handoff_byte_identity.rs`; both `retire:obsolete` with
`story: E8-S7`, which deletes the driver together with the Rust test that
execs it once its TypeScript subject — the file lock or the supervisor — is
itself deleted by E8-S6/E8-S3), `tests/setup-durable-store.ts` (the bunfig
preload, superseded by `@chief/testing`, E9-S1), `tests/e2e/harness/`'s 18
non-test modules (`park:e2e`, except `hold-mutation-lock.ts` which is
`retire:obsolete` — E8-S6 deletes it with the lease lock it holds, as a
deliberately unprotected module),
`tests/e2e/fresh-org/`'s 4 support modules (`park:e2e`), and
`tests/manual/`'s 4 capture-proof scripts (`park:e2e`, companions to the one
manual test).

## 2026-08-09 (#1035): the corpus is deleted, the record is not

140 of the 178 files under `tests/` are deleted. The first test: **can the
file resolve its own relative imports?** Every one of the 135 named at least one
module that does not exist — the pre-monorepo repo-root `src/` tree, or
`apps/cli/src/legacy/**`, both removed by the #751 Rust port. A file that
cannot link cannot be run, ported, or read as a specification of anything
that still exists.

The second test is whether it survives its own first read: five files linked
fine and died on a `readFileSync` of a path an earlier deletion had removed.
They went too — including `unreachable-watchdog.test.ts`, whose subject was
the corpus itself and whose vacuity floor (500 parsed test blocks) this
deletion takes away; retuning that floor downward would leave a guard that
asserts nothing.

38 files survive, and the reason is not sentiment: they resolve, and they
assert something about the tree as it is today rather than as it was. That
set is the bunfig preload chain (`setup-conditional-preload.ts`,
`setup-workspace-build-preflight.ts`, and the runtime ordering proof plus its
fixtures) and roughly thirty repo-invariant greps whose assertions exist
nowhere else in the repository — unresolved conflict markers, the SQL-only
operator scripts, the retired-Tribes-proxy absence, the sub-second-poll ban,
the public-guide company vocabulary, the deploy-script shapes. They are wired
to no lane, which is a real and separate gap; it is not a licence to delete
the only place an invariant is stated.

**Every row stays.** 134 rows move to `retired`: 89 already carried a
`retire:*` disposition (the decision was recorded, only the status moved),
and 45 carried `port:*` / `park:e2e` / `split` / `keep:active` — a claim of
work owed against a file that no longer exists — and are restated as
`retire:obsolete` with the reason stated per row. Removing a row instead
would be exactly the silent deletion `capturedAt` exists to make impossible,
the same reasoning that restored the 128 `tests/e2e/` rows.

`tests/org-intercom.test.ts` (15,933 lines, disposition `split`) is deleted
and its row's `target` now names
`packages/piing/test/toolcontract/OrganizationToolContract.test.ts`. #843's
tool-coverage backlog is repointed there rather than dropped: that suite
drives the same `installOrganizationIntercom` surface through the registered
`execute` against a real chiefd, and unlike the file it replaces, it runs.

## Cross-links

- `docs/testing/parked-suite-triage.json` — the machine-checked authority.
- `scripts/test/parked-suite-triage.test.mjs` — the guard.

## Mandate compliance

- **Mandate 0 (forward only).** No file carries two dispositions (guarded);
  the map is the single decision record, not a "keep both" outcome.
- **Mandate 1 (reactive-only).** The guard is synchronous
  reads only: `grep -rnE "setInterval|Atomics\.wait|sleep" scripts/test/parked-suite-triage.test.mjs` → 0 matches.
- **Mandate 2 (state in SQLite through chiefd).** The map is a committed
  source document, not runtime state: `grep -rn "parked-suite-triage" apps packages | wc -l` → 0.
- **Mandate 3 (business logic in Rust).** No product logic added in any
  language: `git diff --stat -- src extensions apps/chiefd/crates` against
  this story's branch touches none of those paths.
