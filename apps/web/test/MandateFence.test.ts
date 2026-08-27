/**
 * Ruling D18 — the conformance fence. Named to match `apps/cli/test/
 * ImportFences.test.ts` (E4-S2) and `apps/api/test/MandateFence.test.ts`
 * (E5-S1) so a reviewer recognises it on sight.
 *
 * Walks every file under `apps/web/src/` FROM DISK (never imported — an
 * import-based scan only sees what already compiles, and a `setInterval`
 * hidden behind a type error would silently pass) and asserts apps/web makes
 * no durable-state decision locally. Data-driven: S2-S7 extend this file by
 * adding rows to `FENCE_RULES` / `RESTRICTED_RULES`, never by rewriting the
 * walker.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const srcRoot = join(here, '..', 'src')

interface SourceFile {
  relativePath: string
  contents: string
}

function walk(dir: string): string[] {
  const entries = readdirSync(dir, { withFileTypes: true })
  const files: string[] = []
  for (const entry of entries) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) {
      files.push(...walk(full))
    } else if (entry.isFile()) {
      files.push(full)
    }
  }
  return files
}

function readSourceFiles(): SourceFile[] {
  const stat = statSync(srcRoot, { throwIfNoEntry: false })
  if (!stat || !stat.isDirectory()) {
    throw new Error(`[MandateFence] apps/web/src is missing — refusing to pass on an empty scan`)
  }
  return walk(srcRoot)
    .filter((path) => path.endsWith('.ts') || path.endsWith('.tsx'))
    .map((path) => ({
      relativePath: relative(srcRoot, path),
      contents: readFileSync(path, 'utf8')
    }))
}

interface FenceRule {
  name: string
  pattern: RegExp
}

// Rules that must never match anywhere under src/.
const FENCE_RULES: FenceRule[] = [
  { name: 'no localStorage (mandate 2)', pattern: /\blocalStorage\b/ },
  { name: 'no sessionStorage (mandate 2)', pattern: /\bsessionStorage\b/ },
  { name: 'no indexedDB (mandate 2)', pattern: /\bindexedDB\b/ },
  { name: 'no document.cookie (mandate 2)', pattern: /document\.cookie/ },
  { name: 'no new EventSource (mandate 1)', pattern: /new\s+EventSource\s*\(/ },

  {
    name: 'no sqlite/database mutation literal (ruling D19)',
    pattern: /\b(sqlite|INSERT INTO|UPDATE\s+\w+\s+SET)\b/i
  },
  {
    name: 'no lock/lease/mutex/semaphore primitive (ruling D19)',
    pattern: /\b(mutex|semaphore|lockfile|acquireLock|advisoryLock)\b/i
  },
  {
    // The WRITE primitives, banned everywhere including the server. apps/web
    // owns no durable state: everything it knows comes from chiefd, and a file
    // written here would be state chiefd cannot see (mandates 2 and 5).
    name: 'no server-side disk write (ruling D20)',
    pattern: /\bBun\.write\s*\(|\bwriteFileSync\s*\(|\bwriteFile\s*\(|\bappendFile\s*\(/
  },
  {
    // E2-S3 (#807): the operator challenge domain tag and the P-256 signing
    // math have exactly one implementation, in @chief/chiefing. This file
    // must import authChallengeMessage/signAuthChallenge, never restate the
    // domain-separation tag or reimplement the signature math locally.
    name: 'no copied challenge crypto (ruling D0 — one signer)',
    pattern: /chiefd-auth-v1|prime256v1|subtle\.sign/
  },
  {
    name: 'no localStorage/document.cookie write of the access token (mandate 2)',
    pattern: /\.setItem\s*\(\s*['"](token|accessToken|jwt)['"]/i
  },
  {
    // streaming fetch can authenticate with a header, so a credential never
    // appears in URLs, referrers, or server logs.
    name: 'no access token query parameter (E6-S4)',
    pattern: /accessToken=/
  },
  {
    // E6-S7 (#812), the company view MVP: apps/api's `GET
    // /companies/:companyKey/tree` is already ordered and placement-resolved
    // server-side (E5-S4). The web never reads chiefd's internal activity
    // ledger and never reconstructs pane placement from it.
    name: 'no chiefd activity-ledger read or local placement derivation (E6-S7, ruling D0/mandate 3)',
    pattern: /\bgetActivity\b|\bactivityLedger\b|\bpaneDepartmentId\b/
  }
]

// Rules that may match ONLY inside files whose path includes one of
// `allowedSegments` (a substring match against the file's path relative to
// src/ — e.g. 'services/' matches any file under a services/ directory,
// 'helpers/OperatorChallenge.ts' matches that one file exactly).
interface RestrictedRule {
  name: string
  pattern: RegExp
  allowedSegments: readonly string[]
}

const RESTRICTED_RULES: RestrictedRule[] = [
  // A repeating timer is banned because POLLING is banned — asking again for
  // state nobody said had changed. `server/PersonStream.ts` is the single
  // exception, and it is not a poll: the callback writes one constant SSE
  // comment (`: beat`), reads nothing, and asks nothing.
  //
  // The bytes are not optional. `SseClientService` fails a connection that has
  // been silent for 45s and reconnects with backoff, so a stream carrying only
  // real events tears down and re-subscribes every 45 seconds while an agent
  // thinks — a reconnect storm dressed up as reactivity. Nothing distinguishes
  // a dead socket from a thinking agent except traffic; that is why SSE has
  // comment frames.
  //
  // Scoped rather than deleted, and scoped rather than widened: any OTHER file
  // that wants a repeating timer is doing the thing mandate 1 forbids.
  {
    name: 'setInterval only in the SSE writer (mandate 1)',
    pattern: /\bsetInterval\b/,
    allowedSegments: ['server/PersonStream.ts']
  },
  // Reading from disk is confined to `src/server/`, and the split from the
  // write ban above is deliberate. apps/web must READ the operator's own Pi
  // configuration — `auth.json` and `models.json` in `~/.pi/agent` — because
  // hosting the Pi harness in-process moved that job here from the subprocess
  // that used to do it. They are read THERE and not in a person's home:
  // chiefd stopped redirecting `PI_CODING_AGENT_DIR`, so a home holds no
  // credential and no provider registry at all. It must still WRITE nothing:
  // the write primitives stay banned everywhere, including the server.
  //
  // The previous rule banned `node:fs` outright under a name that said
  // "write", so it caught a read-only module and would have been widened by
  // whoever hit it next — losing the write ban along with the read ban.
  {
    name: 'node:fs read only in src/server/ (ruling D20)',
    pattern: /\bnode:fs\b/,
    allowedSegments: ['server/']
  },
  // #751/P3: importing Pi directly used to be banned outright, because apps/api
  // hosted every agent and apps/web only ever rendered what it was told. apps/api
  // is deleted and this server hosts the harnesses itself, so the ban is now
  // about the BROWSER — a Pi harness shipped to a page would carry the agent's
  // whole runtime, and its credentials, to the client.
  //
  // `types/AgentTools.ts` joins `server/` for ONE `import type`, and the rule
  // is about the BROWSER BUNDLE: a type-only import is erased at compile time,
  // so it ships no runtime, no credentials and no bytes. A VALUE import in any
  // of these files would be a real violation and is still caught, because the
  // pattern matches the statement and each allowance is scoped to one file.
  // (`types/AgentHost.ts` used to be on this list for Pi's `ThinkingLevel`;
  // chief holds no thinking level for anybody now, so the file imports nothing
  // from Pi and its row went with the field.)
  //
  // The reason for `types/AgentTools.ts` in particular:
  // `ToolSelection` names Pi's `AgentTool` in one `import type`,
  // because the tools a hosted person gets ARE Pi's own — a locally restated
  // shape would drift from the objects the harness is actually built with.
  // `lucy/no-exported-type-outside-types-dir` requires the interface to live
  // in `types/`, so the type-only allowance has to follow it there.
  //
  // `types/ExtensionTools.ts` joins on the same terms again, and for the same
  // reason one level further in: an `ExtensionInstaller` is a function OF Pi's
  // `ExtensionAPI`, and an `ExtensionToolSet` names Pi's `AgentTool` and
  // `AgentHarness`. All three are `import type` and all three are the real
  // shapes the adapter hands the harness — restating them locally is how the
  // web host would come to hold its own idea of what a tool is, which is the
  // second source of truth the adapter exists to prevent.
  //
  // `types/ExtensionLifecycle.ts` joins for the same allowance and the
  // strictest case of it: a `LifecycleSubject` IS Pi's `AgentHarness` and its
  // `Session`, and a `HostedLifecycle` hands out Pi's own `ExtensionContext`.
  // Every one is `import type`. A locally restated context is precisely the
  // failure the driver exists to end — the host would be answering an
  // extension's questions about a session shape it had invented rather than
  // the one Pi defines.
  //
  // `types/OperatorPi.ts` is the last of them, and the shortest argument of
  // all: an `OperatorRoute` IS Pi's own `Models` catalog and one `Model` out
  // of it. Chief chooses no route, so the type cannot be anything other than
  // Pi's — a locally restated one would be this server describing a catalog it
  // does not build. Both are `import type`.
  {
    name: 'Pi imported only in src/server/ (E3 interface)',
    pattern: /from\s+['"](@earendil-works\/|@chief\/piing)/,
    allowedSegments: [
      'server/',
      'types/AgentTools.ts',
      'types/ExtensionLifecycle.ts',
      'types/ExtensionTools.ts',
      'types/OperatorPi.ts'
    ]
  },
  // #751/P3: constructing a chiefd client used to be banned outright — apps/api
  // held every one, and apps/web only ever talked to apps/api. apps/api is
  // deleted, so THIS SERVER constructs them; the ban is now about the browser,
  // which must never hold a company's daemon address at all.
  //
  // `server/` is the one place allowed to build one, deliberately narrower than
  // "any server module": a route handler that constructs its own client is a
  // route handler that resolved an address its own way, and one resolver is
  // what keeps every handler agreeing about where a company lives.
  {
    name: 'chiefd client constructed only in src/server/ (mandate 3)',
    pattern: /\bnew\s+(DocsClient|ChiefdClient|RowStoresClient|StaffingClient|LocksClient)\b/,
    allowedSegments: ['server/']
  },
  // #751/P0: these three used to be blanket bans — apps/web held exactly one
  // address (apps/api's) and never learned where a chiefd was. apps/api is
  // DELETED, so this Next SERVER resolves companies through beacond and calls
  // each company's chiefd itself; the ban is now about the BROWSER BUNDLE,
  // which is what it was always protecting.
  //
  // A company's daemon is started per company on a port allocated at genesis,
  // so an address shipped to a browser is a guess that goes stale the moment
  // that company restarts — and it would make every company's daemon directly
  // reachable from the page. Server modules (`common/` for env, `app/api/` for
  // route handlers) are the only places allowed to know.
  // #751/P3: `server/` joins the list, for the same reason it joined the
  // `fetch(` and `/v1/` rules and none of its own. The rule's NAME already
  // says "server modules"; the list predates `src/server/` existing at all,
  // and `src/server/AgentHost.ts` names `ORG_CHIEFD_URL` because it hands each
  // person the launch environment chiefd materialized for THEM — the one place
  // that variable legitimately appears outside `common/`. The ban is, and
  // always was, about the BROWSER holding a company daemon's address.
  {
    name: 'chiefd/beacond env var only in server modules (ruling D1/D2)',
    pattern: /\b(ORG_CHIEFD_URL|CHIEFD_URL|BEACOND_URL)\b/,
    allowedSegments: ['common/', 'app/api/', 'server/']
  },
  {
    name: 'chiefd/beacond port literal only in server modules (ruling D1/D2)',
    pattern: /\b(6969|8792)\b/,
    allowedSegments: ['common/', 'app/api/']
  },
  {
    name: '/v1/ chiefd path literal only in server modules (ruling D1/D2)',
    pattern: /['"]\/v1\//,
    // #751/P3: `server/` joins the list for the same reason it joined the
    // fetch rule — a server module reaching chiefd is where a chiefd path
    // belongs, and the ban is about the BROWSER holding one.
    // `helpers/OperatorChallenge.ts` is BACK on this list (A2), and the reason
    // it left is the defect A2 fixed: it POSTed to `/auth/challenge` and
    // `/auth/token`, paths written when `apiUrl` meant the deleted apps/api and
    // carried the version prefix already. `apiUrl` is now a company daemon's
    // bare origin, where the routes are `/v1/auth/*`, so every call was a 404.
    // It names both `/v1/` literals now, which is what puts it back here.
    allowedSegments: ['common/', 'app/api/', 'server/', 'helpers/OperatorChallenge.ts']
  },
  // `common/` stays the ONE reader — that is the whole rule, and widening this
  // to `server/` would end it. `server/AgentHost.ts` is listed BY NAME because
  // it is the one module whose job it is to explain why it does not read the
  // environment: it builds a per-person `shellEnv` from chiefd's profile and
  // deliberately never exports it to this process, since the env is per person
  // and the process is shared. Naming `process.env` to say "not this" is the
  // only mention there, and a file-scoped allowance keeps every OTHER server
  // module fenced.
  //
  // `server/AgentTools.ts` used to be listed here on the same terms — it named
  // `process.env.ORG_CHIEFD_URL` in prose to record why the `org_*` family
  // could not be built in this process. It no longer names it at all, so the
  // row was exempting an occurrence that does not exist: a false fact about
  // the code, and a silent widening the next time a REAL read appears in that
  // file. Deleted, and the staleness check below is why it can never sit here
  // unnoticed again.
  {
    name: 'process.env only in src/common/ (centralized env)',
    pattern: /process\.env/,
    allowedSegments: ['common/', 'server/AgentHost.ts']
  },
  {
    // `helpers/OperatorChallenge.ts` was the one other sanctioned caller — it
    // POSTs to /auth/challenge and /auth/token from the Next route handler,
    // before any ChiefApiClientService instance exists to hold a token (#807).
    // It no longer calls `fetch(` at all: it takes an injected `FetchImpl` and
    // defaults it, which is the seam this rule wanted in the first place. The
    // row expired with that refactor and is deleted rather than carried.
    name: 'fetch( only in src/services/ or src/server/ (network seams)',
    pattern: /\bfetch\s*\(/,
    // #751/P3: `server/` joins the list. The rule is about the BROWSER's
    // network seam — a component that fetches for itself bypasses the client's
    // auth, error taxonomy and retry. A server module has no such client to
    // bypass: it IS the thing the browser fetches, and reaching chiefd is its
    // whole job.
    allowedSegments: ['services/', 'server/']
  },
  // #751/P3: `doc-change` is chiefd's frame name, and it was banned outright
  // when apps/api sat in between and re-spelled it. apps/api is deleted, so
  // this server is now the ONE place that adaptation happens — and it must
  // name the frame it is adapting FROM. Everything the browser sees is still
  // the page's spelling; a `doc-change` outside `server/` means somebody
  // taught the browser chiefd's wire.
  {
    name: 'chiefd frame names only in src/server/ (E6-S4, re-scoped)',
    pattern: /doc-change/,
    allowedSegments: ['server/']
  }
]

// ── the allowlists check themselves ─────────────────────────────────────────
//
// #963's class, at the surface that produces it most: a `RESTRICTED_RULES`
// row's `allowedSegments` are a hand-maintained exemption list, and an
// exemption for an occurrence that no longer exists is not harmless. It tells
// every reader a false fact about the code, and it silently widens the day a
// REAL occurrence appears at that path — which is exactly how a row orphaned
// by a file move stayed invisible until batch assembly and was then
// misattributed to an unrelated change.
//
// Three rows were live examples when this was written, and all three are
// deleted above: `server/AgentTools.ts` had stopped naming `process.env`, and
// `helpers/OperatorChallenge.ts` had stopped naming both a `/v1/` path and a
// literal `fetch(` when it moved to an injected `FetchImpl`.
//
// THE TWO KINDS OF SEGMENT ARE NOT THE SAME CLAIM, and collapsing them would
// make this check wrong rather than stricter:
//
//   A FILE segment (`server/AgentHost.ts`) is a RECORD. It names one module
//   and is justified in prose by one occurrence in it. If that occurrence is
//   gone, the record is false — fail, by name, and say "delete it".
//
//   A DIRECTORY segment (`common/`, `server/`, `services/`) is a POLICY:
//   "this is where the thing belongs". A policy is legitimately empty; there
//   is no `/v1/` literal under `common/` today and there does not need to be
//   for `common/` to be the right place for one. Demanding an occurrence
//   would silently narrow rules whose own NAMES state the policy, and would
//   turn deleting the last instance of a sanctioned pattern into a fence
//   failure. What a directory row IS checked for is existence: a segment
//   naming a directory that is not there is orphaned exactly the way a file
//   row is, and that is #963 verbatim.
function isFileScoped(segment: string): boolean {
  return segment.endsWith('.ts') || segment.endsWith('.tsx')
}

/** Every file-scoped allowance that no longer matches the rule it exempts,
 *  as `<rule name> -> <segment>`. Empty is the passing answer. */
