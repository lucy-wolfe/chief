/**
 * #149/#10 — the extension read CALLERS send their cached row seq as
 * `ifSeqNot`, so an UNCHANGED live document is a cheap server-side
 * seq probe instead of a full re-serialize.
 *
 * Each case drives the real extension read helper (the async, in-process
 * `fetch`-based twin of the deleted `spawnSync("curl")` transport) against a
 * stub chiefd that honors the #149 wire contract
 * AND counts every FULL-BLOB response it ships. The gate: a repeat read at an
 * unchanged seq returns the same value while the server's serialize
 * count stays flat. Before this change the callers sent NO `ifGenerationNot`,
 * so the stub would serialize on every read — the RED the count assertion
 * pins.
 *
 * The stub runs OUT OF PROCESS (as chiefd does), each `describe` block gets
 * its OWN stub server and its OWN `beforeEach`/`afterEach` pair (#911) —
 * NOT a single shared server/`afterEach` and NOT a manual `try/finally`
 * inside the test body. `ORG_CHIEFD_URL` is a PROCESS-WIDE global, and a
 * vitest per-test timeout does NOT cancel the test's async body — it only
 * stops AWAITING it. The abandoned body (its own remaining `await`s, and any
 * `finally` attached to them) keeps running on the event loop and can fire
 * arbitrarily late, interleaved with whichever block vitest has since moved
 * on to.
 *
 * Native `beforeEach`/`afterEach` hooks close ONE half of that: vitest runs a
 * timed-out test's `afterEach` promptly (not only once the abandoned body
 * eventually settles), and always before the next block's `beforeEach` — so
 * env restoration itself can no longer land at an unpredictable later time.
 * They do NOT close the other half: the team-ui extension still
 * re-reads `process.env.ORG_CHIEFD_URL` fresh on every call (by design — see
 * that function's own comment), so an abandoned body's NEXT read call — the one
 * after whichever read actually timed out — targets whatever URL is CURRENTLY
 * stubbed, which may by then be a LATER block's own live stub server. That is
 * a genuine second call over the network, not a restored env value, so hooks
 * alone cannot prevent it. Reproduced directly before this fix, with a
 * synthetic response delay that let the FIRST block's response arrive late
 * but successfully: its dangling body proceeded to a real SECOND read against
 * the SECOND block's already-stubbed server, incrementing that server's
 * serialize counter mid-test and turning the second block's own unrelated
 * assertion into `expected 2 to be 1` — the exact failure this issue reports
 * — even with `beforeEach`/`afterEach` in place.
 *
 * `checkActive()` below closes that remaining path structurally: every block
 * captures a monotonically increasing generation token when its `beforeEach`
 * starts, and every read call after the first in a test body is guarded by a
 * synchronous check that a NEWER block has not since started. Since JS is
 * single-threaded, that comparison cannot itself race — an abandoned body
 * that resumes after any await sees the ACTIVE generation exactly as it
 * stands at that instant and, if superseded, throws before issuing the next
 * network call rather than reaching it. This does not depend on timing (kill
 * signals, response latency, scheduler jitter) the way the hooks-only fix
 * did — a stale generation can never again touch shared process state,
 * regardless of how late it resumes.
 */
import { type ChildProcess, spawn } from 'node:child_process'

import { afterEach, beforeEach, describe, expect, test } from 'vitest'

/**
 * Monotonic token identifying whichever `describe` block's `beforeEach` most
 * recently ran. #911: a test body abandoned by a vitest timeout keeps
 * running after control has moved to the next block; every such body must be
 * able to tell, at each step, whether it is still the current one before
 * touching the shared `ORG_CHIEFD_URL` global or issuing another network
 * call against a stub server it does not own.
 */
let activeGeneration = 0

/**
 * Build a per-block guard: captures the generation active when this block's
 * `beforeEach` ran, and returns a function that throws the instant a LATER
 * block has since started. Call it between every pair of network calls in a
 * test body — an abandoned body that resumes after its timeout must stop
 * before its next call, not merely restore state after the fact.
 */
