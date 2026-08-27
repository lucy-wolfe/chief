# chiefd architecture

chiefd turns a company specification into a durable Pi workforce. It keeps the
organization model in SQL, decides who should be running, and gives each person
an isolated Pi home, history, workspace, and memory. **It does not run anyone.**
A client does that, and a runtime observation is a reproducible view of the
model rather than a second copy of it.

## Source map

There is no root `src/`. The product is the Rust workspace:

```text
apps/chiefd/crates/       # nine crates; this listing is the whole workspace
├── chiefd-daemon/   # the BACKEND binary: `run`, `docstore-only`, `bootstrap-store`,
│                    #   `memory-worker`, `set-actuation-config`, `clear-breaker`.
│                    #   One daemon per COMPANY DIRECTORY.
├── chiefd-core/     # the typed docstore — manifest and every ledger, as SQL rows.
│                    #   Pure with respect to the host: no tmux, no Pi, no filesystem.
├── chiefd-api/      # the HTTP surface over that store: request/response types,
│                    #   their `schemars` derivation, and the axum router
├── chiefd-host/     # everything the BACKEND touches on the machine
│                    #   (materialize, credentials, launch catalog, health)
├── chief-cli/       # the CLIENT, installed as `chief`: operator verbs, `host`,
│                    #   placement, actuation, tmux, the terminal. Depends on
│                    #   none of the four above — it speaks HTTP.
├── chiefd-log/      # the daemon-level observability stream, above the
│                    #   per-company JSONL logs
├── host-primitives/ # the host answers BOTH actuators need identically —
│                    #   rendezvous files, redaction, path shapes
├── identity-keys/   # a leaf: where a non-person identity key lives on disk,
│                    #   and the 0600 mode it must have
└── beacond/         # the small no-auth discovery daemon
apps/web/            # the browser client, and a full second host for a person
packages/chiefing/   # the ONLY TypeScript client of chiefd and beacond
packages/piing/      # Pi artifacts: skills and extensions copied into Pi homes
packages/testing/    # the shared vitest harness that boots a real chiefd
packages/eslinter/   # the repo's own ESLint rules
```

`packages/piing/extensions/` are copied into Pi homes; they must stay
self-contained.

## Authoritative flow

```text
chief (the client) / apps/web / a protected Pi tool
        │
        │  @chief/chiefing — the ONLY TypeScript client. No business logic above
        │  this line. chief-cli is Rust and carries its own hyper client.
        ▼
beacond  GET /v1/lookup, /v1/list, /v1/data-root
        │      which companies exist, where each one's chiefd is, and the one
        │      durable data root (<home>/.chiefd). Every caller resolves its own
        │      company here — `resolveCompanyChiefdUrl` in TypeScript, `Discovery`
        │      in chief-cli. There is no per-process chiefd address and no
        │      fixed-port fallback: "I don't know where it is" is an error.
        │      apps/chiefd/crates/beacond/src/directory.rs.
        ▼
chiefd   POST /v1/org/*        ← chiefd-api/src/docstore/{router,runtime_routes,
        │                          actuation}.rs
        │
        ├─ chiefd-core   the manifest and every ledger, as SQL rows in this
        │                company's own database. One BEGIN IMMEDIATE per
        │                mutation; the writer actor is the only writer.
        │                runtime/desired.rs answers "who should be running";
        │                store/{organization,activity,supervision,runtime_rows,
        │                session_maintenance,company_session_action,
        │                runtime_ownership,runtime_actuation,model_command}.rs
        │
        └─ chiefd-host   everything the BACKEND touches on the machine.
                         agent_home  ──►  <company>/.chief/agent/<person>/
                                             (non-Chief, created once)
                                          <company>/.pi/skills/
                                             (seeded once at genesis)
                         converge_apply/  ──►  the launch catalog and the
                                               person-scoped action stream
        ▲                                                  │
        │  POST /v1/org/runtime/observed                   │  POST /v1/org/runtime/
        │  (what the client actually saw)                  ▼  {actions,launch-catalog}
        └──────────────  chief-cli/src/{placement,actuate/*}  ──►  panes / windows
                         or apps/web's own host  ──►  browser panes
```

The arrow back into chiefd is the important one. The daemon has no way to look
at a runtime; it knows only what a client reported, and it says so — an
observation is `trusted` or `untrusted` as an enum rather than a people list
beside a flag, because "untrusted, and here are zero people" would read
downstream as *nothing is running* and mandate starting the whole company a
second time on top of one already up.

