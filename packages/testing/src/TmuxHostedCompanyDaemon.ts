/**
 * Boots one real, FULLY ACTUATING `chiefd run --dir <dir>` per call — the only
 * harness in the repo that mounts chiefd's tmux host capability.
 *
 * # Why `--serve-only` is not enough
 *
 * `CompanyDaemon` boots `chiefd run --serve-only`, which is deliberately
 * non-actuating: it leaves `host_executor` and `reconcile_actuator_config`
 * unset. Every route in the runtime family reads them through one helper
 * (`runtime_routes.rs`'s `host()`), which answers
 *
 *     503 "this chiefd has no tmux host capability"
 *
 * when either is absent. `/v1/org/runtime/launch` is therefore UNREACHABLE in
 * `--serve-only`, and that route is the one every org tool calls after its
 * durable write commits (`reconcileRuntime`). A proof built on `--serve-only`
 * cannot reach the code under test at all — which is exactly how #751/P4's
 * reconcile defect survived three packets that each proved their route
 * returned 200.
 *
 * # The boot chain is three steps, and none of them is optional
 *
 * A full `chiefd run` takes single-writer admission from beacond before it
 * opens any company storage, and beacond refuses to admit a DIRECTORY it has
 * no row for ("a daemon cannot create one by binding"). So:
 *
 *  1. spawn a test-owned `beacond` on its own port and its own registry file;
 *  2. `POST /v1/company/create {dir, key, slug}` so the directory has a row;
 *  3. spawn `chiefd run --dir`, pointed at that beacond through `BEACOND_URL`.
 *
 * The daemon then port-walks, registers its bound address, and serves. It
 * does NOT genesis a company — a caller that needs `/v1/org/*` routes to
 * resolve must post `/v1/org/manifest/genesis` itself with
 * [`TmuxHostedCompany.companyKey`] as the `slug`.
 *
 * # Isolation
 *
 * Every run gets its own company directory, its own beacond registry, its own
 * ports, and its own **private tmux socket** (`-L <socket>`), torn down with
 * `tmux kill-server` in `stop()`. A test therefore never touches the
 * operator's tmux server, and two runs on one machine never collide.
 */
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { createHash, randomBytes } from 'node:crypto'
import { createWriteStream } from 'node:fs'
import { mkdir, readFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'

import {
  assertChiefdBinaryBuilt,
  resolveChiefBinaryPath,
  resolvePinnedPiBinaryPath
} from '@/ChiefdBinary'
import { allocateEphemeralPort } from '@/EphemeralPort'
import { isNullish } from '@/Nullish'
import { createTempDir } from '@/TempDir'
import type {
  ChiefdRunArgvOptions,
  TmuxHostedCompany,
  TmuxHostedCompanyOptions
} from '@/types/TmuxHostedCompany'

const DEFAULT_READY_TIMEOUT_MS = 30_000
const READY_POLL_INTERVAL_MS = 100
const STOP_GRACE_MS = 2_000
const LOG_TAIL_LINES = 40

/**
 * `sha256(<dir>)[..12]` — chiefd's company key, twelve lowercase hex.
 *
 * A HARNESS mints it; production READS it. This one has a real beacond, so it
 * is also the value it POSTS to `/v1/company/create` — beacond records the
 * key its caller minted and never derives one, so the harness standing in for
 * `chief` must mint it exactly as `chief` would.
 *
 * Recomputed here rather than imported from `@chief/chiefing` for the same
 * reason `CompanyDaemon` does it: this package must not depend on the client
 * it exists to help test, or a contract test could pass because both sides
 * share one wrong implementation.
 */
function companyKeyFor(dir: string): string {
  return createHash('sha256').update(dir).digest('hex').slice(0, 12)
}

/**
 * The `beacond` binary sits beside `chiefd` in the same debug directory and
 * is produced by the same debug Cargo build (and by CI's
 * `chiefd-ci-binary` artifact, which carries both).
 */
function resolveBeacondBinaryPath(repoRoot: string): string {
  return join(dirname(resolveChiefBinaryPath(repoRoot)), 'beacond')
}

async function readLogTail(logPath: string): Promise<string> {
  try {
    const content = await readFile(logPath, 'utf8')
    return content.split('\n').slice(-LOG_TAIL_LINES).join('\n')
  } catch {
    return '(no log available)'
  }
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

async function probe(url: string): Promise<boolean> {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(1_000) })
    return response.ok
  } catch {
    return false
  }
}

/**
 * Races the child exiting (fail fast, naming its log) against a bounded,
 * awaited reachability poll. Never a blocking wait and never a
 * fixed-cadence interval timer.
 */
