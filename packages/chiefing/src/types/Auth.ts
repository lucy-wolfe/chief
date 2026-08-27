// Types for AuthClient and the Identity helpers.
//
// Source: src/organization/agent-jwt.ts:23-32 (challenge/token, previously
// module-private — exported here per the Contract), agent-enroll.ts:24-33,
// agent-identity.ts:49-55. Wire shapes re-verified against
// apps/chiefd/crates/chiefd-api/src/authn/routes.rs.

export interface AgentKeypair {
  publicSpkiBase64: string
  privatePkcs8Pem: string
}

/** Rust authority: apps/chiefd/crates/chiefd-api/src/authn/routes.rs — the
 * `POST /v1/auth/challenge` response. `identityId` -> `{nonceId, nonce}`. */
export interface ChallengeResponse {
  nonceId: string
  nonce: string
}

/** Rust authority: authn/routes.rs — the `POST /v1/auth/token` response.
 * `{nonceId, signature}` -> `{token}`. */
export interface TokenResponse {
  token: string
}

/**
 * Why a pi-home has no usable identity key (A4).
 *
 * Three reasons that used to be one `undefined`. They are not the same fact
 * and must not be reported as one: `absent` is a real, benign, DOCUMENTED
 * state (a home written before #751/P7, or a person whose materialization was
 * contained) that nobody should be paged about, while `permissive-mode` is a
 * key that EXISTS and is being refused — a support incident an operator fixes
 * in one `chmod`, and one that was otherwise completely invisible from the
 * pane. Collapsing the two is what made the strict half silent.
 */
export type AgentKeyRefusalReason = 'absent' | 'permissive-mode' | 'unreadable'

export interface AgentKeyRefusal {
  readonly reason: AgentKeyRefusalReason
  /** The file the refusal is about, so a report can name it. */
  readonly keyPath: string
  /** The permission bits found, present only for `permissive-mode` — a report
   * that says `0644` is actionable in a way that "bad mode" is not. */
  readonly mode?: number
}

/** Either the pane's keypair, or exactly why it does not have one. */
export type AgentKeypairRead =
  | { readonly keypair: AgentKeypair; readonly refusal?: undefined }
  | { readonly keypair?: undefined; readonly refusal: AgentKeyRefusal }

/**
 * The daemon a pane speaks to, as the person the pane belongs to.
 *
 * All three are CARRIED, never resolved ambiently. A single module-level slot
 * is the defect the daemon address already had: the last install in the
 * process wins, so a host running several companies — or several people of one
 * company, who share a URL and hold different keys — signs every call after
 * the second one as somebody else. chiefd answers a valid credential for the
 * wrong person with a refusal naming that person, so the symptom points away
 * from the cause. A credential is the last thing that should be resolved
 * ambiently.
 */
export interface PaneIdentity {
  /** The daemon this company answers on. */
  readonly url: string
  /** The acting person — whose pi-home identity key signs this call. */
  readonly personId: string
  /** The COMPANY DIRECTORY; where that person's own agent folder is found. */
  readonly organizationDir: string
  /** Exact directory holding `chiefd-identity.key.pem`. The Chief uses
   * `<dir>/.chief`; an agent uses its own `.chief/agent/<id>` directory. */
  readonly identityDir: string
}

/** How a caller learns that a key EXISTS but was refused. Passed IN by the
 * extension, because the modules that discover the refusal are copied flat
 * into a pi-home and cannot log. */
export type PaneKeyRefusalReporter = (refusal: AgentKeyRefusal, identity: PaneIdentity) => void
