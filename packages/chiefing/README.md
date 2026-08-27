# @chief/chiefing

**The only TypeScript client of `chiefd` and `beacond`.** Every TypeScript
caller — the web app, the Pi extensions in
[`@chief/piing`](../piing/README.md), the test suites — reaches a company
through this package. Nothing above it re-implements discovery, transport, or
the wire types, and nothing imports its internals: `src/index.ts` is the only
barrel, and deep imports of `src/**` are a lint failure.

The Rust client, `chief-cli`, is the other half of that rule. It carries its own
hyper client and does not go through here. Two clients, one wire contract —
`scripts/test/*-shape-drift-check` guards keep the two shapes from drifting.

## What is in it

| Area | What it owns |
|---|---|
| `discovery/` | Finding a company's daemon: the `<dir>/.chief/run/daemon.json` rendezvous file, `resolveCompanyChiefdUrl`, the beacond `DiscoveryClient`, and each person's Pi home path. |
| `transport/` | The authenticated HTTP transport to chiefd, including how a caller presents its bearer. |
| `sse/` | The change-feed reader over chiefd's `GET /v1/docs/watch`, so a client reacts to a durable change over a socket read rather than a timer. |
| `resources/` | Typed accessors for the org resources chiefd serves — staffing, activity, supervision, runtime. |
| `types/` | The wire types, matching `chiefd-api`'s `deny_unknown_fields` structs. |
| `extensionruntime/` | The subpath export `@chief/chiefing/extension-runtime`. See below — this one is special. |

`ChiefdClient` is the front door for most callers.

## The `extension-runtime` subpath is COPIED, not imported

`@chief/chiefing/extension-runtime` is not a normal import. Pi extensions run
inside a Pi home, which has no `node_modules` pointing back at this workspace,
so the runtime's source and its whole relative-import closure are
**materialized into that home** as files.
`chiefingExtensionRuntimeSourceEntry()` is the entry point that materialization
starts from.

The consequence for a contributor: anything reachable from
`src/extensionruntime/index.ts` must stay resolvable as plain relative source.
A new dependency there is not a `package.json` line — it is another file copied
into every Pi home.

## Failure is typed, and "I don't know" is an error

The error classes exported from the barrel (`ChiefdUnavailableError`,
`BeacondUnavailableError`, `CompanyNotRunningError`, `DiscoveryRefusalError`,
`CompanyLifecycleRefusalError`, `AuthAcquisitionError`, …) exist because the
callers must tell these apart. There is no fixed-port fallback and no
per-process chiefd address: if discovery cannot say where a company's daemon is,
that is a refusal with a reason, never a guess.

## Tests

`bun run test` at the root, or `vitest run` in this directory. The suite boots a
real `chiefd` through [`@chief/testing`](../testing/README.md) rather than
mocking the wire — see that package's README for how the daemon URL reaches a
test.