function staleFileScopedAllowances(files: SourceFile[], rules: RestrictedRule[]): string[] {
  const stale: string[] = []
  for (const rule of rules) {
    for (const segment of rule.allowedSegments.filter(isFileScoped)) {
      const matched = files.some(
        (file) => file.relativePath.includes(segment) && rule.pattern.test(file.contents)
      )
      if (!matched) stale.push(`${rule.name} -> ${segment}`)
    }
  }
  return stale
}

/** Every allowed segment — file OR directory — that names nothing on disk
 *  under `src/`. */
function missingAllowedPaths(rules: RestrictedRule[]): string[] {
  const missing: string[] = []
  for (const rule of rules) {
    for (const segment of rule.allowedSegments) {
      const stat = statSync(join(srcRoot, segment), { throwIfNoEntry: false })
      if (!stat) missing.push(`${rule.name} -> ${segment}`)
    }
  }
  return missing
}

describe('MandateFence', () => {
  const files = readSourceFiles()

  it('scanned at least one source file (a fence over nothing passes everything)', () => {
    expect(files.length).toBeGreaterThan(0)
  })

  for (const rule of FENCE_RULES) {
    it(rule.name, () => {
      const offenders = files.filter((file) => rule.pattern.test(file.contents))
      expect(offenders.map((file) => file.relativePath)).toEqual([])
    })
  }

  for (const rule of RESTRICTED_RULES) {
    it(rule.name, () => {
      const offenders = files.filter(
        (file) =>
          rule.pattern.test(file.contents) &&
          !rule.allowedSegments.some((segment) => file.relativePath.includes(segment))
      )
      expect(offenders.map((file) => file.relativePath)).toEqual([])
    })
  }

  it('every file-scoped allowance still exempts a real occurrence', () => {
    expect(staleFileScopedAllowances(files, RESTRICTED_RULES)).toEqual([])
  })

  it('every allowed path still exists under src/', () => {
    expect(missingAllowedPaths(RESTRICTED_RULES)).toEqual([])
  })

  it('the staleness check is real: a row for a path with no occurrence is named, a live row is not', () => {
    // Both arms run against the REAL scanned tree, so neither can pass by
    // reading a fixture the production rules never touch. The live row is
    // `server/AgentHost.ts` under the `process.env` rule — the one file-scoped
    // allowance that survived the deletions above, and the exact shape a
    // checker that simply reported everything would get wrong.
    const live: RestrictedRule = {
      name: 'probe (live)',
      pattern: /process\.env/,
      allowedSegments: ['server/AgentHost.ts']
    }
    expect(staleFileScopedAllowances(files, [live])).toEqual([])

    const stale: RestrictedRule = {
      name: 'probe (stale)',
      pattern: /process\.env/,
      allowedSegments: ['server/CompanyChiefd.ts']
    }
    expect(staleFileScopedAllowances(files, [stale])).toEqual([
      'probe (stale) -> server/CompanyChiefd.ts'
    ])

    const moved: RestrictedRule = {
      name: 'probe (moved)',
      pattern: /process\.env/,
      allowedSegments: ['server/AFileThatWasMovedAway.ts']
    }
    expect(missingAllowedPaths([moved])).toEqual([
      'probe (moved) -> server/AFileThatWasMovedAway.ts'
    ])
    expect(missingAllowedPaths([live])).toEqual([])
  })
})
