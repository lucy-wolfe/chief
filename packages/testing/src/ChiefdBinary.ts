/**
 * The ONE definition of where the `chief` and `chiefd` debug test binaries live — the
 * vitest-native successor to `tests/e2e/harness/chiefd-binary-path.ts`
 * (which this module re-implements, not imports: `@chief/testing` never
 * imports the legacy tree). Honors `CARGO_TARGET_DIR` exactly like `cargo
 * build` does; a hardcoded path once made a harness "a blind instrument"
 * (gh#143) — with `CARGO_TARGET_DIR` set, `cargo build` writes the fresh
 * binary elsewhere while a hardcoded `existsSync` check finds a STALE
 * binary at the repo-relative path and reports success.
 *
 * EXISTENCE IS NOT IDENTITY (#751/P7 follow-up). That gh#143 note describes one
 * route to a stale binary; there are others, and they all end the same way. On
 * 2026-08-09 a missing `Cargo.lock` member made `cargo build --locked`
 * REFUSE on a clean checkout. The build failed, a binary from six hours earlier
 * was still sitting at the resolved path, `existsSync` said yes, and the whole
 * suite ran against a daemon that predated the code under test. Two tests went
 * red: one with a 404 for a route the old binary had never heard of, and one —
 * an authentication test — reporting that an unenrolled key was ACCEPTED, which
 * read as a live security hole and was escalated as one. It was not; the fence
 * simply was not in the binary being exercised.
 *
 * So this module now proves the binary is NEWER than the sources that produce
 * it, and says "this binary predates the code under test" in as many words. A
 * suite that boots "the daemon" must prove it booted THIS daemon.
 */
