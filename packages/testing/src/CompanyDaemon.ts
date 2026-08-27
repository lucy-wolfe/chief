/**
 * Boots one real `chiefd run --dir <dir> --serve-only` per call — the
 *
 * # Why this exists
 *
 * `chiefd docstore-only` serves the document routes and nothing else, so a
 * company-scoped write answers `404 unknown_company` there. Every contract
 * test for those verbs was therefore unwritable against that surface.
 *
 * `--serve-only` is the right mode for a test: it mounts the whole route
 * surface but does NOT actuate tmux, so a contract test cannot spawn panes on
 * the machine running it. `chiefd run`'s own usage calls it "the
 * non-actuating snapshot-reader mode".
 *
 * # The company must exist before its routes answer
 *
 * A daemon serves a company; it does not invent one. Callers seed a manifest
 * through `POST /v1/org/manifest/genesis` — the one route that may
 * create an absent company — using [`CompanyDaemon.companyKey`] as the `slug`.
 * `seedCompany()` below does exactly that for the common case.
 *
 * # The company is a DIRECTORY
 *
 * `--dir` is the daemon's whole configuration: the store is
 * `<dir>/.chief/db/chief.db`, the keys are `<dir>/.chief/keys`, and the wire
 * identity is `sha256(<dir>)[..12]`. There is no `--company`, no
 * `--data-root`, and no orgs root — one slug under two data roots was two
 * companies, and a directory needs no composite to tell them apart.
 *
 * # It authenticates (A7)
 *
 * `--serve-only` used to refuse to start under an enforced universal auth gate,
 * which made this harness the one caller class that could not be given a
 * credential — and therefore the reason the gate could not be turned on. The
 * mode now builds the same auth runtime `chiefd run` builds, mints
 * `<dir>/.chief/keys/operator.key` at boot like every other daemon, and this
 * harness reads that key and presents a bearer on every call it makes. Callers
 * get the same through [`CompanyDaemon.authorizedFetch`]; a raw `fetch` at this
 * daemon is deliberately still possible, because proving a route REFUSES an
 * anonymous caller is half of what the harness exists for.
 */
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createWriteStream } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

import { assertChiefdBinaryBuilt, resolvePinnedPiBinaryPath } from '@/ChiefdBinary'
import { allocateEphemeralPort } from '@/EphemeralPort'
import { isNullish } from '@/Nullish'
import { acquireOperatorBearer, createOperatorFetch } from '@/OperatorBearer'
import { createTempDir } from '@/TempDir'
import type { CompanyDaemon, CompanyDaemonOptions } from '@/types/CompanyDaemon'

/** `<dir>/.chief` — everything chief owns inside a company directory. The Rust
 * twin is `company_dir::chief_dir`. */
function chiefFolder(dir: string): string {
  return join(dir, '.chief')
}

const DEFAULT_READY_TIMEOUT_MS = 20_000
const READY_POLL_INTERVAL_MS = 100
const STOP_GRACE_MS = 2_000
const LOG_TAIL_LINES = 40

/**
 * `sha256(<dir>)[..12]` — chiefd's company key, twelve lowercase hex.
 *
 * A HARNESS is the one place this may legitimately be derived rather than
 * read. In production the key has exactly one producer and is SERVED
 * (beacond's `CompanyRow.key`, the daemon rendezvous's `key`); here there is no
 * registry and no rendezvous to read it from, because `--serve-only` is a
 * daemon booted directly with no beacond on the box. So the harness mints the
 * same value the daemon will, from the one input both are given.
 *
 * Deliberately computed here rather than imported from `@chief/chiefing`: this
 * package must not depend on the client it exists to help test, or a contract
 * test could pass because both sides share one wrong implementation. It is
 * pinned against the Rust `company_dir::company_key` by
 * `TmuxHostedCompanyDaemon.test.ts`'s cross-language literal.
 */
function companyKeyFor(dir: string): string {
  return createHash('sha256').update(dir).digest('hex').slice(0, 12)
}

async function readLogTail(logPath: string): Promise<string> {
  try {
    const content = await readFile(logPath, 'utf8')
    return content.split('\n').slice(-LOG_TAIL_LINES).join('\n')
  } catch {
    return '(no log available)'
  }
}

