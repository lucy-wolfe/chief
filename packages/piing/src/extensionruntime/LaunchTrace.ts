/**
 * The Pi-side half of the daemon observability stream.
 *
 * # The gap this closes
 *
 * `apps/chiefd/crates/chiefd-log` made every Rust step of a company launch
 * attributable. It did not, and cannot, see the part of a launch that runs
 * inside a Pi extension. Measured on a live box: a `chiefd_launch_company`
 * that took 143.2 seconds end to end spent 2.643 of them inside chiefd —
 * 1.8% — and the remaining 140.6 seconds passed BEFORE the tool opened its
 * first socket to chiefd, with no line written anywhere by anybody. Proving
 * where that time went needed SSH, `ss` and `/proc`, and still did not answer
 * it.
 *
 * This module gives the extension the same instrument the daemon already has:
 * one JSONL line per event, `enter`/`exit` pairs around anything that can
 * block, and `detail.durationMs` on the line that closes a step.
 *
 * # Why the schema is copied rather than invented
 *
 * The consumer is one reader grepping one directory. A second line shape
 * would mean two parsers, and the streams would stop merging on timestamp —
 * which is the entire reason to write a file at all. So the record here is
 * `chiefd-log`'s record, key for key and in the same order, and
 * `LaunchTrace.test.ts` asserts the key set against the same closed list
 * `chiefd_log::sink::TOP_LEVEL_KEYS` states.
 *
 * The stream is its OWN file (`founder-pi.jsonl`). `chiefd.jsonl` belongs to
 * the Rust operator client; two processes appending to one file would put the
 * per-stream sequence numbers — the only evidence that a line was lost — at
 * the mercy of interleaving.
 *
 * # Why it lives in the extension-runtime closure
 *
 * The founder tools are copied byte-for-byte into a pi-home, where package
 * aliases and `node_modules` do not exist. Node builtins and relative sibling
 * imports are the only specifiers that survive that copy.
 *
 * # Everything here is best-effort
 *
 * A logger that can fail the turn it was only supposed to observe is worse
 * than no logger. Every path that touches the filesystem is wrapped, and a
 * failure is counted rather than raised.
 */

import { appendFileSync, mkdirSync, renameSync, statSync } from 'node:fs'
import { join } from 'node:path'

/** Bumped only on a breaking key change; tracks `chiefd_log`'s `SCHEMA_VERSION`. */
export const CHIEFD_LOG_SCHEMA_VERSION = 1

/**
 * The closed set of top-level keys, mirroring `chiefd_log::sink::TOP_LEVEL_KEYS`.
 * Free-form payload is confined to `detail` so a careless call site cannot
 * break a reader's queries.
 */
export const CHIEFD_LOG_TOP_LEVEL_KEYS = [
  'schemaVersion',
  'at',
  'level',
  'service',
  'event',
  'organization',
  'pid',
  'seq',
  'personId',
  'effectId',
  'assignmentId',
  'messageId',
  'detail'
] as const

/**
 * Organization-agnostic lines still carry the field, spelled explicitly, so
 * that the presence of a real slug stays meaningful. A launch has no company
 * for most of its duration — that is precisely the window being measured.
 */
export const NO_ORGANIZATION = '-'

/**
 * The service name, and therefore the file: `<root>/logs/founder-pi.jsonl`.
 * One directory listing then reads as the list of programs that have run —
 * `chiefd`, `chiefd`, `beacond`, `founder-pi`.
 */
export const FOUNDER_TRACE_SERVICE = 'founder-pi'

/**
 * The directory chief owns, inside a company directory and inside `$HOME`
 * alike. One name for both, because they hold the same kind of thing. Mirrors
 * `chiefd_log::sink::CHIEF_DIR`.
 */
const CHIEF_DIR = '.chief'

/** Per-stream byte cap before rotation, matching `chiefd-log`'s default. */
const DEFAULT_MAX_BYTES = 16 * 1024 * 1024

/** The environment variable naming an explicit per-stream cap. */
const MAX_BYTES_ENV = 'ORG_LOG_MAX_BYTES'