import { type Dirent, existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { isNullish } from '@/Nullish'
import type { ChiefdBinaryTestGate } from '@/types/ChiefdBinary'

// packages/testing/src/ChiefdBinary.ts (or the mirrored packages/testing/dist/
// path once built) sits exactly three directories below the workspace root —
// this module's own location IS the "resolved upward" default the Contract
// asks for, with no marker-file search needed.
function defaultRepoRoot(): string {
  const here = dirname(fileURLToPath(import.meta.url))
  return join(here, '..', '..', '..')
}

/** The cargo target directory chiefd is built into, honouring
 * `CARGO_TARGET_DIR` when set, falling back to the in-repo
 * `apps/chiefd/target` otherwise (E1-S1 moved the Rust workspace there). */
export function resolveChiefdTargetRoot(repoRoot: string): string {
  const override = process.env.CARGO_TARGET_DIR?.trim()
  return override ? override : join(repoRoot, 'apps', 'chiefd', 'target')
}

/** The full path to the debug test `chief` binary — the operator client. */
export function resolveChiefBinaryPath(repoRoot: string): string {
  return join(resolveChiefdTargetRoot(repoRoot), 'debug', 'chief')
}

/**
 * The full path to the debug test `chiefd` binary — the daemon — beside `chief`.
 *
 * TWO binaries since P6 of the design record. Every
 * harness here spawns `chiefd run` or `chiefd docstore-only`; the `chief`
 * binary is the operator client and answers neither — it `exec`s its SIBLING
 * `chiefd` for both. So a build that produced only `chief` gives a
 * harness that starts, execs into a missing file, and reports whatever a dead
 * child looks like from the outside. Resolved and checked by name instead.
 */
export function resolveChiefdDaemonBinaryPath(repoRoot: string): string {
  return join(resolveChiefdTargetRoot(repoRoot), 'debug', 'chiefd')
}

/**
 * The checkout's own pinned Pi, as an ABSOLUTE path — what `chiefd run`'s
 * `--pi-binary` must be told.
 *
 * `chiefd run` no longer defaults this. It used to fall back to the bare name
 * `pi`, publish that in every person's launch-catalog entry, and let whatever
 * PATH the pane happened to inherit decide whether anybody could start; on a
 * host where Pi was pinned rather than on PATH, every pane died at creation
 * and the actuator reported a window-dimensions error once a second. The
 * operator client now resolves it absolutely and passes it, so a harness has
 * to as well.
 *
 * Pinned to the CHECKOUT, for the same reason `--launcher-root` is pinned to
 * it right beside this in every spawn: a harness must exercise the tree under
 * test, never whichever Pi a developer happens to have on PATH.
 */
export function resolvePinnedPiBinaryPath(repoRoot: string): string {
  return join(repoRoot, 'node_modules', '.bin', 'pi')
}

/** The exact command that builds the binaries this module resolves. */
/** Files whose change invalidates a built `chief`: every Rust source in the
 * workspace, plus the manifests and the lockfile that decide what it links. */
const CHIEFD_SOURCE_SUFFIXES = ['.rs', 'Cargo.toml', 'Cargo.lock']

function isChiefdSource(name: string): boolean {
  return CHIEFD_SOURCE_SUFFIXES.some((suffix) => name.endsWith(suffix))
}

/** The newest source file that feeds the chiefd build, and when it changed.
 *
 * Walks `apps/chiefd` and skips `target/` — the build's own output, which is
 * always newer than its inputs and would make every comparison vacuous. */
export function newestChiefdSource(
  repoRoot: string
): { path: string; mtimeMs: number } | undefined {
  let newest: { path: string; mtimeMs: number } | undefined
  const walk = (dir: string): void => {
    // `Dirent[]`, NOT `ReturnType<typeof readdirSync>`.
    //
    // `readdirSync` is OVERLOADED, and `ReturnType<typeof f>` on an overloaded
    // function silently binds to the LAST overload — the buffer-returning one.
    // So this annotation asserted `Dirent<NonSharedBuffer>[]` while the call
    // below returns `Dirent<string>[]`, and had done since it was written. It
    // compiled only by coincidence: at `@types/node` 20.17.6 the last
    // overload's `Dirent` had no generic parameter, which made the wrong
    // annotation assignable. Bumping the types to match the runtime the Pi
    // extensions run on is what finally made the compiler able to see it.
    //
    // `Dirent`'s default type parameter is `string`, which is what
    // `withFileTypes: true` actually returns.
    let entries: Dirent[]
    try {
      entries = readdirSync(dir, { withFileTypes: true })
    } catch {
      return
    }
    for (const entry of entries) {
      // `target` is the OUTPUT. Including it would compare the build against
      // itself and never fire.
      if (entry.isDirectory()) {
        if (entry.name !== 'target' && entry.name !== 'node_modules') {
          walk(join(dir, entry.name))
        }
        continue
      }
      if (!isChiefdSource(entry.name)) continue
      const path = join(dir, entry.name)
      try {
        const { mtimeMs } = statSync(path)
        if (isNullish(newest) || mtimeMs > newest.mtimeMs) newest = { path, mtimeMs }
      } catch {
        /* a file that vanished mid-walk cannot be the newest one that matters */
      }
    }
  }
  walk(join(repoRoot, 'apps', 'chiefd'))
  return newest
}

/**
 * The newest source THIS binary is actually built from, read out of cargo's
 * own dependency file (`<binary>.d`), or `undefined` when there is no usable
 * one.
 *
 * # Why this exists
 *
 * The whole-tree walk below asks "is this binary older than ANY chiefd
 * source", and that question has a wrong answer whenever the workspace holds
 * binaries with different dependency graphs — which it has since P6 split
 * `chief` (the operator client) from `chiefd` (the backend).
 * `chiefd-daemon` does not depend on `chief-cli`, so editing
 * `chief-cli/src/preflight.rs` and running `cargo build` correctly
 * rebuilds `chief` and correctly leaves `chiefd` alone — and the
 * whole-tree walk then calls the untouched daemon stale, forever, because no
 * amount of rebuilding will move its mtime. Measured, not reasoned: a `cargo
 * fmt` of one `chief-cli` test module put ten `packages/chiefing` contract
 * suites and the `packages/piing` tool-contract suite permanently red, with a
 * message instructing a rebuild that provably could not fix it.
 *
 * An instrument whose remedy does not clear its own alarm is worse than no
 * instrument: the only way past it is to ignore it, and a check people learn
 * to ignore has stopped checking.
 *
 * # Why cargo's own file, and not a smarter walk
 *
 * `<target>/debug/<binary>.d` is a Makefile-shaped line cargo writes on
 * every build listing exactly the files that binary was compiled from. It is
 * not a second opinion about the dependency graph — it IS the graph, written
 * by the tool that owns it, so it cannot drift from what cargo rebuilds.
 * Inferring the graph here from crate names and `Cargo.toml` files would be
 * exactly the second source of truth this codebase keeps paying for.
 *
 * # Fail closed
 *
 * A missing, empty or unparseable `.d` file returns `undefined` and the caller
 * falls back to the stricter whole-tree walk. The strict answer's failure mode
 * is a rebuild nobody needed; the lenient one's is a stale daemon standing in
 * for a fresh one, which is the incident this module exists to prevent.
 */
function newestDependencyOf(binaryPath: string): { path: string; mtimeMs: number } | undefined {
  let line: string
  try {
    line = readFileSync(`${binaryPath}.d`, 'utf8')
  } catch {
    return undefined
  }
  const separator = line.indexOf(': ')
  if (separator < 0) return undefined
  // Cargo escapes a space inside a path as `\ `; splitting on unescaped
  // whitespace keeps a path with a space in it in one piece.
  const dependencies = line
    .slice(separator + 2)
    .split(/(?<!\\)\s+/)
    .map((entry) => entry.replace(/\\ /g, ' ').trim())
    .filter((entry) => entry.length > 0 && isChiefdSource(entry))
  let newest: { path: string; mtimeMs: number } | undefined
  for (const path of dependencies) {
    try {
      const { mtimeMs } = statSync(path)
      if (isNullish(newest) || mtimeMs > newest.mtimeMs) newest = { path, mtimeMs }
    } catch {
      /* a listed file that is gone cannot be the newest one that matters */
    }
  }
  return newest
}

/**
 * Throw when `binaryPath` is older than the newest chiefd source.
 *
 * The message is the point. A stale daemon fails in whatever way its missing
 * code happens to produce — a 404 here, an accepted call there — and every one
 * of those reads as a defect in the thing under test. Naming the real cause
 * turns an evening of bisecting into one line.
 *
 * Deliberately mtime, not a build hash: it needs no change to the Rust build,
 * it catches an uncommitted edit that a git hash would miss, and its false
 * positive (touching a file after building) asks for a rebuild, which is never
 * the wrong instruction. There is no env escape hatch — an override is how a
 * guard becomes decoration, and rebuilding is cheap.
 */
export function assertChiefdBinaryCurrent(binaryPath: string, repoRoot: string): void {
  const newest = newestDependencyOf(binaryPath) ?? newestChiefdSource(repoRoot)
  if (isNullish(newest)) return
  const builtMs = statSync(binaryPath).mtimeMs
  if (builtMs >= newest.mtimeMs) return
  throw new Error(
    `this chief/chiefd binary predates the code under test\n\n` +
      `  binary: ${binaryPath}\n` +
      `          built ${new Date(builtMs).toISOString()}\n` +
      `  newest source: ${newest.path}\n` +
      `          changed ${new Date(newest.mtimeMs).toISOString()}\n\n` +
      `Every failure below would be a failure of the OLD daemon, not of this ` +
      `checkout — a missing route answers 404 and a missing check answers 200, ` +
      `and both read as defects in the code you are testing.\n\n` +
      `Rebuild it (a build that REFUSED, e.g. on a stale Cargo.lock, leaves the ` +
      `previous binary in place and this is what that looks like):\n\n` +
      `    ${chiefdBuildCommand()}\n`
  )
}

/** The exact command that builds the binary this module resolves. */
export function chiefdBuildCommand(): string {
  return 'cargo build --locked --manifest-path apps/chiefd/Cargo.toml --bin chief --bin chiefd'
}

/**
 * Returns the binary path, or throws an Error whose message is exactly the
 * build command to run — never a confusing downstream failure far from the
 * must always be a hard failure regardless of environment — a caller that
 * asked to boot a real daemon and got nothing must never be told it was
 * skipped. Test SUITES that merely need the binary as a precondition should
 * use `chiefdBinaryTestGate` instead (below), which adds the local-skip/
 * CI-fail split #846 requires.
 */
export function assertChiefdBinaryBuilt(repoRoot?: string): string {
  const root = repoRoot ?? defaultRepoRoot()
  const binaryPath = resolveChiefBinaryPath(root)
  const daemonPath = resolveChiefdDaemonBinaryPath(root)
  // BOTH, and the daemon named first when it is the missing one: `chief`
  // present with `chiefd` absent is the confusing half of the P6 split,
  // and it fails at exec time inside a spawned child rather than here.
  const missing = [binaryPath, daemonPath].find((path) => !existsSync(path))
  if (isNullish(missing)) {
    // Present is not enough — see `assertChiefdBinaryCurrent`. Checked on BOTH
    // halves of the P6 split, and the daemon matters more: it is the one that
    // actually serves, so a stale `chiefd` beside a fresh `chief` is
    // precisely the shape that reports an old daemon's behaviour as this
    // checkout's.
    assertChiefdBinaryCurrent(binaryPath, root)
    assertChiefdBinaryCurrent(daemonPath, root)
    return binaryPath
  }
  const targetDirInForce =
    process.env.CARGO_TARGET_DIR?.trim() || '(unset — using in-repo apps/chiefd/target)'
  throw new Error(
    `chief/chiefd binary not found at ${missing}\n` +
      `CARGO_TARGET_DIR in force: ${targetDirInForce}\n\n` +
      `Build it first (CARGO_TARGET_DIR, if set above, must match the build's):\n\n` +
      `    ${chiefdBuildCommand()}\n`
  )
}

/**
 * Whether this process believes it is running in CI. Presence of the `CI`
 * env var is enough — its value is never inspected, matching every CI
 * runner's own convention (GitHub Actions, and everything else, sets
 * `CI=true` or `CI=1` unconditionally when the var is set at all). Mirrored
 * in `apps/chiefd/tests/e2e/src/lib.rs`'s `is_running_in_ci` — keep both in
 * sync if this changes.
 */
export function isRunningInCI(): boolean {
  return !isNullish(process.env.CI)
}

/**
 * The precondition every docstore-daemon-dependent test FILE must check
 * before describing its tests (#846): call this once at module top level
 * (never inside a test body — the skip decision must be made before
 * `describe`/`it` registration, so a missing binary produces a visible
 * `describe.skip`, not a `beforeAll` failure indistinguishable from a real
 * regression).
 *
 * - Binary present: returns `{ present: true, binaryPath }` — the caller
 *   describes its tests normally.
 * - Binary absent, NOT in CI: returns `{ present: false, binaryPath }` —
 *   the caller must `describe.skip(...)`, naming the gap in the describe
 *   block's own title (see `chiefdBinarySkipTitle`) so the skip is visible
 *   in every reporter's output, not just a log line that scrolls past.
 * - Binary absent, IN CI: throws immediately, via `assertChiefdBinaryBuilt`
 *   — CI always builds this binary as a prior pipeline step, so its
 *   absence there is a real break, never a routine local gap. A skip here
 *   would convert a loud failure into a silent one, which is strictly
 *   worse than the bug this exists to fix.
 */
export function chiefdBinaryTestGate(repoRoot?: string): ChiefdBinaryTestGate {
  const root = repoRoot ?? defaultRepoRoot()
  const binaryPath = resolveChiefBinaryPath(root)
  const present = existsSync(binaryPath) && existsSync(resolveChiefdDaemonBinaryPath(root))
  if (!present && isRunningInCI()) {
    assertChiefdBinaryBuilt(root)
  }
  // A stale binary throws in EVERY environment, CI or not. "Absent" is a
  // routine local gap worth skipping over; "present but older than the code"
  // is a trap that reports the old daemon's behaviour as this checkout's, and
  // skipping it locally is how it reaches CI wearing someone else's failure.
  if (present) {
    assertChiefdBinaryCurrent(binaryPath, root)
    assertChiefdBinaryCurrent(resolveChiefdDaemonBinaryPath(root), root)
  }
  return { present, binaryPath }
}

/** The actionable, visible skip title for a `describe.skip(...)` gated on
 * `chiefdBinaryTestGate`'s `present: false` result — named to be found by
 * a "SKIPPING" grep across reporters, mirroring the Rust e2e harness's own
 * `[chiefd-e2e] SKIPPING "..."` banner (`apps/chiefd/tests/e2e/src/lib.rs`),
 * carried in the describe block's own title rather than a console line so
 * it survives in every vitest reporter (list, verbose, json, junit) without
 * a separate `lucy/no-console-usage` exemption. */
export function chiefdBinarySkipTitle(suiteLabel: string, gate: ChiefdBinaryTestGate): string {
  return (
    `SKIPPING "${suiteLabel}": chief debug test binary not found at ${gate.binaryPath}. ` +
    `Build it locally with: ${chiefdBuildCommand()}`
  )
}
