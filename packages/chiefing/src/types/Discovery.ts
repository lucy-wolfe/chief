// Types for the discovery surface (src/discovery/**, E10-chiefing-addendum.md).
// Placed here rather than inside discovery/* per lucy/no-exported-type-outside-types-dir
// (the addendum's own file layout shows these declared inline; E2's convention wins on
// this point per the addendum's own "if any E2 convention differs, E2 wins" rule).
// Do not move these back into discovery/* — E10-S4 should not "fix" this back.

/** One COMPANY in beacond (ruling D21: the row IS the company). Field-for-field
 * the Rust `Company` (apps/chiefd/crates/beacond/src/wire.rs) — camelCase on
 * the wire. The location fields are optional: a company that has never been
 * started, or has been stopped gracefully, has none of them. */
export interface CompanyRow {
  /** **The identity.** The canonical absolute directory the operator ran
   * `chief` in. One row per directory, forever. */
  readonly dir: string
  /** The directory-derived company key, `sha256(dir)[..12]` — twelve lowercase
   * hex characters.
   *
   * READ, never computed. It is minted once by whoever creates the company and
   * served back on every row, so the wire identity every chiefd route resolves
   * a company by has exactly one producer. The composite `slug@hash` this
   * replaced was recomputed independently in nine places and drifted. */
  readonly key: string
  /** The company's DISPLAY name. Not an identity: two directories may hold
   * companies with the same slug, and both rows are legitimate. */
  readonly slug: string
  /** When the COMPANY was created. ISO-8601 millis. NOT when a daemon last
   * registered — no route ever rewrites it. */
  readonly registeredAt: string

  // ---- location: all five present together, or none of them ----
  readonly url?: string
  readonly port?: number
  readonly pid?: number
  readonly hostname?: string
  readonly lastSeenAt?: string
}

/** A daemon's published location for one company directory, read from
 * `<dir>/.chief/run/daemon.json`. Field-for-field the Rust
 * `host_primitives::rendezvous::DaemonRendezvous` — camelCase, and no field
 * either side has not declared. */
export interface DaemonRendezvous {
  /** The company directory this daemon serves, canonical and absolute.
   * Carried rather than inferred from the file's own location so a reader can
   * catch a rendezvous COPIED between directories. */
  readonly dir: string
  /** The directory-derived company key, `sha256(dir)[..12]`. The same value
   * beacond serves as {@link CompanyRow.key}, published here so a pane inside
   * the directory needs no registry call to learn its own identity. */
  readonly key: string
  /** Where the daemon bound its docstore listener. */
  readonly url: string
  /** The daemon process's pid. A pid ALONE is not proof — pids are reused. */
  readonly pid: number
}

export interface DiscoveryClientOptions {
  /** beacond's base URL. Required and explicit — there is no ambient fallback. */
  readonly beacondUrl: string
  /** Per-request timeout. Default 2000. */
  readonly timeoutMs?: number
  /** Injected for tests. Defaults to global fetch. */
  readonly fetchImpl?: typeof fetch
}

// #751/G5: `RegistrationLiveness` / `LivenessHost` and the
// `registrationLiveness` judge they described are DELETED. The judge is
// `apps/chiefd/crates/chief-cli/src/discovery.rs:90-125`, and the port
// deliberately CORRECTED the rule this package had frozen: an unnameable host
// ("unknown" on either side) is judged by pid, not answered 'unknown'. The TS
// copy was exported, unit-tested green, and called by nothing — a second
// implementation pinning the superseded rule. Do not reintroduce it here; if
// a TypeScript caller ever needs a liveness verdict, read it from chiefd.

/** Mirrors ChiefdUnavailableKind, value for value (ruling D6). */
export type BeacondUnavailableKind = 'unreachable' | 'timeout' | 'http-error' | 'malformed-body'
