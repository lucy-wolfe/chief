/**
 * #270: the boot-time structural-root gate must distinguish a transient
 * normalized-manifest read failure from a genuine non-root person. A read
 * failure fails open (and is logged by the installer); a successfully-read
 * department head remains silently tool-less.
 */
import { createServer, type Server } from 'node:http'

import { isNullish } from '@test/support/Nullish'
import {
  type OrganizationRuntimeContext,
  resolveInstallerStructuralRoot
} from '@test-assets/organization-intercom'
import { afterEach, describe, expect, it } from 'vitest'

const servers: Server[] = []

afterEach(async () => {
  for (const server of servers.splice(0)) {
    await new Promise<void>((resolve) => server.close(() => resolve()))
  }
})

function fixtureManifest(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: 'acme',
    name: 'Acme',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'quant'],
    peopleOrder: ['ceo', 'quant-head'],
    departments: {
      executive: {
        id: 'executive',
        name: 'Executive',
        purpose: 'Root department.',
        headPersonId: 'ceo',
        state: 'active'
      },
      quant: {
        id: 'quant',
        name: 'Quant',
        purpose: 'Child department.',
        parentDepartmentId: 'executive',
        headPersonId: 'quant-head',
        state: 'active'
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      'quant-head': person('quant-head', 'head', 'quant')
    }
  }
}

function person(
  id: string,
  kind: 'executive' | 'head',
  departmentId: string
): Record<string, unknown> {
  return {
    id,
    name: id,
    title: id,
    kind,
    departmentId,
    employmentState: 'active',
    createdAt: '2026-07-15T00:00:00.000Z'
  }
}

interface ManifestReadResponse {
  found: boolean
  manifest?: string
  seq?: number
}

/**
 * Boot a real `node:http` server standing in for chiefd's
 * `POST /v1/org/manifest/read` route and hand back its address.
 * `resolveInstallerStructuralRoot` reads through the shared async
 * chiefing-extension-runtime transport (`postOrgRoute`/`FetchTransport`)
 * now that the legacy synchronous `spawnSync("curl")` transport is deleted
 * (#794/E4-S8) — an in-process HTTP server is fine here because the caller
 * is genuinely async, unlike the old synchronous-curl shape this suite used
 * to work around.
 *
 * The address is RETURNED and put on the context rather than exported through
 * `process.env`: the extension resolves its daemon from the context it is
 * given, so a suite that stamped an ambient variable would be testing a seam
 * that no longer exists.
 */
async function installManifestReadFixture(
  response: ManifestReadResponse | undefined
): Promise<string> {
  const server = createServer((req, res) => {
    if (req.url !== '/v1/org/manifest/read' || isNullish(response)) {
      res.statusCode = isNullish(response) ? 503 : 404
      res.end()
      return
    }
    res.setHeader('content-type', 'application/json')
    /* eslint-disable lucy/no-json-stringify */
    // See packages/piing/test/support/JsonFixture.ts's header (#833/#842):
    // this fixture-only HTTP response body has no sanctioned replacement.
    res.end(JSON.stringify(response))
    /* eslint-enable lucy/no-json-stringify */
  })
  servers.push(server)
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (typeof address !== 'object' || isNullish(address)) {
    throw new Error('structural-root fixture server did not report a port')
  }
  return `http://127.0.0.1:${address.port}`
}

function context(personId: string, chiefdUrl: string): OrganizationRuntimeContext {
  return {
    organizationDir: '/fixture/acme',
    identityDir: '/fixture/acme/.chief',
    organization: 'acme',
    personId,
    launcherRoot: '/fixture/launcher',
    chiefdUrl,
    // The company key, as the rendezvous served it. A context carrying an
    // address but no key is the parsed-only state, which refuses on use.
    companyKey: '0123456789ab'
  }
}

function manifestResponse(): ManifestReadResponse {
  /* eslint-disable lucy/no-json-stringify */
  // See packages/piing/test/support/JsonFixture.ts's header (#833/#842):
  // this is the wire-shaped `manifest` STRING field the real read route
  // returns, not production formatting.
  const manifest = JSON.stringify(fixtureManifest())
  /* eslint-enable lucy/no-json-stringify */
  return { found: true, manifest, seq: 1 }
}

describe('#270 resolveInstallerStructuralRoot', () => {
  it('reports the structural root as root without a read failure', async () => {
    const chiefdUrl = await installManifestReadFixture(manifestResponse())
    await expect(resolveInstallerStructuralRoot(context('ceo', chiefdUrl))).resolves.toMatchObject({
      isRoot: true,
      readFailed: false,
      attempts: 1
    })
  })

  it('reports a genuine department head as non-root without a read failure', async () => {
    const chiefdUrl = await installManifestReadFixture(manifestResponse())
    const probe = await resolveInstallerStructuralRoot(context('quant-head', chiefdUrl))
    expect(probe.isRoot).toBe(false)
    expect(probe.readFailed).toBe(false)
    expect(probe.attempts).toBe(1)
  })

  it('retries a persistent manifest-read failure and reports fail-open state', async () => {
    const chiefdUrl = await installManifestReadFixture(undefined)
    const probe = await resolveInstallerStructuralRoot(context('ceo', chiefdUrl))
    expect(probe.isRoot).toBe(false)
    expect(probe.readFailed).toBe(true)
    expect(probe.attempts).toBeGreaterThan(1)
    expect(probe.error).toBeDefined()
  }, 15_000)

  it('treats an unknown installer as undetermined rather than a genuine non-root', async () => {
    const chiefdUrl = await installManifestReadFixture(manifestResponse())
    const probe = await resolveInstallerStructuralRoot(context('nobody', chiefdUrl))
    expect(probe.isRoot).toBe(false)
    expect(probe.readFailed).toBe(true)
    expect(probe.attempts).toBeGreaterThan(1)
  }, 15_000)
})
