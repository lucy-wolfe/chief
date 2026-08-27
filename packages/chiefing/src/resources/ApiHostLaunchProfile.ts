import { ChiefdUnavailableError } from '@/Errors'
import { isNullish } from '@/Nullish'
import { postOrgRoute } from '@/resources/OrgRoutes'
import type {
  ApiHostActuation,
  ApiHostLaunchProfile,
  ApiHostLaunchProfileRead
} from '@/types/ApiHostLaunchProfile'
import type { HttpTransport } from '@/types/Transport'

const PATH = '/v1/org/api-host-launch-profile/read'

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

function malformed(url: string): ChiefdUnavailableError {
  return new ChiefdUnavailableError({ kind: 'malformed-body', url, path: PATH })
}

function stringRecord(value: unknown, url: string): Record<string, string> {
  if (!isRecord(value)) throw malformed(url)
  const result: Record<string, string> = {}
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry !== 'string') throw malformed(url)
    result[key] = entry
  }
  return result
}

function stringArray(value: unknown, url: string): string[] {
  if (
    !Array.isArray(value) ||
    !value.every((entry): entry is string => typeof entry === 'string')
  ) {
    throw malformed(url)
  }
  return value
}

function requiredString(value: unknown, url: string): string {
  if (typeof value !== 'string') throw malformed(url)
  return value
}

function decodePlan(value: unknown, url: string): ApiHostLaunchProfile {
  if (
    !isRecord(value) ||
    typeof value.personId !== 'string' ||
    typeof value.cwd !== 'string' ||
    typeof value.displayName !== 'string'
  ) {
    throw malformed(url)
  }
  return {
    personId: value.personId,
    cwd: value.cwd,
    env: stringRecord(value.env, url),
    // Absent is ORDINARY — a person who has never spoken has no transcript —
    // so it is optional, while a present non-string is malformed rather than
    // quietly dropped.
    ...(isNullish(value.sessionFile)
      ? {}
      : { sessionFile: requiredString(value.sessionFile, url) }),
    tools: stringArray(value.tools, url),
    displayName: value.displayName
  }
}

function decodeActuation(value: unknown, url: string): ApiHostActuation {
  if (
    !isRecord(value) ||
    typeof value.effectiveMode !== 'string' ||
    typeof value.configuredMode !== 'string' ||
    typeof value.breakerTripped !== 'boolean'
  ) {
    throw malformed(url)
  }
  return {
    effectiveMode: value.effectiveMode,
    configuredMode: value.configuredMode,
    breakerTripped: value.breakerTripped
  }
}

/** Typed ChiefD client for the Rust-owned API/RPC child projection. The
 * resource intentionally has no write method: credential materialization and
 * all launch policy stay outside the HTTP read path. */
export class ApiHostLaunchProfileClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  async read(slug: string): Promise<ApiHostLaunchProfileRead> {
    const wire = await postOrgRoute<unknown>(this.transport, this.url, PATH, {
      slug
    })
    if (!isRecord(wire) || !Array.isArray(wire.plans)) throw malformed(this.url)
    return {
      // Who is actuating is REQUIRED, not optional: a caller that must not
      // double-actuate decides from it, and a silently absent field would let
      // that caller default to "go ahead".
      actuation: decodeActuation(wire.actuation, this.url),
      plans: wire.plans.map((plan) => decodePlan(plan, this.url))
    }
  }
}