/** What a redacted span is replaced with. */
const MASK = '[redacted]'

/**
 * Key names whose values are always masked, matched case-insensitively as
 * substrings so `OPENROUTER_API_KEY` and `x-api-key` are both covered.
 * Verbatim from `host_primitives::redact`.
 */
const SENSITIVE_KEY_FRAGMENTS = ['token', 'key', 'secret', 'password', 'passwd', 'credential']

/** Literal prefixes that identify a credential regardless of context. */
const SECRET_PREFIXES = ['sk-', 'ghp_', 'github_pat_', 'xoxb-']

export type TraceLevel = 'debug' | 'info' | 'warn' | 'error'

/** Free-form payload. Only `detail` is free-form; the top level is closed. */
export type TraceDetail = Record<string, unknown>

/** The environment shape this module reads, passed in rather than reached for. */
export type TraceEnvironment = Readonly<Record<string, string | undefined>>

/**
 * Redact a diagnostic before it is written.
 *
 * A direct port of `host_primitives::redact` — the same mask both Rust
 * actuators already apply, chosen over a second opinion about what a
 * credential looks like because two opinions is how one of them gets it
 * wrong. Three shapes are masked: `KEY=value` / `KEY: value` where the key
 * names a credential, bare tokens carrying a known credential prefix, and
 * nothing else. Operators debug from these strings, so the surrounding text
 * survives.
 */
export function redactSecrets(input: string): string {
  return rustLines(input).map(redactLine).join('\n')
}

/**
 * Rust's `str::lines()`, reproduced: split on `\n`, drop the trailing empty
 * segment, and strip a `\r` before each break. Reimplementing this rather
 * than using a plain `split` is what keeps the two masks byte-identical on
 * the same input.
 */
function rustLines(input: string): string[] {
  const parts = input.split('\n')
  if (parts.length > 0 && parts[parts.length - 1] === '') parts.pop()
  return parts.map((line) => (line.endsWith('\r') ? line.slice(0, -1) : line))
}

function redactLine(line: string): string {
  let out = ''
  let first = true
  // `AUTH_TOKEN: abc` splits the value into the *next* token. A dangling
  // credential name therefore arms the following token as well; forgetting
  // this is how "we redact assignments" still ships the secret.
  let maskNext = false
  for (const token of line.split(' ')) {
    if (!first) out += ' '
    first = false
    if (maskNext && token !== '') {
      out += MASK
      maskNext = false
      continue
    }
    const rendered = redactToken(token)
    maskNext = rendered.dangling
    out += rendered.text
  }
  return out
}

/**
 * The rendered token, and whether it was a credential *name* whose value has
 * not been seen yet.
 */
function redactToken(token: string): { text: string; dangling: boolean } {
  for (const separator of ['=', ':']) {
    const assignment = maskAssignment(token, separator)
    if (assignment.kind === 'masked') return { text: assignment.text, dangling: false }
    if (assignment.kind === 'name-only') return { text: assignment.text, dangling: true }
  }
  if (SECRET_PREFIXES.some((prefix) => token.includes(prefix))) {
    return { text: MASK, dangling: false }
  }
  return { text: token, dangling: false }
}

type Assignment =
  { kind: 'masked'; text: string } | { kind: 'name-only'; text: string } | { kind: 'not-sensitive' }

/** `NAME<sep>value` where `NAME` looks like a credential name. */
function maskAssignment(token: string, separator: string): Assignment {
  const at = token.indexOf(separator)
  if (at < 0) return { kind: 'not-sensitive' }
  const name = token.slice(0, at)
  const value = token.slice(at + separator.length)
  const lowered = name.toLowerCase()
  if (!SENSITIVE_KEY_FRAGMENTS.some((fragment) => lowered.includes(fragment))) {
    return { kind: 'not-sensitive' }
  }
  if (value === '') return { kind: 'name-only', text: `${name}${separator}` }
  return { kind: 'masked', text: `${name}${separator}${MASK}` }
}

