/**
 * Surface a daemon's own output when a test FAILS, before teardown discards it.
 *
 * There are two ways a harness here loses that output, and both produced the
 * same symptom — `corrupt store: activity` seen seven times across three hosts
 * with a cause zero times (#1031):
 *
 *     log inside a temp data root, and `stop()` removes that root.
 *  2. a fixture may capture them into an in-memory ring buffer
 *     that only `waitForBoundUrl` ever reads, for BOOT diagnostics.
 *
 * In both, the only reader runs when READINESS fails. A failure DURING a test
 * discards the daemon's account of what went wrong — including the
 * `[activity] store error: ...` line that names the broken invariant, which is
 * the one thing that makes such a failure explicable.
 *
 * `afterEach` hooks run before `afterAll`, so a hook registered here always
 * reads while the output still exists.
 */
import { readFile } from 'node:fs/promises'

import { afterEach } from 'vitest'

import { isNullish } from '@/Nullish'

/** Enough to carry a panic plus the requests around it, short enough to read. */
const FAILURE_LOG_TAIL_LINES = 80

/** The last `lines` lines of `text`, or a stated reason there are none. */
export function tailLines(text: string, lines: number): string {
  const tail = text.split('\n').slice(-lines).join('\n')
  return tail.trim().length === 0 ? '(the daemon log is empty)' : tail
}

/** The last `lines` lines of `logPath`, or a stated reason there are none. */
export async function readDaemonLogTail(logPath: string, lines: number): Promise<string> {
  try {
    return tailLines(await readFile(logPath, 'utf8'), lines)
  } catch {
    return `(no daemon log at ${logPath})`
  }
}

/**
 * The banner, kept separate from the IO so the shape is directly assertable.
 * It names the source on purpose: when the tail is not enough, the next question
 * is always "where is the rest", and after teardown the answer is "gone" — so
 * the source has to appear while it still means something.
 */
export function formatDaemonLogTail(testName: string, source: string, tail: string): string {
  return (
    `\n----- chiefd daemon log tail for FAILED test: ${testName} -----\n` +
    `----- source: ${source} (discarded when this suite tears down) -----\n` +
    `${tail}\n` +
    `----- end chiefd daemon log tail -----\n`
  )
}

/**
 * Register an `afterEach` that prints a daemon's log tail whenever the test that
 * just ran failed, for a harness that writes its output to a FILE.
 *
 * `resolve` is a callback rather than the daemon itself because suites boot
 * theirs in `beforeAll`, so there is nothing to pass at registration time.
 * Returning `undefined` (a boot that failed, or has not happened) is normal and
 * prints nothing.
 */
export function surfaceDaemonLogOnFailure(
  resolve: () => { readonly logPath: string } | undefined
): void {
  afterEach(async (context) => {
    if (context.task.result?.state !== 'fail') return
    const daemon = resolve()
    if (isNullish(daemon)) return
    const tail = await readDaemonLogTail(daemon.logPath, FAILURE_LOG_TAIL_LINES)
    process.stderr.write(formatDaemonLogTail(context.task.name, daemon.logPath, tail))
  })
}

/**
 * The same, for a harness that captures its output IN MEMORY rather than to a
 * file — an in-memory ring buffer being the case this exists
 * for. Its output has no path to name, so the caller states its own source
 * label.
 */
export function surfaceDaemonOutputOnFailure(
  resolve: () => { readonly source: string; readonly text: string } | undefined
): void {
  afterEach((context) => {
    if (context.task.result?.state !== 'fail') return
    const captured = resolve()
    if (isNullish(captured)) return
    process.stderr.write(
      formatDaemonLogTail(
        context.task.name,
        captured.source,
        tailLines(captured.text, FAILURE_LOG_TAIL_LINES)
      )
    )
  })
}