function newGenerationGuard(): () => void {
  const myGeneration = ++activeGeneration
  return () => {
    if (myGeneration !== activeGeneration) {
      throw new Error(
        '#911: this block was abandoned by a vitest timeout and superseded by a later block; ' +
          'refusing to make another call against the shared ORG_CHIEFD_URL global on its behalf.'
      )
    }
  }
}

describe('#911 — newGenerationGuard', () => {
  test('a guard stays callable with no error while it remains the active generation', () => {
    const checkActive = newGenerationGuard()
    expect(() => checkActive()).not.toThrow()
    expect(() => checkActive()).not.toThrow() // idempotent: checking twice is fine
  })

  test('a guard throws the instant a NEWER guard has been created, and stays broken', () => {
    const older = newGenerationGuard()
    expect(() => older()).not.toThrow()

    const newer = newGenerationGuard()
    expect(() => older()).toThrow(/superseded by a later block/)
    expect(() => newer()).not.toThrow() // the newer generation is unaffected

    newGenerationGuard() // a third generation supersedes the second too
    expect(() => newer()).toThrow(/superseded by a later block/)
  })
})

/**
 * A stub chiefd holding ONE typed row document. Its normalized read routes
 * implement the #149 contract: an `ifSeqNot` equal to the live seq is answered
 * `{found:true, unchanged:true, seq}` with no aggregate; anything else ships
 * the named aggregate and increments `serializes`. `/__bump` advances the seq
 * (a foreign commit); `/__count` reports the serialize count.
 */
async function stubChiefd(): Promise<{ port: number; proc: ChildProcess }> {
  // Bind an EPHEMERAL port (`port: 0`) and report the OS-assigned port on
  // stdout AFTER the listener is up. The pre-#59 form picked a RANDOM unchecked
  // port (39000 + random*20000) and ran the stub in a subprocess with stderr
  // ignored, so a port collision made the server throw EADDRINUSE, die
  // silently, and `/ready` "never came up" — an intermittent CI flake. Port 0
  // eliminates the collision at the root (the OS never assigns a bound port).
  // The stub is a plain `node:http` server — vitest runs under Node, so the
  // Bun-only runtime APIs the legacy stub used are unavailable here (see
  // packages/testing's README); listening alone keeps the process alive.
  const handler = `
    const http = require("node:http");
    let seqCursor = 1;
    let doc = JSON.stringify({ schemaVersion: 1, marker: "v1" });
    let serializes = 0;
    const server = http.createServer((req, res) => {
      const u = new URL(req.url, "http://127.0.0.1");
      if (u.pathname === "/ready") { res.end("ok"); return; }
      if (u.pathname === "/__count") {
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify({ serializes }));
        return;
      }
      if (u.pathname === "/__bump") {
        seqCursor += 1;
        doc = JSON.stringify({ schemaVersion: 1, marker: "v" + seqCursor });
        res.end("ok");
        return;
      }
      if (u.pathname === "/v1/org/supervision/read") {
        let raw = "";
        req.on("data", (chunk) => { raw += chunk; });
        req.on("end", () => {
          const b = JSON.parse(raw);
          res.setHeader("content-type", "application/json");
          if (b.ifSeqNot === seqCursor) {
            res.end(JSON.stringify({ found: true, unchanged: true, seq: seqCursor }));
            return;
          }
          serializes += 1;
          res.end(JSON.stringify({ found: true, ledger: doc, seq: seqCursor }));
        });
        return;
      }
      res.statusCode = 404;
      res.end("no");
    });
    server.listen(0, "127.0.0.1", () => {
      console.log(server.address().port);
    });
  `
  const proc = spawn(process.execPath, ['-e', handler], { stdio: ['ignore', 'pipe', 'pipe'] })
  const port = await readStubPort(proc, 'stub chiefd')
  return { port, proc }
}