async function waitUntilReadyOrExit(
  child: ChildProcess,
  label: string,
  probeUrl: string,
  readyTimeoutMs: number,
  logPath: string
): Promise<void> {
  let exited = false
  const exitPromise = new Promise<never>((_resolve, reject) => {
    child.once('exit', (code, signal) => {
      exited = true
      void readLogTail(logPath).then((tail) => {
        reject(
          new Error(
            `${label} exited before becoming reachable ` +
              `(code=${String(code)}, signal=${String(signal)}).\n\nLog tail:\n${tail}`
          )
        )
      })
    })
  })

  const deadline = Date.now() + readyTimeoutMs
  const pollLoop = (async (): Promise<void> => {
    for (;;) {
      if (exited) return
      if (await probe(probeUrl)) return
      if (Date.now() >= deadline) {
        throw new Error(
          `${label} did not become reachable within ${readyTimeoutMs}ms.\n\n` +
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

/**
 * Tear down the private tmux server this daemon actuated on, if it ever
 * started one. Best-effort by design: `kill-server` on a socket that was
 * never created exits nonzero, and that is the ordinary case for a test that
 * never reached a converge pass.
 */
async function killTmuxServer(socket: string): Promise<void> {
  await new Promise<void>((resolve) => {
    const child = spawn('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' })
    child.once('error', () => resolve())
    child.once('exit', () => resolve())
  })
}

/**
 * The exact `chiefd run` argv this harness spawns.
 *
 * Split out of the spawn call so the argument list is assertable without
 * booting a daemon: a rule about what must be on the command line is only
 * enforceable if a test can read the command line.
 *
 * # `--launcher-root` is not optional, and it is not a default
 *
 * Omit it and chiefd resolves its resource root from the `resources/`
 * directory beside its own binary — which for a freshly built `target/release`
 * binary does not exist, so the daemon REFUSES to start and says so. That is
 * the good case; the bad one is the reason this paragraph is long.
 *
 * It used to fall back to `~/.chief/launcher-root`, a record in the OPERATOR's
 * home directory, which on a shared build box is ONE FILE for every agent on
 * it. Observed 2026-08-09: a test company booted by this harness recorded
 * `org_settings.launcher_root = /root/wt-web`, a different agent's worktree,
 * and inventoried Pi resources out of a tree nobody in that test had ever
 * read. Nothing failed; the suite was green about the wrong checkout. The
 * pointer is deleted, and the fallback with it — but pinning the flag is still
 * what makes this harness read THIS worktree rather than whatever is
 * installed.
 *
 * It has to be pinned HERE, at first boot, and cannot be repaired afterwards.
 * chiefd records the launcher root into `org_settings` on the first completed
 * materialization, and that recorded value then OUTRANKS every daemon-level
 * setting — `~/.chief/launcher-root`, `ORG_LAUNCHER_ROOT` and
 * `--launcher-root` alike (`runtime_lifecycle.rs`'s own refusal message says
 * so). The directory is fresh per boot, so at this moment there is no recorded
 * value and this flag is what gets written.
 */
export function chiefdRunArgv(options: ChiefdRunArgvOptions): string[] {
  return [
    'run',
    '--dir',
    options.dir,
    // #751/P9 renamed this flag: `chiefd-daemon/src/run.rs` parses
    // `--runtime-socket` and reads `ORG_LAUNCHER_RUNTIME_SOCKET`. The old
    // `--tmux-socket` spelling is not accepted by any surviving arm, so a
    // harness still passing it boots the company onto the SLUG-fallback
    // socket instead of the per-test one this harness kills in `stop()`.
    '--runtime-socket',
    options.tmuxSocket,
    '--launcher-root',
    options.repoRoot,
    // What every person's pane execs. `chiefd run` no longer defaults this to
    // the bare name `pi` — it did, nothing ever set it, and the resulting
    // PATH-dependent lookup killed every pane on a host that pinned Pi instead
    // of putting it on PATH. Pinned to the CHECKOUT for the same reason
    // `--launcher-root` above is.
    '--pi-binary',
    resolvePinnedPiBinaryPath(options.repoRoot)
    // NO `--serve-only`: the whole point of this harness is the tmux host
    // capability that flag switches off.
  ]
}

/**
 * Boot one fully-actuating company daemon and its beacond. Always paired with
 * `stop()` in an `afterAll`.
 */
export async function startTmuxHostedCompany(
  options: TmuxHostedCompanyOptions
): Promise<TmuxHostedCompany> {
  const repoRoot = options.repoRoot ?? join(import.meta.dirname, '..', '..', '..')
  const chiefdPath = assertChiefdBinaryBuilt(repoRoot)
  const beacondPath = resolveBeacondBinaryPath(repoRoot)
  const readyTimeoutMs = options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS

  const temp = await createTempDir(options.dirPrefix ?? 'chief-tmux-host-test-')
  // The company IS this directory. `<dir>/.chief` holds the store, the keys,
  // the logs and the disposable run folder; nothing this daemon owns lands
  // anywhere else, so the temp tree's removal is a complete teardown.
  const dir = join(temp.path, 'company')
  await mkdir(dir, { recursive: true })
  const companyKey = companyKeyFor(dir)
  // A private socket name per boot: two suites on one machine must never
  // actuate the same tmux server, and neither may touch the operator's.
  const tmuxSocket = `chief-test-${randomBytes(6).toString('hex')}`

  const beacondLogPath = join(temp.path, 'beacond.log')
  const beacondLog = createWriteStream(beacondLogPath, { flags: 'a' })
  const beacondPort = await allocateEphemeralPort()
  const beacondUrl = `http://127.0.0.1:${beacondPort}`

  const beacondChild = spawn(beacondPath, [], {
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env,
      BEACOND_BIND: `127.0.0.1:${beacondPort}`,
      BEACOND_DB_PATH: join(temp.path, 'beacond.sqlite'),
      // #987/#751: the beacond half of this file used to be the weaker of its
      // two detached spawns — the chiefd half arms
      // CHIEFD_STORE_EXIT_WITH_PARENT and this one armed nothing, so it was
      // reaped only by `stop()`. BEACOND_WATCH_PID is beacond's equivalent:
      // the daemon polls this pid and exits when it is gone.
      BEACOND_WATCH_PID: String(process.pid)
    }
  })
  beacondChild.unref()
  beacondChild.stdout?.pipe(beacondLog)
  beacondChild.stderr?.pipe(beacondLog)

  const logPath = join(temp.path, 'chiefd.log')
  let chiefdChild: ChildProcess | undefined

  let stopped = false
  const stop = async (): Promise<void> => {
    if (stopped) return
    stopped = true
    if (chiefdChild) await killChild(chiefdChild)
    await killChild(beacondChild)
    await killTmuxServer(tmuxSocket)
    await temp.remove()
  }

  try {
    await waitUntilReadyOrExit(
      beacondChild,
      'beacond',
      `${beacondUrl}/v1/health`,
      readyTimeoutMs,
      beacondLogPath
    )

    // Step 2: the company row. `chiefd run` refuses admission without it and
    // exits before opening any storage, so this must precede the spawn.
    /* eslint-disable lucy/no-json-stringify */
    // Same reasoning as `CompanyDaemon.seedCompany`'s own disable: this
    // package does not depend on the client it helps test, so it serializes
    // its one outgoing body here.
    const created = await fetch(`${beacondUrl}/v1/company/create`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ dir, key: companyKey, slug: options.slug })
    })
    /* eslint-enable lucy/no-json-stringify */
    if (!created.ok) {
      throw new Error(
        `beacond refused the company row for '${dir}': ` +
          `${created.status} ${(await created.text()).slice(0, 400)}`
      )
    }

    const port = await allocateEphemeralPort()
    const url = `http://127.0.0.1:${port}`
    const chiefdLog = createWriteStream(logPath, { flags: 'a' })
    chiefdChild = spawn(chiefdPath, chiefdRunArgv({ dir, tmuxSocket, repoRoot }), {
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        // The test-owned beacond, never the conventional 127.0.0.1:6969 a
        // developer box may already be running for a real company.
        BEACOND_URL: beacondUrl,
        CHIEFD_STORE_BIND: `127.0.0.1:${port}`,
        // The daemon watches THIS process and self-exits when it is gone,
        // however many forks removed — a leaked actuating daemon would keep
        // spawning panes long after the suite ended.
        CHIEFD_STORE_EXIT_WITH_PARENT: '1',
        CHIEFD_STORE_WATCH_PID: `${process.pid}`,
        ...options.env
      }
    })
    chiefdChild.unref()
    chiefdChild.stdout?.pipe(chiefdLog)
    chiefdChild.stderr?.pipe(chiefdLog)

    await waitUntilReadyOrExit(
      chiefdChild,
      'chiefd run (tmux-hosted)',
      `${url}/v1/docs/runtime`,
      readyTimeoutMs,
      logPath
    )

    return {
      url,
      port,
      slug: options.slug,
      companyKey,
      dir,
      tmuxSocket,
      beacondUrl,
      logPath,
      beacondLogPath,
      pid: chiefdChild.pid ?? -1,
      stop
    }
  } catch (error) {
    await stop()
    throw error
  }
}
