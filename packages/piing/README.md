# @chief/piing

**The Pi side of chief.** Two different things live here, and the difference
matters more than anything else in this README:

1. **`extensions/` and `skills/` are ARTIFACTS.** They are copied, as files,
   into each person's Pi home. They are not linked, bundled, or imported.
2. **`src/` is a normal library**, imported through the `@chief/piing` barrel
   like any other workspace package.

Everything below follows from that split.

## `extensions/` — copied into Pi homes, therefore SELF-CONTAINED

Each file in `extensions/` is a Pi extension that runs inside a Pi process, in a
directory that has no `node_modules` pointing back at this workspace. So an
extension may import:

- Pi's own packages (`@earendil-works/pi-coding-agent`, `@earendil-works/pi-tui`),
- other files in `extensions/` by relative path,
- the copied extension runtime from
  [`@chief/chiefing`](../chiefing/README.md)'s `extension-runtime` subpath,
  whose closure is materialized alongside it.

It may not reach anywhere else in the workspace. **This is the reason
`organization-intercom.ts` is one large file and stays one large file** — it is
a shipped artifact, and splitting it changes the artifact's shape, not just its
source layout. `docs/ARCHITECTURE.md` states the same rule.

| Extension | What it does |
|---|---|
| `organization-intercom.ts` | The org toolset a person is granted: messaging, staffing, structure, supervision — every protected verb, over chiefd's transport. |
| `organization-activity-status.ts` | The status line a person publishes (working / idle / blocked) and the activity writes behind it. |
| `organization-runtime-policy.ts` | Runtime policy the pane enforces locally. |
| `team-ui.ts`, `card-style.ts` | The cards and the footer — chief's whole visible surface inside a pane. `docs/cards-style.md` is the style contract. |
| `founder-launch.ts`, `tribes-welcome.ts`, `chief-logo.ts` | Founder mode: the pre-company identity, its welcome, and its mark. |
| `org-send-replay.ts`, `bus-events-bounded-append.ts`, `attached-input-observability.ts` | Delivery replay, the bounded event trail, and what the pane can observe about its own input. |

Extensions mutate nothing directly. They speak to chiefd over its protected
transport, and chiefd decides.

## `skills/` — copied into a company's skill root

Skill directories seeded into `<company>/.pi/skills/` once, at genesis:
`browser`, `fal-ai`, `founder-launch`, `market-data`,
`organization-management`, `project-status-reporting`. Same rule as extensions —
they are content, not code this workspace links against.

## `src/` — the library half

| Module | What it owns |
|---|---|
| `runtime/PiAttestation.ts` | The PINNED Pi version and its artifact digests, plus `attestPiRuntime()`. |
| `runtime/PiBinary.ts`, `runtime/PiPaths.ts` | Resolving the Pi binary and the paths around it. |
| `home/IdentityTheme.ts` | A person's identity colour — the same accent the rail draws. |
| `policy/CapabilityPolicy.ts` | Which capabilities a person's toolset may contain. |
| `extensionruntime/` | The piing-side half of the copied runtime. |

The `#751/G5` note at the top of `src/index.ts` is worth reading before adding
anything here: the pi-home materialization, pane-argv and session modules were
**deleted** from this package rather than kept, because the live implementation
is in Rust (`chiefd-host/src/materialize/**`). A second implementation nobody
calls is exactly the drift this package exists downstream of. Do not
reintroduce one.

## Tests

`vitest run` in this directory, or `bun run test` at the root. Extension tests
live in `test/extensions/`, and they exercise the artifact as the artifact —
that is why they are worth reading first when changing one.