// Read the OS-assigned port a `port: 0` stub prints after binding. Surfaces the
// subprocess stderr if it dies before reporting a port, so a real boot failure
// is diagnosable instead of a silent "never came up".
function readStubPort(proc: ChildProcess, label: string): Promise<number> {
  const stdout = proc.stdout
  const stderr = proc.stderr
  if (!stdout || !stderr) {
    proc.kill()
    return Promise.reject(new Error(`${label} has no captured stdout/stderr`))
  }
  return new Promise((resolve, reject) => {
    let output = ''
    let errorOutput = ''
    const cleanup = (): void => {
      stdout.off('data', onStdout)
      stderr.off('data', onStderr)
      proc.off('error', onError)
      proc.off('exit', onExit)
    }
    const fail = (): void => {
      cleanup()
      proc.kill()
      reject(
        new Error(
          `${label} never reported a port (stderr: ${errorOutput.trim().slice(0, 800) || '<none>'})`
        )
      )
    }
    const onStdout = (chunk: Buffer): void => {
      output += chunk.toString()
      const newline = output.indexOf('\n')
      if (newline < 0) return
      const port = Number.parseInt(output.slice(0, newline).trim(), 10)
      if (!Number.isInteger(port) || port <= 0) {
        fail()
        return
      }
      cleanup()
      resolve(port)
    }
    const onStderr = (chunk: Buffer): void => {
      errorOutput += chunk.toString()
    }
    const onError = (): void => fail()
    const onExit = (): void => fail()
    stdout.on('data', onStdout)
    stderr.on('data', onStderr)
    proc.once('error', onError)
    proc.once('exit', onExit)
  })
}