/**
 * Where the observability streams live — the port of `chiefd_log::sink`'s
 * `log_root_from_env`, answer for answer. This is a PORT, not a second
 * policy: a Pi extension that resolved its own root would split the record of
 * one launch across two directories, which is the exact failure that made a
 * 4½-minute launch undiagnosable.
 *
 * TWO answers, both named, neither guessed:
 *
 * 1. `ORG_LAUNCHER_ORG_DIR` — the company directory a pane is stamped with.
 *    Its logs go to `<dir>/.chief/log/`, beside the store whose story they
 *    tell.
 * 2. `$HOME/.chief/log/` — for a BOX-WIDE process, which is a real category
 *    and not a fallback: `chief` itself spends the minutes before any company
 *    exists with no directory to write into.
 *
 * # Why the four-tier ladder this replaces was a defect
 *
 * It tried `ORG_LAUNCHER_DATA_ROOT`, then `dirname(CHIEFD_DATA_ROOT)`, then
 * `$HOME/.chiefd`, then the literal `/root/.chiefd`. Tier 2 RECONSTRUCTED a
 * directory by walking up from another one, correct only while the launcher
 * derived the data root as exactly `dirname(orgs)`; tier 4 was only ever right
 * on one Linux box running as root. Because this writer is best-effort by
 * construction, guessing wrong is SILENT. With the company IN the directory
 * there is nothing left to reconstruct.
 *
 * # Why no answer is an answer
 *
 * The deleted last resort was `/root/.chiefd`, which on any host where `$HOME`
 * is unset is a directory the process almost certainly cannot write — so the
 * ladder's final rung produced the same silence it existed to prevent, while
 * looking like it had an answer. `undefined` says the honest thing.
 */
export function chiefdLogDirectory(environment: TraceEnvironment): string | undefined {
  const companyDir = usable(environment.ORG_LAUNCHER_ORG_DIR)
  if (companyDir) return join(companyDir, CHIEF_DIR, 'log')
  const home = usable(environment.HOME)
  if (home) return join(home, CHIEF_DIR, 'log')
  return undefined
}

function usable(value: string | undefined): string {
  return value?.trim() ?? ''
}

/** The per-stream cap, from the same variable the Rust sink reads. */
export function traceMaxBytes(environment: TraceEnvironment): number {
  // Base 10 explicitly (eslint 10's `radix`): a byte cap is a decimal number,
  // and a radix-less parse would read a `0x`-prefixed value as hex.
  const parsed = Number.parseInt(usable(environment[MAX_BYTES_ENV]), 10)
  if (Number.isFinite(parsed) && parsed > 0) return parsed
  return DEFAULT_MAX_BYTES
}

/**
 * Sequence numbers are per *stream*, not per process: one process may write
 * several, so a process-wide counter would leave gaps in every file and a gap
 * would prove nothing. Per-stream, a gap is positive evidence that a line was
 * lost or the file was truncated.
 */
const sequences = new Map<string, number>()

/** Lines this process failed to write, surfaced so a silent sink is visible. */
let dropped = 0

/** How many lines this process could not write. */
export function droppedTraceLines(): number {
  return dropped
}

/** Test seam: the per-stream sequence counters are process-local state. */
export function resetTraceStreamStateForTests(): void {
  sequences.clear()
  dropped = 0
}

/** One open step. `close` returns the measured duration so a caller can report it. */
export interface TraceStep {
  /** Add fields that the exit line will carry. */
  record(detail: TraceDetail): void
  /** Close the step, writing the `exit` line with `detail.durationMs`. */
  close(detail?: TraceDetail): number
  /** Close the step at `error` level, naming the failure. */
  fail(error: unknown, detail?: TraceDetail): number
}

export interface LaunchTraceOptions {
  /** The environment the data root and cap are resolved from. */
  readonly environment: TraceEnvironment
  /** Names the file. Defaults to `founder-pi`. */
  readonly service?: string
  /** Epoch milliseconds. One seam serves both `at` and `durationMs`, so a
   *  line's timestamp and the duration beside it can never disagree. */
  readonly now?: () => number
  /** Overrides the resolved directory. Tests use it; nothing else does. */
  readonly directory?: string
  /** Overrides the reported pid. Tests use it; nothing else does. */
  readonly pid?: number
}