The SQL manifest is the structural source of truth. There is no `org.json`, no
`state/*.json`, no `location.json` and no pid file: every fact that is not a
Pi home or an agent workspace is a row, and the only tree on disk is
`<data-root>/orgs/<company>/` (mandate 5). Runtime snapshots are derived
observations, never authority. The runtime first materializes person resources
and preflights Pi argv, then converges from the durable graph. A converge pass
can be repeated safely after a crash, and only one may apply at a time:
`converge_safety`'s durable single-flight claim is what serializes them, not a
lock. The `tmux_writer_lease` that once did this job is deleted with the
`.org.lock`/`.runtime.lock` files (mandate 4) — beacond has already admitted one
daemon per company, and that daemon's writer actor is one thread running one
`BEGIN IMMEDIATE` per mutation.

The boundary between the two halves is measured, not asserted.
`scripts/test/backend-tmux-boundary.test.mjs` fails if any `.rs` file under
`chiefd-core`, `chiefd-host`, `chiefd-api` or `chiefd-daemon` names tmux **in
code**, with no exception list; it strips comments first, so the tombstones
explaining why tmux left are allowed to stay and a grep will find them. It also
enforces the dependency direction both ways.

A person's Pi configuration is Pi's own domain, and since #1307 chief does not
stage any of it. chief no longer sets `PI_CODING_AGENT_DIR`, so Pi resolves
`~/.pi/agent` from the user's home for any working directory — one sign-in, one
provider registry, one set of defaults, inherited by the operator and every
person alike. chief writes no `auth.json`, no `settings.json`, no `trust.json`,
no `keybindings.json` and no `models.json` anywhere. The one thing it redirects
is transcripts, through Pi's own `PI_CODING_AGENT_SESSION_DIR`. A consequence
the operator chose explicitly: the default model is shared, so anybody's
`/model` moves the default for everybody's fresh sessions. Model catalogs come from Pi itself:
native providers use Pi's built-in discovery, and configured endpoints are
registered at session start by the copied `zipbox-tribe-addons` extension,
which reads a non-secret provider contract (`ORG_CUSTOM_PROVIDERS`, projected
from the operator's root registry into the pane argv by
`chiefd-host/src/converge_apply/resource_catalog.rs`), fetches the provider's `/v1/models`
in its async extension factory, and calls `pi.registerProvider()`. Literal
credentials travel only on a 0600 `<pi-home>/.provider-credentials.json` the
extension reads inside the Pi process. When Pi is embedded as an SDK library
rather than run as the CLI, extension auto-discovery from
`<agentDir>/extensions/` does not happen: a library consumer must wire
`zipbox-tribe-addons` explicitly through `DefaultResourceLoader`
(`additionalExtensionPaths` or `extensionFactories`), or set `agentDir`/`cwd`
so the loader discovers it.

There is one data root, `~/.chiefd`, and no per-user location registry: the
`location.json` index that once recorded a per-slug root is gone with mandate 5
(`chiefd-host/src/runtime_lifecycle.rs`). `beacond` answers which companies
exist and where each one's chiefd is; there is no directory scan and no
source-local fallback.

## Key responsibilities

- **chiefd-core/store** validates and atomically persists the recursive
  company, department, contract, and person graph, plus every ledger.
- **Staffing/units/activity** implement hire, bench, transfer, lifecycle
  handoff, and memory boundaries, as rows in that store.
- **chiefd-host/materialize** writes immutable person contracts, explicit
  resource plans, isolated homes, and private memory directories.
- **chiefd-host/converge_apply** publishes the launch catalog and the
  person-scoped action stream. The action stream is computed at read time and
  never stored, because a stored copy is a second answer to "what should happen
  now" that goes stale between passes.
- **chief-cli/{placement,actuate}** maps that to one explicitly owned tmux
  session and fails closed on foreign, duplicate or unprovable ownership.
  `actuate/trust.rs` classifies tmux's literal stderr strings as provably absent
  versus merely unproven, and only a *proved* absence permits a rebuild.
- **Supervision** persists assignments, acknowledgement deadlines, progress,
  replacement, and deterministic delivery effects.
- **piing extensions/UI** render Pi cards and the footer and communicate
  through chiefd's protected transport; they do not mutate the organization
  directly.

## Further reading

- [What is a company?](WHAT_IS_A_COMPANY.md) is the short product guide.
- [Organization architecture reference](ORGANIZATION_ARCHITECTURE.md) covers
  durability, activity, supervision, and security invariants in depth.
