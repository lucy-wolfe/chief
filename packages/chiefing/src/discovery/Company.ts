import { isNullish } from '../Nullish.js'
import type { CompanyRow } from '../types/Discovery.js'

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

/** The compiled-in beacond address. Loopback, because beacond has no auth. */
export const DEFAULT_BEACOND_URL = 'http://127.0.0.1:6969'

/** Env var naming a non-default beacond. Read by CALLERS and passed in —
 * chiefing never touches the ambient environment itself. */
export const BEACOND_URL_ENV = 'BEACOND_URL'

/** Returns the trimmed BEACOND_URL when it is a valid http:/https: URL,
 * otherwise DEFAULT_BEACOND_URL. Pure over an explicit record; never reads
 * the ambient environment. Real in the stub (E0-S4). */
export function beacondUrlFromEnvironment(
  environment: Readonly<Record<string, string | undefined>>
): string {
  const trimmed = (environment[BEACOND_URL_ENV] ?? '').trim()
  if (trimmed.length === 0) {
    return DEFAULT_BEACOND_URL
  }
  try {
    const parsed = new URL(trimmed)
    if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {
      return trimmed
    }
    return DEFAULT_BEACOND_URL
  } catch {
    return DEFAULT_BEACOND_URL
  }
}

function malformed(detail: string): never {
  throw new Error(`malformed company row: ${detail}`)
}

/** `undefined`/`null` both mean "absent". Anything else must be a non-empty
 * string, or the field is malformed — never coerced, never silently dropped. */
function requiredString(record: Record<string, unknown>, field: string): string {
  const raw = record[field]
  if (typeof raw !== 'string' || raw.length === 0) {
    malformed(`${field} must be a non-empty string`)
  }
  return raw
}

function optionalString(record: Record<string, unknown>, field: string): string | undefined {
  const raw = record[field]
  if (isNullish(raw)) return undefined
  if (typeof raw !== 'string' || raw.length === 0) {
    malformed(`${field} must be a non-empty string, or absent`)
  }
  return raw
}

/** `pid`/`port` are positive integers when present — a `0` or a negative
 * value is never a real OS pid or a real bound port. */
function optionalPositiveInt(record: Record<string, unknown>, field: string): number | undefined {
  const raw = record[field]
  if (isNullish(raw)) return undefined
  if (typeof raw !== 'number' || !Number.isInteger(raw) || raw < 1) {
    malformed(`${field} must be a positive integer, or absent`)
  }
  return raw
}

/** Twelve lowercase hex characters — the shape `sha256(dir)[..12]` produces.
 *
 * A SHAPE check, deliberately not a re-derivation: re-hashing `dir` here would
 * make this client a SECOND producer of the identity, which is the whole defect
 * the served `key` field exists to end. It is the same check beacond's own
 * `wire::is_company_key` runs on the way in, so a row that reached the registry
 * cannot fail it here — what it catches is a body from something that is not
 * beacond. */
function requiredCompanyKey(record: Record<string, unknown>, field: string): string {
  const raw = requiredString(record, field)
  if (!/^[0-9a-f]{12}$/.test(raw)) {
    malformed(`${field} must be twelve lowercase hex characters`)
  }
  return raw
}

/** Validates an unknown value as a wire CompanyRow: requires dir/key/slug/
 * registeredAt, accepts a row with no location fields (or explicit nulls
 * treated as absent), and rejects a partial location. An absent optional
 * field and a `null` are both "no location"; anything else malformed —
 * this is where the wire boundary is checked, so nothing downstream ever
 * has to re-validate a `CompanyRow`. */
export function parseCompanyRow(value: unknown): CompanyRow {
  if (!isRecord(value)) {
    malformed('not an object')
  }
  const record = value

  const dir = requiredString(record, 'dir')
  const key = requiredCompanyKey(record, 'key')
  const slug = requiredString(record, 'slug')
  const registeredAt = requiredString(record, 'registeredAt')

  const url = optionalString(record, 'url')
  const port = optionalPositiveInt(record, 'port')
  const pid = optionalPositiveInt(record, 'pid')
  const hostname = optionalString(record, 'hostname')
  const lastSeenAt = optionalString(record, 'lastSeenAt')

  // All five location fields land together (chiefd's own register writes
  // them as one row) or none do — a partial location is malformed, never a
  // "best effort" partial object.
  const locationFields = [url, port, pid, hostname, lastSeenAt]
  const presentCount = locationFields.filter((field) => !isNullish(field)).length
  if (presentCount !== 0 && presentCount !== locationFields.length) {
    malformed('location fields must all be present or all be absent')
  }

  return { dir, key, slug, registeredAt, url, port, pid, hostname, lastSeenAt }
}