async function isReachable(url: string): Promise<boolean> {
  try {
    // `/v1/docs/runtime`, NOT `/v1/health`. The docstore-only surface serves a
    // health route; this one does not, and probing it 404s forever — the
    // daemon comes up perfectly and the harness times out saying it never
    // did. This is a route the company surface actually mounts.
    const response = await fetch(`${url}/v1/docs/runtime`, { signal: AbortSignal.timeout(1_000) })
    // A `401` is READY (A7). What is measured here is that the listener
    // answers HTTP at all, and this probe necessarily runs BEFORE any bearer
    // exists — the key it would sign with is minted by the very boot being
    // waited on. So an authenticated probe is impossible and an anonymous
    // refusal must count as up, or the harness would time out on exactly the
    // posture it was changed to support.
    //
    // This route happens to answer `200` anonymously TODAY even under an
    // enforced gate, because `docstore/mod.rs` adds it after the verify layer
    // is applied — a real gap, owned by A6 and pinned by
    // `CompanyDaemonAuth.test.ts`. This line is written for the fixed world,
    // not the current one, so closing that gap does not break the harness.
    return response.ok || response.status === 401
  } catch {
    return false
  }
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** Boot one company daemon. Always paired with `stop()` in an `afterAll`. */
export async function startCompanyDaemon(options: CompanyDaemonOptions): Promise<CompanyDaemon> {
  const repoRoot = options.repoRoot ?? join(import.meta.dirname, '..', '..', '..')
  const binaryPath = assertChiefdBinaryBuilt(repoRoot)
  const readyTimeoutMs = options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS

  // The temp tree IS the company directory — a company is just a directory the
  // operator ran `chief` in, and a disposable one is exactly what a test wants.
  // Everything the daemon owns lands under `<dir>/.chief/`, which dies with it,
  // including the `operator.key` it mints. That key used to land in the OS temp
  // directory itself: one shared file for every suite on the box, outliving
  // all of them.
  const temp = await createTempDir(options.dirPrefix ?? 'chief-company-test-')
  const dir = temp.path
  const port = await allocateEphemeralPort()
  const url = `http://127.0.0.1:${port}`
  const logPath = join(dir, 'chiefd.log')
  const logStream = createWriteStream(logPath, { flags: 'a' })

  const child = spawn(
    binaryPath,
    [
      'run',
      '--dir',
      dir,
      // Required even here. `chiefd run` used to default this to the bare
      // name `pi`, which nothing set, and a serve-only reader that is exempt
      // from a rule is a second answer to what the rule is. Pinned to the
      // checkout; this mode actuates nobody, so it is never executed.
      '--pi-binary',
      resolvePinnedPiBinaryPath(repoRoot),
      // Required even here, and for the same reason `--pi-binary` is: serve-only
      // is not exempt from a rule the actuating path obeys. It never reads
      // `resources/` (it actuates nobody), but `chiefd run` resolves a launcher
      // root for every mode — no `~/.chief/launcher-root` pointer and no
      // silent default any more — so a binary run straight out of a build
      // directory is pointed at the checkout, which is where its resources sit.
      '--launcher-root',
      repoRoot,
      '--serve-only'
    ],
    {
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        // An explicit bind, on an ephemeral port this harness allocated.
        // Without it `chiefd run` falls back to the conventional company port
        // (8792) and refuses to start whenever a real company already holds it
        // — which on any machine running the product is always.
        CHIEFD_STORE_BIND: `127.0.0.1:${port}`,
        // Same leak safety as the docstore harness: the daemon watches THIS
        // process and self-exits when it is gone, however many forks removed.
        CHIEFD_STORE_EXIT_WITH_PARENT: '1',
        CHIEFD_STORE_WATCH_PID: `${process.pid}`,
        ...options.env
      }
    }
  )
  child.unref()
  child.stdout?.pipe(logStream)
  child.stderr?.pipe(logStream)

  try {
    await waitUntilReadyOrExit(child, url, readyTimeoutMs, logPath)
  } catch (error) {
    await killChild(child)
    await temp.remove()
    throw error
  }

  let stopped = false
  const stop = async (): Promise<void> => {
    if (stopped) return
    stopped = true
    await killChild(child)
    await temp.remove()
  }

  // Acquired eagerly, and here rather than at the first call: a daemon that
  // came up but cannot mint a bearer must fail in `startCompanyDaemon` with
  // the log tail attached, not later inside whichever route a suite happened
  // to call first.
  let bearer: string
  try {
    bearer = await acquireOperatorBearer({ url, keysRoot: chiefFolder(dir) })
  } catch (error) {
    // The tail is read BEFORE teardown: `stop()` removes the temp tree the log
    // lives in, so reversing these two lines produces `(no log available)` on
    // every failure this branch exists to explain.
    const tail = await readLogTail(logPath)
    await stop()
    throw new Error(`${String(error)}\n\nLog tail:\n${tail}`, { cause: error })
  }

  return {
    url,
    port,
    slug: options.slug,
    companyKey: companyKeyFor(dir),
    dir,
    bearer,
    authorizedFetch: createOperatorFetch({ url, keysRoot: chiefFolder(dir) }),
    logPath,
    pid: child.pid ?? -1,
    stop
  }
}

