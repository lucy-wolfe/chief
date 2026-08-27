// #776: StaffingClient's refusal-as-value contract against the real policy
// engine (chiefd-core/src/store/org_ops.rs) -- the one guarantee #880 flags
// as unprovable against a fake: a scripted transport can return whatever
// shape a test wants, but only the real engine's own atomic transaction
// actually decides "applied" vs "refused" for a genuine structural request.
//
// #751/G6 CORRECTION. This file used to pre-compute the composite
// `documentKey(slug, dataRoot)` and pass it into every StaffingClient call,
// on the stated grounds that "StaffingClient (unlike every other resource
// client) takes NO root and does no company-keying of its own". That was
// true when written and is not true now: the StaffingClient-without-root
// live defect was fixed (`src/resources/Staffing.ts`'s
// `companyKeyedTransport`, with its own postmortem in place), so the client
// keys the slug itself. Pre-keying here therefore produced a DOUBLE-keyed
// slug, the live-company resolver returned None, and all four tests in this
// file failed with a bare `http-error`.
//
// They failed invisibly. Every one of these suites is gated on the chiefd
// debug test binary and `describe.skip`s without it, so on a machine that never
// built one, this file reported nothing at all — see
// `ContractSuiteResidual.test.ts`, which now makes that residual a red.
//
// The composite is now deleted outright and so is the bare slug that briefly
// replaced it: a company is addressed by its KEY, `sha256(<dir>)[..12]`, and
// every call below passes `companyKey` exactly like every other resource. The
// display slug survives only as the company's NAME — what `genesisSpecFor`
// slugifies back — so the two are held in two variables here and never one.
//
// A6(c): the daemon is now `chiefd run --serve-only`, not `chiefd
// docstore-only`. The `/v1/org/*` ROUTE FAMILY moved off the unauthenticated
// mount; docstore-only's own posture is unchanged, A5's fence stands, and it
// still serves `/v1/docs/*`. Nothing about what this file proves changes with
// it: `org_ops.rs` is the same policy engine on both mounts, and every
// assertion below is still the real engine's own atomic transaction deciding
// `applied` against `refused`.
import { chiefdBinarySkipTitle, chiefdBinaryTestGate } from '@chief/testing'
import { bootContractDaemon, genesisSpecFor } from '@test/contract/support/bootContractDaemon'
import type { ContractDaemon } from '@test/types/Contract'
import { afterEach, describe, expect, it } from 'vitest'

import type { ChiefdClient } from '@/ChiefdClient'
import { ROOT_DEPARTMENT_ID } from '@/types/Organization'

const SUITE_LABEL = 'StaffingContract (real chiefd --serve-only)'
const gate = chiefdBinaryTestGate()
const maybeDescribe = gate.present ? describe : describe.skip