/**
 * A JSONL stream, and the span timer over it.
 *
 * ```ts
 * const trace = new LaunchTrace({ environment: process.env })
 * const step = trace.step('founder.registry.refresh')
 * await registry.refresh()
 * step.close()
 * ```
 */
export class LaunchTrace {
  readonly #directory: string | undefined
  readonly #path: string | undefined
  readonly #service: string
  readonly #maxBytes: number
  readonly #now: () => number
  readonly #pid: number
  #organization = NO_ORGANIZATION

  constructor(options: LaunchTraceOptions) {
    this.#directory = options.directory ?? chiefdLogDirectory(options.environment)
    this.#service = options.service ?? FOUNDER_TRACE_SERVICE
    this.#path = this.#directory ? join(this.#directory, `${this.#service}.jsonl`) : undefined
    this.#maxBytes = traceMaxBytes(options.environment)
    this.#now = options.now ?? (() => Date.now())
    this.#pid = options.pid ?? process.pid
  }

  /**
   * The stream this trace appends to, or `undefined` when the environment
   * names no directory to write into. A process with neither a company
   * directory nor a `$HOME` has nowhere honest to put a file, and inventing
   * one is how the deleted `/root/.chiefd` tier wrote nothing while looking
   * like it had an answer.
   */
  get path(): string | undefined {
    return this.#path
  }

  /**
   * Name the company every subsequent line is about.
   *
   * The trace opens org-agnostic because the process that owns the slow part
   * of a launch has no company yet, and has one by the time it finishes. A
   * step still open when this is called carries the slug on its exit line,
   * so one grep finds the whole launch.
   */
  nameCompany(slug: string): void {
    const named = slug.trim()
    if (named) this.#organization = named
  }

  /** Write one line. */
  event(level: TraceLevel, event: string, detail?: TraceDetail): void {
    this.#emit(level, event, detail ?? {})
  }

  /**
   * Open a step. The returned handle writes the `exit` line, whose
   * `detail.durationMs` is THE measurement: a step that blocks for minutes
   * says so on the line that ends it, without anybody subtracting timestamps
   * by hand.
   */
  step(name: string, detail?: TraceDetail): TraceStep {
    const opened = this.#now()
    const fields: TraceDetail = { ...detail }
    this.#emit('info', name, { ...fields, phase: 'enter' })
    let closed = false
    const finish = (level: TraceLevel, extra: TraceDetail): number => {
      const durationMs = Math.max(0, this.#now() - opened)
      if (closed) return durationMs
      closed = true
      this.#emit(level, name, { ...fields, ...extra, phase: 'exit', durationMs })
      return durationMs
    }
    return {
      record: (more: TraceDetail): void => {
        Object.assign(fields, more)
      },
      close: (extra?: TraceDetail): number => finish('info', extra ?? {}),
      fail: (error: unknown, extra?: TraceDetail): number =>
        finish('error', { ...extra, error: failureText(error) })
    }
  }

  /**
   * Time `body`, closing the step on either outcome. A step that threw must
   * still say how long it ran before it did — the failing path is the one an
   * operator is reading the file about.
   */
  async measure<T>(name: string, body: () => Promise<T>, detail?: TraceDetail): Promise<T> {
    const step = this.step(name, detail)
    try {
      const result = await body()
      step.close()
      return result
    } catch (error) {
      step.fail(error)
      throw error
    }
  }

  /**
   * [`LaunchTrace.measure`] for a step that does not await.
   *
   * A synchronous step is instrumented for the same reason a blocking one is:
   * a number that reads 0 ms is what RULES IT OUT, and ruling steps out is how
   * the remaining one gets named. Every guess about this launch so far has
   * been wrong, so nothing on the path is assumed cheap.
   */
  measureSync<T>(name: string, body: () => T, detail?: TraceDetail): T {
    const step = this.step(name, detail)
    try {
      const result = body()
      step.close()
      return result
    } catch (error) {
      step.fail(error)
      throw error
    }
  }

  #emit(level: TraceLevel, event: string, detail: TraceDetail): void {
    try {
      this.#append(this.#render(level, event, detail))
    } catch {
      dropped += 1
    }
  }

