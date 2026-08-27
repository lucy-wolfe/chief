# Documentation map

Every document in this repository, in reading order, with who it is for.

**Audience key** — 📘 newcomer · 🔧 contributor · 🛠 operator · 📜 history

## Start here

| Doc | For | What it is |
| --- | --- | --- |
| [`../README.md`](../README.md) | 📘 | What chief is, how to install it, and every human-facing command. |
| [`WHAT_IS_A_COMPANY.md`](WHAT_IS_A_COMPANY.md) | 📘 | The product in 87 lines: what a "company" of agents actually is. |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 📘 🔧 | The source map and the authoritative flow — client, daemon, store, host. Read this before any code. |
| [`OPERATING.md`](OPERATING.md) | 🛠 | The operator's reference: install and upgrade, the disk layout, every command, the startup and capacity knobs, and how the runtime behaves. The depth the README used to carry. |
| [`../examples/`](../examples/) | 📘 | Three companies you can copy and found in one command — a trading desk, a growth agency, and an open-source maintenance team. Three markdown files each, no code. |
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | 🔧 | Setup, the standing check list, and the conventions a pull request is reviewed against. |
| [`../AGENTS.md`](../AGENTS.md) | 🔧 | The same working agreement, written for the coding agents that do most of the work here. Longer and blunter, and the one kept most current. |
| [`../SECURITY.md`](../SECURITY.md) | 🔧 🛠 | What counts as a security bug in a tool that runs agents with real credentials, and how to report one. |

## The model, in depth

| Doc | For | What it is |
| --- | --- | --- |
| [`ORGANIZATION_ARCHITECTURE.md`](ORGANIZATION_ARCHITECTURE.md) | 🔧 | 1,200 lines on durability, activity, supervision, and the security invariants. The reference, not a tutorial. |
| [`organization-spec.md`](organization-spec.md) | 🔧 | The organization model stated as a specification. |
| [`cards-style.md`](cards-style.md) | 🔧 | The style contract for the cards and footer a person renders in its pane. Read before touching `packages/piing/extensions/card-style.ts`. |
| [`../apps/chiefd/README.md`](../apps/chiefd/README.md) | 🔧 | The Rust workspace: crate boundaries, the `test-support` feature policy, and the seam lints that hold the boundaries. |
| [`../packages/chiefing/README.md`](../packages/chiefing/README.md) | 🔧 | The only TypeScript client of chiefd and beacond. |
| [`../packages/piing/README.md`](../packages/piing/README.md) | 🔧 | The Pi artifacts — extensions and skills copied into Pi homes — and why they must stay self-contained. |
| [`../packages/testing/README.md`](../packages/testing/README.md) | 🔧 | The shared vitest harness that boots a real chiefd for a package's suite. |
| [`../apps/web/README.md`](../apps/web/README.md) | 🔧 | The browser host — **not live and currently broken**. Why it is still in the tree, and what reviving it would mean. |

## Working in the repo

| Doc | For | What it is |
| --- | --- | --- |
| [`agent-gotchas.md`](agent-gotchas.md) | 🔧 | The traps, written down as they were hit. Short and worth the five minutes. |
| [`verification-rules.md`](verification-rules.md) | 🔧 | What counts as having verified something here. |
| [`commit-namespace.md`](commit-namespace.md) | 🔧 | The commit-message namespace convention. |
| [`../conformance/README.md`](../conformance/README.md) and [`../conformance/FORMAT.md`](../conformance/FORMAT.md) | 🔧 | The conformance fixture format and runner. |

## Testing

| Doc | For | What it is |
| --- | --- | --- |
| [`testing/TEST_SUITE.md`](testing/TEST_SUITE.md) | 🛠 | **Operator-run, not CI.** The live end-to-end suite: it stages a real machine, installs a real build, creates a real company, and drives it. Substitute your own host — see its preamble. |
| [`../tests/manual/`](../tests/manual/) | 🛠 | **Operator-run, not CI.** One-off capture proofs. |

**Which greens are yours.** Everything under `.github/workflows/ci.yml` runs on
every pull request and is a contributor's responsibility. `testing/TEST_SUITE.md`
and `tests/manual/` are not wired to CI and never run on a pull request — a
maintainer runs them against real hardware before a release. Contributors are
not expected to.

The four `toolcontract` lanes DO run in CI, on public runners, against real tmux
and freshly built binaries. They take minutes and `bun run test` excludes them —
`CONTRIBUTING.md` explains how to run them by path and how to read their
skipped-versus-run counts.

## Runbooks

| Doc | For | What it is |
| --- | --- | --- |
| [`LIVE_RECOVERY_RUNBOOK.md`](LIVE_RECOVERY_RUNBOOK.md) | 🛠 | Recovering a live company. |

## History and audits

These record how something came to be. They are not maintained, and where they
disagree with the code, the code wins.

| Doc | For | What it is |
| --- | --- | --- |
| [`concept-collision-audit.md`](concept-collision-audit.md) | 📜 | The naming-collision sweep and what it resolved. |
| [`store-implementation-audit.md`](store-implementation-audit.md) | 📜 | The store implementation as audited at one point in time. |
| [`pi-patch-capability-losses.md`](pi-patch-capability-losses.md) | 📜 | What was lost when the Pi patch was deleted, so a future symptom has a name to match against. |
| [`testing/parked-suite-triage.md`](testing/parked-suite-triage.md) | 📜 | The parked legacy unit corpus, one row per file, and the re-entry criteria. |
| [`../CHANGELOG.md`](../CHANGELOG.md) | 📜 | Delivered behaviour, newest first. |
| [`../DECISIONS.md`](../DECISIONS.md) | 📜 | One dated line per product, UX, security, workflow, or architecture decision. |
