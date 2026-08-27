// `@chief/chiefing` must never reach a browser bundle.
//
// Several server modules under `apps/web/src` import the chiefing barrel, and
// each is correct on its own terms: `helpers/OperatorChallenge.ts` uses the ONE
// implementation of the operator P-256 crypto in the monorepo (restating the
// domain tag or the signature math here would be a second implementation of a
// security primitive), and the `server/**` modules and the session route reach
// beacond and read chiefd's own wire types rather than compiling a second
// opinion about either. All of them are server-only — one of them handles the
// operator PRIVATE KEY, which the browser must never see.
//
// Nothing enforced that. The barrel pulls in transport, SSE and node-shaped
// code, so a single `'use client'` component importing it — directly, or by
// importing a helper that does — would ship all of it to the browser along
// with whatever the private-key path touches. The layering law
// (`web -> api -> chiefing`) says apps/web talks to apps/api, and these are the
// sanctioned exceptions; an exception that nothing checks is just a hole.
//
// The audit (#751/R9) offered "a browser-safe chiefing subpath OR a decision".
// This IS the decision, and the reasoning is not "cheaper", it is "correct":
//
//   1. A browser-safe subpath (the `extension-runtime` pattern) would have no
//      consumer. Every importer below is a server module, a route handler, or
//      an env constant read on the server; nothing in a browser bundle wants
//      chiefing, today or in any planned shape. Building an export surface for
//      an importer that does not exist is the speculative abstraction the
//      root CLAUDE.md forbids, and an unused subpath rots — it drifts out of
//      sync with the barrel precisely because nothing exercises it.
//   2. A subpath would not even close the hole. It offers a safe door; it does
//      not lock the unsafe one. A `'use client'` component importing the
//      BARREL is exactly as broken with a subpath in the package as without,
//      because nothing stops it. The fence below does stop it — a browser
//      import is a failing test, which is the property the audit actually
//      wanted.
//   3. The list is deliberately hand-maintained, not derived. A new importer
//      is a layering decision; deriving the list would make it a formality.
//
// Revisit if a browser component ever genuinely needs a chiefing type or
// constant. Then the subpath has a consumer and earns its existence.
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

const WEB_SRC = join(import.meta.dirname, '..', 'src')

/** Every `.ts`/`.tsx` file under `apps/web/src`. */
function sourceFiles(dir: string, found: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) {
      sourceFiles(full, found)
      continue
    }
    if (entry.endsWith('.ts') || entry.endsWith('.tsx')) found.push(full)
  }
  return found
}

function importsChiefing(source: string): boolean {
  return /from\s+'@chief\/chiefing/.test(source) || /from\s+"@chief\/chiefing/.test(source)
}

function isClientComponent(source: string): boolean {
  // The directive must be the first statement, so it lands in the opening lines.
  return /^\s*['"]use client['"]/m.test(source.split('\n').slice(0, 5).join('\n'))
}

describe('@chief/chiefing stays out of the browser bundle', () => {
  const files = sourceFiles(WEB_SRC)

  it('scans a non-empty tree', () => {
    // A file-walk guard that silently found nothing would pass forever.
    expect(files.length).toBeGreaterThan(20)
  })

  it('is imported by exactly the server-only helper, and nothing else', () => {
    const importers = files
      .filter((file) => importsChiefing(readFileSync(file, 'utf8')))
      .map((file) => file.slice(WEB_SRC.length + 1).replaceAll('\\', '/'))

    // A new importer is not automatically wrong, but it is a layering decision
    // that must be made deliberately, not by autocomplete. The sanctioned
    // server-only importers, all of them server modules:
    //   - the session route handler, which resolves a company's chiefd through
    //     beacond (`DiscoveryClient`) since apps/api no longer does it for us;
    //   - `common/Env.ts`, which takes beacond's default URL from chiefing
    //     rather than compiling a second copy of the port number;
    //   - the operator P-256 signer;
    //   - `server/CompanyChiefd.ts`, which resolves a company's daemon through
    //     beacond for every route handler that touches a company;
    //   - `server/CompanyDirectory.ts`, which asks beacond which companies
    //     exist before probing each daemon for whether it is actually up;
    //   - `server/HostedRoster.ts`, which reads chiefd's launch profile — the
    //     wire type is chiefd's, and typing it locally would be the second
    //     opinion this whole layer exists to avoid.
    // Any NEW importer is a decision, not autocomplete — especially one under
    // `components/` or `providers/`, which reach the browser.
    expect(importers.sort()).toEqual([
      'app/api/session/route.ts',
      'common/Env.ts',
      'helpers/OperatorChallenge.ts',
      'server/CompanyChiefd.ts',
      'server/CompanyDirectory.ts',
      // `server/CompanyLifecycle.ts`, which boots and stops a company through
      // `chief host`'s own resident lifecycle surface (`CompanyLifecycleClient`)
      // and recognises its refusal by class (`CompanyLifecycleRefusalError`).
      // Both are chiefd's; a local re-spelling of either would be a second
      // vocabulary for the daemon's own answer.
      'server/CompanyLifecycle.ts',
      // `server/Goals.ts`, which projects chiefd's own `GoalsBoard` wire types
      // for the 🎯 rail. Type-only, and typing the board locally would be the
      // second opinion this layer exists to avoid.
      'server/HostedRoster.ts',
      // `server/RouteResult.ts`, the ONE place a chiefd refusal becomes an
      // HTTP status. It imports chiefing's error classes so the mapping is an
      // `instanceof` rather than a string match on a message chiefing's own
      // module says is not part of the contract. It deliberately imports
      // nothing from the agent runtime — see its own header.
      'server/RouteResult.ts',
      // `server/Staffing.ts`. It still types its own applied-only OUTCOMES in
      // `types/Staffing.ts` — a refused verb is thrown, so there is no refusal
      // arm to borrow a type for. What it borrows is `CompanyTreeDepartment`,
      // the shape of `/v1/org/tree/structured`, which it reads to find the
      // department head who attests a department create. That is chiefd's own
      // projection; re-spelling it here would be the second opinion this layer
      // exists to avoid, and the import is type-only.
      'server/Staffing.ts'
    ])
  })

  it('is never reached from a "use client" component', () => {
    const offenders = files
      .filter((file) => {
        const source = readFileSync(file, 'utf8')
        return isClientComponent(source) && importsChiefing(source)
      })
      .map((file) => file.slice(WEB_SRC.length + 1))

    expect(
      offenders,
      `client components must not import chiefing: ${offenders.join(', ')}`
    ).toEqual([])
  })

  it('keeps the operator private-key helper out of client components', () => {
    const offenders = files
      .filter((file) => {
        const source = readFileSync(file, 'utf8')
        return isClientComponent(source) && /OperatorChallenge/.test(source)
      })
      .map((file) => file.slice(WEB_SRC.length + 1))

    expect(
      offenders,
      `the operator key flow must stay server-side: ${offenders.join(', ')}`
    ).toEqual([])
  })
})