  #render(level: TraceLevel, event: string, detail: TraceDetail): string {
    const record: Record<string, unknown> = {
      schemaVersion: CHIEFD_LOG_SCHEMA_VERSION,
      at: new Date(this.#now()).toISOString(),
      level,
      service: this.#service,
      event: redactSecrets(event),
      organization: this.#organization,
      pid: this.#pid,
      // Sequence numbers are per STREAM. A trace with no directory writes
      // nothing, so all its unwritten lines share one counter under a name no
      // file can take.
      seq: nextSequence(this.#path ?? '')
    }
    const body = redactDetail(detail)
    if (Object.keys(body).length > 0) record.detail = body
    /* eslint-disable lucy/no-json-stringify */
    // Wire text. This IS the serializer for this stream, so there is no
    // further serializer to defer to — and `JSON.stringify` is what makes the
    // framing safe: a field value carrying a quote, a brace or a newline is
    // escaped once, correctly, and can never split a line.
    return `${JSON.stringify(record)}\n`
    /* eslint-enable lucy/no-json-stringify */
  }

  /**
   * Append, rotating first when the file would exceed the cap. Exactly one
   * previous generation is retained, so on-disk bytes for a stream never
   * exceed `2 * maxBytes` plus one in-flight line — the identical bound
   * `chiefd_log::sink::OrgLog::append` keeps.
   */
  #append(line: string): void {
    const directory = this.#directory
    const path = this.#path
    // No directory means the environment named none. Dropping the line is the
    // honest outcome; the console layer still carries it.
    if (!directory || !path) return
    mkdirSync(directory, { recursive: true, mode: 0o700 })
    const size = Buffer.byteLength(line)
    let onDisk: number
    try {
      onDisk = statSync(path).size
    } catch {
      onDisk = 0
    }
    if (onDisk > 0 && onDisk + size > this.#maxBytes) {
      try {
        renameSync(path, `${path}.1`)
      } catch {
        // A rotation that cannot be published must never stop the line from
        // being written.
      }
    }
    appendFileSync(path, line, { encoding: 'utf8', mode: 0o600 })
  }
}

function nextSequence(path: string): number {
  const value = sequences.get(path) ?? 0
  sequences.set(path, value + 1)
  return value
}

/** The text of a thrown value, redacted like every other string on a line. */
function failureText(error: unknown): string {
  return redactSecrets(error instanceof Error ? error.message : String(error))
}

/**
 * Redact every string that reaches a line, at any depth.
 *
 * A log is the last place a credential can escape, and the environment this
 * runs in carries `OPENROUTER_API_KEY`, `XCOM_API_KEY` and
 * `TRIBES_SSH_PUBLIC_KEY`. Numbers and booleans pass through: masking them
 * would delete the measurement without protecting anything.
 */
function redactDetail(detail: object): TraceDetail {
  // Key-ordered, like the Rust layer's `BTreeMap`: two lines about the same
  // step must be diffable, and insertion order is whatever the call site
  // happened to use.
  const entries = Object.entries(detail).sort(([left], [right]) => (left < right ? -1 : 1))
  const out: TraceDetail = {}
  for (const [key, value] of entries) {
    out[key] = redactValue(value)
  }
  return out
}

function redactValue(value: unknown): unknown {
  if (typeof value === 'string') return redactSecrets(value)
  if (typeof value === 'number' || typeof value === 'boolean') return value
  if (Array.isArray(value)) return value.map(redactValue)
  if (value instanceof Error) return redactSecrets(value.message)
  if (value && typeof value === 'object') return redactDetail(value)
  // `undefined`, a function or a symbol has no place on a line; naming the
  // absence is more honest than an omitted key a reader could read as a step
  // that never ran.
  return null
}