/**
 * Seed the daemon's company so its routes have something to resolve against.
 *
 * The wire carries a SPEC — a name, a purpose and a CEO seed — not a manifest:
 * chiefd normalizes it and decides every id, tool grant and unit relationship
 * itself. A harness that posted a manifest would be seeding a company shape
 * production never builds. It carries no route either, because a company has
 * none: every agent boots as plain Pi on the operator's own defaults.
 */
export async function seedCompany(
  daemon: CompanyDaemon,
  options: { name?: string; purpose?: string } = {}
): Promise<void> {
  const name = options.name ?? daemon.slug
  /* eslint-disable lucy/no-json-stringify */
  // Same reasoning as `FetchTransport`'s own disable: `toJsonTreeString` is
  // not a dependency of this package, and this is the one seam that
  // serializes an outgoing body here.
  // Through the daemon's own authorized fetch: genesis is a `/v1/org/*` route
  // and therefore not exempt, so under an enforced gate an anonymous seed is a
  // `401` — the failure that made this harness the caller class blocking A6.
  const response = await daemon.authorizedFetch('/v1/org/manifest/genesis', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      slug: daemon.companyKey,
      spec: {
        name,
        purpose: options.purpose ?? `${name} exists to be tested.`,
        chief: { name: 'Chief' }
      },
      at: new Date().toISOString()
    })
  })
  /* eslint-enable lucy/no-json-stringify */
  if (!response.ok) {
    const body = await response.text()
    throw new Error(
      `seedCompany(${daemon.slug}) failed: ${response.status} ${body.slice(0, 400)}\n\n` +
        `Log tail:\n${await readLogTail(daemon.logPath)}`
    )
  }
}

/**
 * Races two bounded, event-driven conditions: the child exiting (fail fast)
 * and reachability within `readyTimeoutMs` (a bounded, awaited poll loop —
 * never a blocking wait, never a fixed-cadence interval timer).
 */
async function waitUntilReadyOrExit(
  child: ChildProcess,
  url: string,
  readyTimeoutMs: number,
  logPath: string
): Promise<void> {
  let exited = false
  const exitPromise = new Promise<never>((_resolve, reject) => {
    child.once('exit', (code, signal) => {
      exited = true
      reject(
        new Error(
          `chiefd run --serve-only exited before becoming reachable ` +
            `(code=${String(code)}, signal=${String(signal)})`
        )
      )
    })
  })

  const deadline = Date.now() + readyTimeoutMs
  const pollLoop = (async (): Promise<void> => {
    for (;;) {
      if (exited) return
      if (await isReachable(url)) return
      if (Date.now() >= deadline) {
        throw new Error(
          `chiefd run --serve-only did not become reachable within ${readyTimeoutMs}ms.\n\n` +
            `Log tail:\n${await readLogTail(logPath)}`
        )
      }
      await delay(READY_POLL_INTERVAL_MS)
    }
  })()

  await Promise.race([exitPromise, pollLoop])
}

async function killChild(child: ChildProcess): Promise<void> {
  const pid = child.pid
  if (typeof pid !== 'number') return
  // `!isNullish` deliberately, not a truthy check: exitCode 0 is a clean exit
  // — falsy, but very much "already gone".
  if (!isNullish(child.exitCode) || !isNullish(child.signalCode)) return

  await new Promise<void>((resolve) => {
    let settled = false
    const finish = (): void => {
      if (settled) return
      settled = true
      resolve()
    }
    child.once('exit', finish)

    try {
      // Negative pid targets the process group — `detached: true` makes this
      // child its own group leader, so this reaches it even if it forked.
      process.kill(-pid, 'SIGTERM')
    } catch {
      try {
        child.kill('SIGTERM')
      } catch {
        finish()
        return
      }
    }

    setTimeout(() => {
      if (settled) return
      try {
        process.kill(-pid, 'SIGKILL')
      } catch {
        try {
          child.kill('SIGKILL')
        } catch {
          // Already gone; the exit listener above settles this promise.
        }
      }
    }, STOP_GRACE_MS)
  })
}
