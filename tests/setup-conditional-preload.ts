/**
 * #962: `bunfig.toml`'s `[test].preload` is project-global — every `bun
 * test` invocation pays it, regardless of which files were actually named.
 * `tests/setup-workspace-build-preflight.ts` is a hard requirement for the
 * files under `tests/` that dial a real chiefd/beacond binary or a built
 * `@chief/*` workspace package. It is NOT a requirement for
 * `scripts/test/*.test.mjs`, a categorically different, lighter-weight
 * guard suite that runs in CI via `node --test scripts/test/<name>` (see
 * `scripts/gate-matrix-legs.mjs`'s `legCommand()`) and needs neither a
 * built workspace package nor a chiefd binary — proven by running it that
 * way against a bone-dry clone (#962's own investigation).
 *
 * The bug this file fixes is invocation-shaped, not suite-shaped: `bun
 * test scripts/test/doc-append-only.test.mjs` also works (bun's test
 * runner accepts `node:test`-authored files), so anyone who reaches for
 * `bun test` instead of the CI-wired `node --test` pays the FULL `tests/`
 * preload cost for a file that needs none of it — a full workspace build
 * plus a chiefd/beacond release build, for a guard that diffs
 * `CHANGELOG.md`/`DECISIONS.md` as plain text.
 *
 * A preload file cannot be made conditional FROM THE INSIDE when it has
 * static top-level `import`s: those resolve at module-link time, before
 * ANY of that file's own runtime code, no matter where a conditional is
 * placed inside it. Being LISTED in `bunfig.toml`'s preload array is what
 * makes bun evaluate it at all; nothing inside the file can prevent that
 * once listed. So this wrapper REPLACES the direct preload entry in
 * `bunfig.toml`: it is loaded unconditionally (cheap: no imports at its
 * own top level), and only `import()`s (dynamic -- deferred until this
 * code actually runs) the real preload file when the invocation's own
 * targets say it is needed.
 *
 * The second preload entry, `tests/setup-durable-store.ts`, is DELETED
 * (#1035). It booted a shared `chiefd docstore-only` daemon for the parked
 * bun:test corpus and statically imported
 * `apps/cli/src/legacy/foundation/paths`, which #751/P0 removed along with
 * the whole `apps/cli/src/legacy/` tree — so it could not link, let alone
 * run. Its live successor is `packages/testing`'s `DocstoreDaemon`, which
 * every vitest package suite reaches directly. This wrapper keeps its
 * conditional shape with one entry rather than being inlined into
 * `bunfig.toml`: the CONDITION (don't make `scripts/test/*` pay for a
 * workspace build) is what #962 was about, not the arity of the chain.
 *
 * Deliberately fails OPEN (assumes `tests/` IS targeted, pays the full
 * cost) whenever it cannot prove otherwise: a bare `bun test` with no
 * path arguments runs the whole project including `tests/`, and any
 * argument this cannot confidently classify as OUTSIDE `tests/` must not
 * silently skip a real consumer's hard requirement -- that would convert
 * a loud "binary missing" failure into a quiet wrong answer (durable-store
 * tests running against no store at all), the exact defect class #962
 * exists to avoid introducing while fixing the reachability defect.
 */

// This file's own directory IS `tests/` -- comparing resolved arg paths
// against it, rather than a hardcoded string, so a repo move keeps working.
const TESTS_DIR = import.meta.dir

function invocationMayTargetTestsDir(): boolean {
  // `Bun.argv` for `bun test <patterns>` is `[bunBinaryPath, ...patterns]`
  // -- the `test` subcommand itself is not present (verified empirically:
  // `bun test scripts/test/x.mjs` yields
  // `["/usr/local/bin/bun", "/abs/path/to/scripts/test/x.mjs"]`).
  const patterns = Bun.argv.slice(1).filter((arg) => !arg.startsWith('-'))
  if (patterns.length === 0) {
    // Bare `bun test`: bun's own default discovery covers the whole
    // project, `tests/` included. Fail open.
    return true
  }
  return patterns.some((pattern) => {
    const absolute = pattern.startsWith('/') ? pattern : `${process.cwd()}/${pattern}`
    return absolute === TESTS_DIR || absolute.startsWith(`${TESTS_DIR}/`)
  })
}

if (invocationMayTargetTestsDir()) {
  // SEQUENTIAL, not concurrent, and it stays `await import()` even at arity
  // one: `await` suspends this function until the preflight's own module
  // evaluation (including its top-level `Bun.resolveSync` checks, which
  // throw synchronously during that evaluation) has fully completed before
  // anything after it runs -- an ECMAScript guarantee of `await`, not an
  // assumption about timing. `scripts/test/workspace-build-preflight-wiring.test.mjs`
  // resolves this chain statically (see its `readEffectivePreloadOrder()`),
  // and `tests/setup-conditional-preload.ordering.test.mjs` proves the
  // sequencing property at runtime against instrumented fixtures, so a
  // SECOND entry added here later cannot quietly become concurrent.
  await import('./setup-workspace-build-preflight')
}
