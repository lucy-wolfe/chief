# @chief/testing

The shared chiefd `docstore-only` vitest harness: it resolves the `chiefd`
debug test binary (honoring `CARGO_TARGET_DIR`), allocates an ephemeral port,
owns a temp data root, boots the real daemon, waits for reachability with a
bounded awaited loop, and tears it down leak-free on both macOS and Linux.

This is the vitest-native successor to `bunfig.toml`'s
`preload = ["./tests/setup-durable-store.ts"]` (one daemon per test
**process**), which is deleted as of #1035: `@chief/testing` gives every
package one daemon per package vitest **run** instead, with no copy-pasted
spawn code anywhere.

## Wiring a package for a real docstore

1. Declare the dependency:

   ```json
   // package.json
   "devDependencies": {
     "@chief/testing": "workspace:*"
   }
   ```

2. Add a three-line global setup:

   ```ts
   // test/DocstoreGlobalSetup.ts
   import { createDocstoreGlobalSetup } from '@chief/testing'

   export default createDocstoreGlobalSetup({
     owner: { kind: 'company', slug: 'your-package-fixture' }
   })
   ```

3. Wire it into `globalSetup` alongside the existing workspace-build guard:

   ```ts
   // vitest.config.ts
   test: {
     globalSetup: [
       '../../scripts/test/assert-workspace-built.mjs',
       './test/DocstoreGlobalSetup.ts'
     ]
   }
   ```

4. Read the daemon's URL from vitest's provided context in any test:

   ```ts
   import { inject } from 'vitest'

   const url = inject('chiefdUrl')
   const client = new ChiefdClient({ url, root: inject('chiefdDataRoot') })
   ```

`inject('chiefdSlug')` is also available (the company slug, or the
`no-company` reason string, whichever `owner` declared).

## The URL is provided, never exported

`createDocstoreGlobalSetup` publishes the daemon URL through vitest's
`provide()`, and a suite reads it back with `inject('chiefdUrl')`. It writes
nothing into the ambient process environment.

It used to also export the URL under the pane env stamp chiefd published, so a
spawned child could inherit it. That stamp is retired — a company's daemon is
resolved from beacond — and one process-global address was never right for a
harness that can be asked for a second company in the same process. A suite
that spawns a child and needs the URL in that child's environment must put it
there itself, in the child's own constructed environment, under a name that
suite owns.

## Vitest runs under Node — `Bun.*` is unavailable in tests

This harness (and every package that uses it) runs its vitest suite under
Node, not Bun. Code exercised by a `@chief/testing`-backed test must not
reference `Bun.*` APIs — a legacy suite being ported that used `Bun.spawn`,
`Bun.file`, etc. needs those calls rewritten to Node equivalents first.

## What this package does NOT do

- It never reads org state, never speaks the domain (`/v1/org/*`) — it only
  boots a daemon and hands out its URL.
- It writes no marker/pid file (D0/D20) — leak safety comes from
  `CHIEFD_STORE_WATCH_PID` (a portable liveness watchdog the daemon polls)
  plus each daemon's own `stop()`, both proven by this package's own tests.
- `tests/setup-durable-store.ts` (the legacy preload) is DELETED (#1035).
  It was never extended and never run by any lane, and it statically imported
  `apps/cli/src/legacy/foundation/paths`, which #751/P0 removed — so it could
  not even link. This package is the whole story now.
