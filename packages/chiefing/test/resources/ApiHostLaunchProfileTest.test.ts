import { fixedResponseTransport, RecordingTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ChiefdUnavailableError, OrgRowRefusalError } from '@/Errors'
import { ApiHostLaunchProfileClient } from '@/resources/ApiHostLaunchProfile'

const PATH = '/v1/org/api-host-launch-profile/read'

/** Who is actuating, as chiefd now publishes it on every successful read.
 *
 * This read used to REFUSE outside shadow mode; the three facts that refusal
 * carried ride on the answer instead, and the caller that must not
 * double-actuate decides from them. */
const ACTUATION = { effectiveMode: 'shadow', configuredMode: 'shadow', breakerTripped: false }

const PLAN = {
  personId: 'val',
  cwd: '/orgs/acme/people/val/workspace',
  env: {
    ORG_CHIEFD_URL: 'http://127.0.0.1:7419',
    PI_CODING_AGENT_SESSION_DIR: '/orgs/acme/.chief/agent/val/sessions'
  },
  sessionFile: '/orgs/acme/.chief/agent/val/sessions/val.jsonl',
  tools: ['read', 'org_send'],
  displayName: 'Acme · Engineer'
}

describe('ApiHostLaunchProfileClient', () => {
  it('reads the typed, keyed, non-secret API/RPC child profile', async () => {
    const transport = fixedResponseTransport(200, { actuation: ACTUATION, plans: [PLAN] })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('0123456789ab')).resolves.toEqual({
      actuation: ACTUATION,
      plans: [PLAN]
    })
    expect(transport.calls).toEqual([
      {
        method: 'POST',
        path: PATH,
        // The company key, verbatim. It used to be rewritten here from a
        // display slug plus a data root into `acme@074619b89d1b`.
        body: { slug: '0123456789ab' }
      }
    ])
  })

  it('keeps typed profile refusals in the ordinary org-route taxonomy', async () => {
    // `company-not-api-hosted` used to be the 422 here. chiefd no longer
    // refuses a READ over the actuation mode, so the surviving 422 is the one
    // about a person's own artifacts.
    const transport = fixedResponseTransport(422, {
      code: 'api-host-launch-profile-not-materialized',
      detail: "person 'val' has no fully materialized API-host launch profile"
    })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('acme')).rejects.toBeInstanceOf(OrgRowRefusalError)
    await expect(client.read('acme')).rejects.toMatchObject({
      status: 422,
      code: 'api-host-launch-profile-not-materialized'
    })
  })

  it('maps an unavailable profile source to ChiefdUnavailableError', async () => {
    const transport = fixedResponseTransport(503, {
      code: 'api-host-launch-profile-unavailable',
      detail: 'the daemon has no live source'
    })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('acme')).rejects.toEqual(
      expect.objectContaining({
        name: 'ChiefdUnavailableError',
        kind: 'http-error',
        status: 503,
        path: PATH
      })
    )
  })

  it('carries a person with no transcript rather than inventing a path', async () => {
    // A person who has never spoken has no session file. That is ORDINARY, so
    // it decodes; the host creates the transcript on the first turn. Inventing
    // a path here would fork history the moment chiefd materialized the real
    // one.
    const { sessionFile: _omitted, ...neverSpoke } = PLAN
    const transport = fixedResponseTransport(200, { actuation: ACTUATION, plans: [neverSpoke] })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('acme')).resolves.toEqual({
      actuation: ACTUATION,
      plans: [neverSpoke]
    })
  })

  it('refuses a present-but-wrong session file rather than dropping it', async () => {
    // Absent is ordinary; present-and-not-a-string is a daemon this client does
    // not understand. Dropping it would host the agent on a NEW transcript and
    // silently lose the conversation.
    const transport = fixedResponseTransport(200, {
      actuation: ACTUATION,
      plans: [{ ...PLAN, sessionFile: 7 }]
    })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('acme')).rejects.toBeInstanceOf(ChiefdUnavailableError)
  })

  it('rejects a malformed successful body instead of inventing launch facts', async () => {
    const transport = fixedResponseTransport(200, {
      actuation: ACTUATION,
      plans: [{ ...PLAN, tools: ['read', 7] }]
    })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('acme')).rejects.toBeInstanceOf(ChiefdUnavailableError)
    await expect(client.read('acme')).rejects.toMatchObject({
      kind: 'malformed-body',
      path: PATH
    })
  })

  it('refuses a body with no actuation facts rather than assuming shadow', async () => {
    // The caller that must not stand up a second actuator decides from these,
    // so a silently absent field would let it default to "go ahead" — which is
    // exactly the double-roster the removed server-side gate prevented.
    const transport = fixedResponseTransport(200, { plans: [PLAN] })
    const client = new ApiHostLaunchProfileClient(transport, 'http://chiefd.example')

    await expect(client.read('acme')).rejects.toBeInstanceOf(ChiefdUnavailableError)
    await expect(client.read('acme')).rejects.toMatchObject({
      kind: 'malformed-body',
      path: PATH
    })
  })

  it('records the profile route before its responder is allowed to fail', async () => {
    const transport = new RecordingTransport(() => ({ status: 500, body: 'not used' }))
    const client = new ApiHostLaunchProfileClient(transport)

    await client.read('acme').catch(() => {})
    expect(transport.calls[0]?.path).toBe(PATH)
  })
})
