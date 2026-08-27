/**
 * Server-only `process.env` access for apps/web (Next.js server runtime: route
 * handlers, server components, the auth session route). This is the ONLY
 * module — besides `PublicEnv.ts` for the build-time-inlined client half —
 * permitted to touch `process.env` (`lucy/no-process-env` allows it under
 * `src/common/`); every other module consumes these typed getters.
 *
 * # One address, and it is beacond's
 *
 * apps/web used to hold exactly one service address: apps/api's. apps/api is
 * DELETED, and with it the layer that resolved companies on the browser's
 * behalf. This server does that itself now: it asks beacond — at a STATIC,
 * well-known port, which is the whole reason beacond has one — for a company's
 * chiefd URL, and then talks to that chiefd directly through
 * `@chief/chiefing`.
 *
 * There is still deliberately no chiefd variable here. Where a company's
 * chiefd runs is beacond's answer, never configuration: a company's daemon is
 * started per company and its port is allocated at genesis, so any static
 * chiefd address would be a guess that goes stale the first time a company
 * restarts.
 */
// The default is chiefing's own constant rather than a second copy of the port
// number: beacond's address is compiled in exactly once, in chiefing, and
// every client — this one included — derives it from there.
import { join } from 'node:path'

import {
  chiefdHostUrlFromEnvironment,
  DEFAULT_BEACOND_URL,
  operatorKeyPath,
  readIdentityKeyPem
} from '@chief/chiefing'

/**
 * The one enrolled operator identity. A LITERAL and not a variable: the daemon
 * hardcodes this exact id in `enroll_bootstrap_operator`, and `identity_keys`
 * writes it down once for both Rust halves. A configurable id here could only
 * ever name an identity nothing enrols.
 */
const OPERATOR_IDENTITY_ID = 'operator'

/** beacond's static discovery address, as seen by this Next server. */
export function beacondUrl(): string {
  return process.env.BEACOND_URL ?? DEFAULT_BEACOND_URL
}

// `chiefdDataRoot()` lived here and answered `$HOME/.chiefd` — the box-wide
// tree every company hung under. There is no such tree: a company is the
// directory the operator ran `chief` in, and everything chiefd owns for it
// lives in that directory's own `.chief` folder. Nothing about a company is
// derivable from `$HOME` any more, so nothing derives it.

/**
 * Operator P-256 private key (PKCS#8 PEM), used server-side to sign the auth
 * challenge. `undefined` when this box has no operator key yet — the daemon
 * mints it at boot, so a box that has never run one legitimately has none.
 *
 * # It is DERIVED, and `CHIEF_WEB_OPERATOR_PRIVATE_KEY` is deleted
 *
 * That variable was set by nothing in the tree, exactly like the
 * `CHIEFD_OPERATOR_KEY_PATH` A1 deleted on the Rust side, so apps/web
 * authenticated as nobody in every deployment this repository can produce. A
 * trust anchor that is unset by default is an off switch wearing a
 * configuration flag's clothes. The key is the same file the daemon mints and
 * the `chiefd` CLI signs with, and apps/web's co-location with the daemon is
 * the operator's own ruling (§5.4 ruling 2).
 *
 * # It is PER COMPANY, and takes that company's directory
 *
 * There was one operator key per BOX, at `$HOME/.chiefd/keys/operator.key`,
 * because there was one box-wide company tree. A company is a directory now
 * and its daemon mints its keys inside it, so the caller must say which
 * company it is authenticating to — the same directory beacond's row carries.
 *
 * A group- or world-readable key reads as ABSENT here, because
 * `readIdentityKeyPem` refuses it. That is the same rule the daemon and the
 * CLI apply to the same file.
 */
export function operatorPrivateKeyPem(companyDir: string): string | undefined {
  return readIdentityKeyPem(operatorKeyPath(join(companyDir, '.chief')))
}

/** Identity id sent to `POST /v1/auth/challenge`. */
export function operatorIdentityId(): string {
  return OPERATOR_IDENTITY_ID
}

/**
 * The resident `chief host` lifecycle surface for this box.
 *
 * Validated through chiefing's own reader rather than read raw, so a malformed
 * `CHIEFD_HOST_URL` falls back to the compiled-in address instead of being
 * passed through as a broken one. Like beacond's, this address is
 * configuration rather than a discovery answer: a per-box service cannot be
 * found through the registry it is used to populate.
 */
export function chiefdHostUrl(): string {
  return chiefdHostUrlFromEnvironment(process.env)
}

/**
 * This server's own build mode, for the one place a hosted agent's shell
 * environment needs it.
 *
 * Next's type augmentation makes `NODE_ENV` required on `ProcessEnv`, so a
 * shell environment assembled from chiefd's per-person overlay has to carry
 * it. It describes the running SERVER, not the person — it is the only
 * ambient value that reaches a hosted agent's shell.
 */
export function nodeEnv(): 'development' | 'production' | 'test' {
  return process.env.NODE_ENV
}

/**
 * The operator's own Pi agent directory.
 *
 * `PI_SOURCE_AGENT_DIR` first, then `$HOME/.pi/agent` — the two tiers Pi itself
 * uses, and the two `chief-cli`'s `root_pi_agent_dir` uses. A third answer here
 * would let chiefd and this server disagree about which Pi describes the box.
 */
export function operatorPiAgentDir(): string {
  const configured = trimmed(process.env.PI_SOURCE_AGENT_DIR)
  if (typeof configured === 'string') return configured
  return join(trimmed(process.env.HOME) ?? '', '.pi', 'agent')
}

/** A value with surrounding space removed, or nothing when it is blank. */
function trimmed(value: string | undefined): string | undefined {
  const text = value?.trim()
  return typeof text === 'string' && text !== '' ? text : undefined
}