async function ready(port: number): Promise<void> {
  for (let i = 0; i < 100; i++) {
    try {
      await fetch(`http://127.0.0.1:${port}/ready`)
      return
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
  }
  throw new Error(`stub chiefd on ${port} never came up`)
}
async function serializeCount(port: number): Promise<number> {
  const response = await fetch(`http://127.0.0.1:${port}/__count`)
  const payload: unknown = await response.json()
  if (!(payload instanceof Object)) throw new Error('stub chiefd count response must be an object')
  const serializes = Reflect.get(payload, 'serializes')
  if (typeof serializes !== 'number')
    throw new Error('stub chiefd count response must contain a number')
  return serializes
}
async function bump(port: number): Promise<void> {
  await fetch(`http://127.0.0.1:${port}/__bump`)
}

describe('#149/#10 — organization-intercom.ts typed live reads send ifSeqNot', () => {
  let port: number
  let proc: ChildProcess
  let checkActive: () => void

  beforeEach(async () => {
    checkActive = newGenerationGuard()
    ;({ port, proc } = await stubChiefd())
    await ready(port)
  })

  afterEach(() => {
    proc.kill()
  })

  /** The runtime context this block's reads travel on — its own daemon, named
   *  explicitly. No `ORG_CHIEFD_URL` is stubbed here at all: this extension
   *  carries its address on the context, which is also why the block needs no
   *  env restoration to keep out of a sibling block's stub. */
  function contextFor(stubPort: number): {
    organizationDir: string
    identityDir: string
    organization: string
    personId: string
    launcherRoot: string
    chiefdUrl: string
    companyKey: string
  } {
    return {
      organizationDir: '/work/acme',
      identityDir: '/work/acme/.chief',
      organization: 'acme',
      personId: 'ceo',
      launcherRoot: '/launcher',
      chiefdUrl: `http://127.0.0.1:${stubPort}`,
      // The key its own daemon published. Distinct per stub port below, so a
      // read that reached the wrong daemon would also be carrying the wrong
      // company.
      companyKey: `00000000${String(stubPort).slice(-4)}`
    }
  }

  test('a repeat read at an unchanged seq reuses the value and does NOT re-serialize', async () => {
    const { readDurableDocumentCached, resetConditionalReadCacheForTest } =
      await import('../extensions/organization-intercom')
    resetConditionalReadCacheForTest()
    const context = contextFor(port)
    const first = await readDurableDocumentCached(context, 'supervision')
    expect(first).toEqual({ schemaVersion: 1, marker: 'v1' })
    expect(await serializeCount(port)).toBe(1)

    checkActive() // #911: stop here, not after, if a later block has since started
    const second = await readDurableDocumentCached(context, 'supervision')
    expect(second).toEqual({ schemaVersion: 1, marker: 'v1' })
    expect(await serializeCount(port)).toBe(1) // unchanged → probe only, no re-serialize

    checkActive()
    await bump(port)
    checkActive()
    const third = await readDurableDocumentCached(context, 'supervision')
    expect(third).toEqual({ schemaVersion: 1, marker: 'v2' })
    expect(await serializeCount(port)).toBe(2) // changed → one fresh serialize
    resetConditionalReadCacheForTest()
  })

  test('two contexts naming DIFFERENT daemons each read their own, in one process', async () => {
    // The multi-company property, at the smallest scale that can show it: two
    // live stub daemons, two contexts, one process, interleaved reads. Before
    // the endpoint was threaded there was exactly one address for the whole
    // process, so the second context's read landed on the first one's server —
    // and a wrong daemon ANSWERS, so the only visible evidence would have been
    // the other server's serialize counter moving.
    const { readDurableDocumentCached, resetConditionalReadCacheForTest } =
      await import('../extensions/organization-intercom')
    const other = await stubChiefd()
    try {
      await ready(other.port)
      resetConditionalReadCacheForTest()
      await bump(other.port) // the second daemon holds 'v2'; the first still holds 'v1'

      checkActive()
      expect(await readDurableDocumentCached(contextFor(port), 'supervision')).toEqual({
        schemaVersion: 1,
        marker: 'v1'
      })
      checkActive()
      expect(await readDurableDocumentCached(contextFor(other.port), 'supervision')).toEqual({
        schemaVersion: 1,
        marker: 'v2'
      })

      // Each daemon served exactly its own read. A count of 2 on either side
      // is the defect: both reads went to one address.
      checkActive()
      expect(await serializeCount(port)).toBe(1)
      expect(await serializeCount(other.port)).toBe(1)
      resetConditionalReadCacheForTest()
    } finally {
      other.proc.kill()
    }
  })
})

describe('#149/#10 — team-ui.ts typed footer gather sends ifSeqNot', () => {
  let port: number
  let proc: ChildProcess
  let checkActive: () => void

  beforeEach(async () => {
    checkActive = newGenerationGuard()
    ;({ port, proc } = await stubChiefd())
    await ready(port)
  })

  afterEach(() => {
    proc.kill()
  })

  test('an unchanged footer re-read reuses the cached value and does NOT re-serialize', async () => {
    const { readFooterStoreDocument, resetFooterStoreDocumentCache } =
      await import('../extensions/team-ui')
    resetFooterStoreDocumentCache()
    // #983: the daemon address is an ARGUMENT now, not `ORG_CHIEFD_URL`. The
    // subject of this test is unchanged — it is the conditional-read probe —
    // but the address has to travel with the call, because a footer serving two
    // companies in one process has no process-global address that is right for
    // both. The company key travels with it for the same reason.
    const chiefdUrl = `http://127.0.0.1:${port}`
    const companyKey = '0123456789ab'
    // `changedStores` undefined → the footer always round-trips (never the
    // SSE-skip branch), so the seq probe is what suppresses the reload.
    const first = await readFooterStoreDocument(chiefdUrl, companyKey, 'supervision', undefined)
    expect(first).toEqual({ schemaVersion: 1, marker: 'v1' })
    expect(await serializeCount(port)).toBe(1)

    checkActive() // #911: stop here, not after, if a later block has since started
    const second = await readFooterStoreDocument(chiefdUrl, companyKey, 'supervision', undefined)
    expect(second).toEqual({ schemaVersion: 1, marker: 'v1' })
    expect(await serializeCount(port)).toBe(1) // unchanged → probe only

    checkActive()
    await bump(port)
    checkActive()
    const third = await readFooterStoreDocument(chiefdUrl, companyKey, 'supervision', undefined)
    expect(third).toEqual({ schemaVersion: 1, marker: 'v2' })
    expect(await serializeCount(port)).toBe(2)
    resetFooterStoreDocumentCache()
  })
})
