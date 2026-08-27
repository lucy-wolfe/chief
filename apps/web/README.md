# apps/web — the browser host

## Status: not live and currently broken

This app does **not** build a working host today. It is not started by
`chief`, it is not covered by the release artifacts, and nothing a user
installs contains it. The terminal client is the product.

Do not report it as a product bug. Issues and pull requests about it are
welcome under the `apps-web` label.

## Why it is still in the tree

The whole design of chief is a split: `chiefd` decides **who** should be
running, and a client decides **where** they are shown. `chief` is one client.
This app is the second one — a full browser **host**, not a viewer of the
terminal.

Deleting it would delete the only worked example of that second host, and the
split is the part of the architecture that is hardest to get right the second
time. So it stays, marked.

What it was built to do, and what the code still shows how to do:

- Build every tool chiefd granted a person, adapting Pi's own extension
  registrations into the agent's tool list with no filter, and reporting
  anything it cannot build in `unavailable` rather than dropping it.
- Drive a person's lifecycle from chiefd's `GET /v1/docs/watch` change feed,
  so a message or a reminder wakes its owner on a socket read rather than a
  timer.
- Measure real context usage, compact at Pi's own threshold, and replace a
  session at an idle boundary.
- Name no tmux socket, session, or pane id anywhere — its `paneId` is a person
  id.

The deferred-consumer contracts it must satisfy are declared in its own
[`package.json`](package.json) under `chief.privateWebDeferredConsumerContracts`:
the app-shell events provider, and the lifecycle, person, and company SSE
streams. Those entries are the contract; they name the file and the export.

## What does work

The unit suites run, and they run in CI on every pull request
(`bun run test`, `bun run typecheck`, `bun run lint`). A change here is held to
the same gates as anything else in the repo — the app is unmaintained, not
unguarded.

## Reviving it

There is no owner and no schedule. The daemon/client split was designed for a
second host and this code is kept for that reason, but reviving it is real
work nobody has taken. If you want to take it on, open a Discussion first —
the useful first step is a written account of what actually breaks, not a pull
request.
