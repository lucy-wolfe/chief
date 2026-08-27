/**
 * GET /api/companies/:companyKey/tree — the company as a forest.
 *
 * A thin pass-through to chiefd's `/v1/org/tree/structured`. It resolves which
 * daemon owns the companyKey and forwards the answer; it does NOT build the tree.
 * That projection is chiefd's (mandate 3), and building it here would recreate
 * exactly what the deleted apps/api did.
 */
import { NextResponse } from 'next/server'

import { companyChiefd, CompanyUnavailableError } from '@/server/CompanyChiefd'

export const runtime = 'nodejs'

export async function GET(
  _request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  try {
    const chiefd = await companyChiefd(companyKey)
    return NextResponse.json(await chiefd.orgSlice.treeStructured(companyKey))
  } catch (error) {
    if (error instanceof CompanyUnavailableError) {
      return NextResponse.json(
        { error: { code: error.code, detail: error.message } },
        { status: error.status }
      )
    }
    return NextResponse.json(
      {
        error: {
          code: 'upstream-unreachable',
          detail: error instanceof Error ? error.message : String(error)
        }
      },
      { status: 502 }
    )
  }
}