maybeDescribe(gate.present ? SUITE_LABEL : chiefdBinarySkipTitle(SUITE_LABEL, gate), () => {
  let contract: ContractDaemon | undefined

  afterEach(async () => {
    await contract?.stop()
    contract = undefined
  })

  /** Boots a daemon under the DISPLAY slug and genesises its company, handing
   * back the COMPANY KEY every `/v1/org/*` call below must carry. The two are
   * different values with different jobs: `slug` is the name genesis slugifies
   * from the spec, `companyKey` is `sha256(<dir>)[..12]` and is the only thing
   * a staffing route resolves against. */
  async function bootGenesis(slug: string): Promise<{ client: ChiefdClient; companyKey: string }> {
    contract = await bootContractDaemon(slug)
    const { client, daemon } = contract
    await client.manifest.genesis(daemon.companyKey, genesisSpecFor(slug))
    return { client, companyKey: daemon.companyKey }
  }

  it('pauseDepartment/resumeDepartment applies for real against a department the engine created', async () => {
    // #751/G6 CORRECTION, second half. This test used to name a
    // "genesis-seeded non-root department" called `engineering` and pause it.
    // No such department exists: genesis now takes a company SPEC and derives
    // the manifest itself (DECISIONS.md, "genesis carries the QUESTION, not
    // the answer"), and `genesisSpecFor` describes a company with a CEO and
    // nothing else — so the real engine answered `unknown-department`, which
    // is correct. The premise was stale, not the client.
    //
    // The property this test exists for — the real engine's own transaction
    // decides `applied` for a genuine structural request — is preserved by
    // CREATING the department first through the same client, which is
    // strictly more coverage than the version that assumed one into
    // existence.
    const slug = 'chiefing-contract-staffing-pause'
    const { client, companyKey } = await bootGenesis(slug)

    const manifest = await client.manifest.readManifest(companyKey)
    const chiefPersonId = manifest?.peopleOrder[0]
    expect(chiefPersonId, 'genesis must seed exactly one person, the chief').toBeTypeOf('string')
    if (typeof chiefPersonId !== 'string') return

    // THE REQUESTER IS THE OPERATOR, because the operator is who this suite
    // IS, and a staffing route now BINDS the declared requester to the
    // AUTHENTICATED caller. `bootContractDaemon` presents the daemon's own
    // operator bearer, which is the credential this suite's subject needs.
    //
    // A person bearer used to be IMPOSSIBLE here by construction — `--serve-only`
    // mounts no runtime host, `/v1/org/materialize/run` answers
    // `503 no-runtime-host-capability`, and enrolment rode on the
    // materialization pass that 503 refuses, so `/v1/auth/challenge` for the CEO
    // answered 401 before and after. That is no longer true: a person's identity
    // is provisioned by the genesis transaction itself, so this mode CAN mint a
    // person bearer now. The requester here stays the operator on purpose — the
    // two refusals below are the rules this file exists to state, and both are
    // about what a NON-PERSON principal may do.
    //
    // So the old body — `hire-new` head, requester `{kind:'person', personId:
    // ceo}` — is one refusal deep, pinned below rather than dropped because it
    // is a rule this file is the only place to state: declaring a person over
    // an operator credential is `requester-identity-mismatch`; a non-person
    // principal may act as the operator and never as somebody.
    //
    // THE SECOND REFUSAL IS GONE, AND ITS ABSENCE IS THE OTHER RULE HERE. A
    // `hire-new` head used to need an ATTESTED MANAGER for one reason only —
    // the new person's model route was inherited from that manager, so
    // `modelAuthority.hiringManagerPersonId` had to equal the requester and an
    // operator had no person id to equal it with (`hiring-manager-mismatch`).
    // Provider/model management is deleted, so there is no route to inherit
    // and nothing left for the attestation to protect: an operator may now
    // create a unit and hire its head in one transaction. That is asserted as
    // an APPLIED outcome below, because a refusal that quietly became a
    // success is exactly the drift this contract suite exists to catch.
    //
    // The subject of this test — the real engine's own transaction decides
    // `applied` for a genuine structural request — is then driven on the path
    // an operator DOES hold, which is the one `AtomicStaffingRequester`
    // documents for it: an env-stripped direct command is attributed to the
    // operator instead of impersonating the CEO.
    const headSeed = {
      name: 'Ada',
      title: 'Head of Engineering',
      mandate: 'Run engineering',
      employmentState: 'active' as const,
      activation: 'resident' as const,
      tools: [],
      prompts: []
    }

    const impersonated = await client.staffing.createDepartment(
      companyKey,
      'engineering',
      ROOT_DEPARTMENT_ID,
      'Engineering',
      { kind: 'hire-new', personId: 'engineering-head', personKind: 'head', ...headSeed },
      { kind: 'person', personId: chiefPersonId },
      { purpose: 'contract fixture department' }
    )
    expect(
      impersonated,
      'an operator credential declaring a person requester must be refused, not obeyed'
    ).toMatchObject({ refused: 'requester-identity-mismatch' })

    const created = await client.staffing.createDepartment(
      companyKey,
      'engineering',
      ROOT_DEPARTMENT_ID,
      'Engineering',
      { kind: 'hire-new', personId: 'engineering-head', personKind: 'head', ...headSeed },
      { kind: 'operator' },
      { purpose: 'contract fixture department' }
    )
    expect(
      created,
      'with no route to inherit, an operator may create a unit and hire its head in one transaction'
    ).toMatchObject({ applied: true })

    // The hire transaction itself, on the same operator credential and against
    // the same live engine. Kept as its own call rather than folded into the
    // create above: `hirePerson` and `createDepartment` are two routes, and a
    // create that succeeded would say nothing about the one this suite is the
    // only real-daemon driver of.
    const hired = await client.staffing.hirePerson(
      companyKey,
      'engineering-worker',
      'engineering',
      { ...headSeed, name: 'Grace', title: 'Engineer', kind: 'worker' },
      { kind: 'operator' }
    )
    expect(hired, 'the operator hires with no route to attest').toMatchObject({ applied: true })

    const paused = await client.staffing.pauseDepartment(companyKey, 'engineering')
    expect(paused).toEqual({ applied: true })

    const resumed = await client.staffing.resumeDepartment(companyKey, 'engineering')
    expect(resumed).toEqual({ applied: true })
  })

  it('the root department refuses pause: exec-root-protected, as a value never thrown', async () => {
    const slug = 'chiefing-contract-staffing-root-protect'
    const { client, companyKey } = await bootGenesis(slug)

    const outcome = await client.staffing.pauseDepartment(companyKey, 'executive')
    expect(outcome).toMatchObject({ refused: expect.any(String), detail: expect.any(String) })
    expect(outcome).not.toHaveProperty('applied')
  })

  it('pauseDepartment on an unknown department id refuses AS A VALUE, never throws', async () => {
    const slug = 'chiefing-contract-staffing-refusal'
    const { client, companyKey } = await bootGenesis(slug)

    const outcome = await client.staffing.pauseDepartment(companyKey, 'does-not-exist')
    expect(outcome).toMatchObject({ refused: expect.any(String), detail: expect.any(String) })
  })

  it('appointDepartmentHead refuses when the successor is not a real person', async () => {
    const slug = 'chiefing-contract-staffing-appoint-refusal'
    const { client, companyKey } = await bootGenesis(slug)

    const outcome = await client.staffing.appointDepartmentHead(
      companyKey,
      'engineering',
      'not-a-person'
    )
    expect(outcome).toMatchObject({ refused: expect.any(String) })
  })
})
