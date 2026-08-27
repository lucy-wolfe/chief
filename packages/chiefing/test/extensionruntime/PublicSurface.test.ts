import { describe, expect, it } from 'vitest'

import * as extensionRuntime from '@/extensionruntime/index'

describe('extension-runtime public surface', () => {
  it('exports the pane transport, document, row, and SSE primitives', () => {
    expect(typeof extensionRuntime.readDaemonRendezvous).toBe('function')
    expect(typeof extensionRuntime.rendezvousPath).toBe('function')
    expect(typeof extensionRuntime.ChiefdUnavailableError).toBe('function')
    expect(typeof extensionRuntime.isTransientChiefdError).toBe('function')
    expect(typeof extensionRuntime.OrgRowRefusalError).toBe('function')
    expect(typeof extensionRuntime.FetchTransport).toBe('function')
    expect(extensionRuntime.CONNECT_RETRY_BACKOFFS_MS).toEqual([25, 75, 150])
    expect(typeof extensionRuntime.awaitedDelay).toBe('function')
    expect(typeof extensionRuntime.DocsClient).toBe('function')
    expect(typeof extensionRuntime.postOrgRoute).toBe('function')
    expect(typeof extensionRuntime.RowStoresClient).toBe('function')
    expect(typeof extensionRuntime.activeSseHubCount).toBe('function')
    expect(typeof extensionRuntime.subscribeSse).toBe('function')
    expect(typeof extensionRuntime.computeBackoffDelayMs).toBe('function')
    expect(typeof extensionRuntime.SseWatcher).toBe('function')
  })

  // #751/P7: a pane's credential is the P-256 key in its own pi-home, so the
  // token manager and the key reader are part of the pane surface now. They
  // are asserted PRESENT rather than merely un-forbidden, because the negative
  // assertion below is what used to keep them out and a silent removal of
  // either would leave every agent unable to prove who it is.
  it('exports the pane credential pair', () => {
    expect(typeof extensionRuntime.AgentTokenManager).toBe('function')
    expect(typeof extensionRuntime.readAgentKeypair).toBe('function')
    expect(typeof extensionRuntime.IDENTITY_KEY_FILENAME).toBe('string')
  })

  // A4: the pane's ACQUIRER, not just its parts. `team-ui` and every SSE reader
  // ran in the same pane over the same key and reached chiefd with nothing —
  // not because they had no key, but because the thing that turns a key into a
  // bearer lived inside one extension. Asserted PRESENT for the same reason as
  // the pair above: a silent removal would put every non-org-tool caller back
  // on an unauthenticated transport, which is exactly the state this packet
  // exists to end.
  it('exports the ONE pane-side acquirer, so no caller has to build a second one', () => {
    expect(typeof extensionRuntime.paneTokenManager).toBe('function')
    expect(typeof extensionRuntime.paneChiefdTransport).toBe('function')
  })

  // #983: an org extension resolves ITS OWN company's daemon rather than
  // reading one process-global address, so the discovery client and the
  // beacond-address helper are part of the pane surface now. Same reasoning as
  // the credential pair above: asserted PRESENT, because their absence is what
  // forced the process-global variable that made a multi-company host
  // impossible.
  it('exports company discovery, so an install can resolve its own daemon', () => {
    expect(typeof extensionRuntime.DiscoveryClient).toBe('function')
    expect(typeof extensionRuntime.resolveCompanyChiefdUrl).toBe('function')
    expect(typeof extensionRuntime.beacondUrlFromEnvironment).toBe('function')
    expect(extensionRuntime.DEFAULT_BEACOND_URL).toBe('http://127.0.0.1:6969')
    expect(extensionRuntime.BEACOND_URL_ENV).toBe('BEACOND_URL')
    expect(typeof extensionRuntime.UnknownCompanyError).toBe('function')
    expect(typeof extensionRuntime.CompanyNotRunningError).toBe('function')
    expect(typeof extensionRuntime.BeacondUnavailableError).toBe('function')
  })

  it('does not leak enrolment or unrelated resource implementations', () => {
    // Enrolment is the OPERATOR's verb — a pane presents a key, it never
    // decides which keys the daemon trusts.
    expect('enrollPersonIdentities' in extensionRuntime).toBe(false)
    expect('AuthClient' in extensionRuntime).toBe(false)
    expect('LocksClient' in extensionRuntime).toBe(false)
    expect('StaffingClient' in extensionRuntime).toBe(false)
  })
})
