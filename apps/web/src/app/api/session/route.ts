/**
 * POST /api/session — the Next route handler that holds the operator P-256
 * private key server-side (Node runtime; env, never inlined to the client
 * bundle). No key configured → `{ token: null }`: auth-off mode, matching a
 * chiefd whose identity enforcement has not been enabled. This is a
 * configuration state, not a fallback — there is no negotiation and no second
 * client.
 *
 * # Which chiefd mints the token
 *
 * apps/api used to be the single auth endpoint. It is deleted, and identities
 * live in each company's own chiefd, so a token is minted BY a company's
 * daemon and is only good there — with that company's OWN operator key, which
 * its daemon minted inside its own directory. The caller therefore names the
 * company by key and this route resolves it through beacond; a request with no
 * company is a refusal, never a guess at which company was meant.
 */
import { DiscoveryClient } from '@chief/chiefing'
import { NextResponse } from 'next/server'

import { beacondUrl, operatorIdentityId, operatorPrivateKeyPem } from '@/common/Env'
import { acquireOperatorToken } from '@/helpers/OperatorChallenge'
import { isNullish } from '@/utils/Nullish'

export const runtime = 'nodejs'

export async function POST(request: Request): Promise<Response> {
  const identityId = operatorIdentityId()

  const companyKey = new URL(request.url).searchParams.get('company')
  if (isNullish(companyKey) || companyKey.trim() === '') {
    return NextResponse.json(
      {
        token: null,
        identityId,
        error: 'this route needs ?company=<key> to know which chiefd should mint the token'
      },
      { status: 400 }
    )
  }

  try {
    // Matched on the registry LIST by key. `lookup` takes the company's
    // directory, which only a process standing in that directory knows; a
    // server minting a token for an operator does not stand anywhere.
    const rows = await new DiscoveryClient({ beacondUrl: beacondUrl() }).list()
    const chiefd = rows.find((row) => row.key === companyKey)
    if (isNullish(chiefd)) {
      return NextResponse.json(
        {
          token: null,
          identityId,
          error: `no company is registered under key "${companyKey}"`
        },
        { status: 404 }
      )
    }
    // The key is read from THAT company's own directory. No key there is
    // auth-off mode, matching a chiefd whose identity enforcement has not been
    // enabled — a configuration state, not a fallback.
    const privateKeyPem = operatorPrivateKeyPem(chiefd.dir)
    if (!privateKeyPem) {
      return NextResponse.json({ token: null, identityId })
    }
    // A registered company that is not RUNNING has no url — beacond knows it
    // exists, but nothing is listening to mint anything. That is a distinct
    // answer from "unknown company", and it tells the operator what to do.
    if (isNullish(chiefd.url)) {
      return NextResponse.json(
        {
          token: null,
          identityId,
          error:
            `company "${chiefd.slug}" in ${chiefd.dir} is registered but not running, ` +
            `so no chiefd can mint a token — start it by running chief in that directory`
        },
        { status: 409 }
      )
    }
    const token = await acquireOperatorToken({
      apiUrl: chiefd.url,
      identityId,
      privateKeyPem
    })
    return NextResponse.json({ token, identityId })
  } catch (error) {
    return NextResponse.json(
      {
        error: {
          code: 'auth-upstream',
          detail: error instanceof Error ? error.message : String(error)
        }
      },
      { status: 502 }
    )
  }
}
