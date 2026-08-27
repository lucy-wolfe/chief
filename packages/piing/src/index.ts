/**
 * `@chief/piing` — shared Pi-harness libraries. Barrel: every public symbol
 * except the `./extension-runtime` subpath's (import that separately from
 * `@chief/piing/extension-runtime`).
 *
 * Every symbol below is implemented (E3); the E0-S5/#756 stub phase is over
 * and nothing here throws `not implemented`. See the E3 epic (#786) for the
 * Contract this surface descends from.
 *
 * #751/G5: the pi-home materialization (`ExtensionLayout`, `HomeWriters`,
 * `StageThenSwap`), pane-argv/session (`PaneCommand`, `PaneEnvironment`,
 * `SessionDiscovery`) and `LauncherProvider` modules were deleted rather than
 * kept — every one had zero production callers while the live implementation
 * runs in Rust (`chiefd-host/src/materialize/**`, `converge_apply/**`,
 * `runtime/reconcile_plan.rs`). A second implementation nobody calls is the
 * drift this package exists downstream of.
 */

// --- runtime -----------------------------------------------------------------
// TOMBSTONE: `@/types/PersonPlan`, THE WHOLE FILE, with `PiBinaryOptions` (the
// argument of `resolvePiBinary`) and `PiRuntimeAttestation` (the return of
// `attestPiRuntime`, whose `patchHash` names a patch that no longer exists).
// Deleting both types left a file of pure comments, which `lucy/no-empty-file`
// refuses by name — the repo already had a rule for this shape.
//
// Its docblock carried #751/G5's history: eight pi-home materialization,
// pane-argv and session-discovery types deleted with the modules they
// described, because that subsystem is owned by `chiefd-host/src/materialize/**`
// in Rust and the TypeScript side was a zero-consumer twin. That history has
// outlived its file twice now, which is the argument for keeping it here.
//
// TOMBSTONE: `@/runtime/PiAttestation` (`attestPiRuntime`, `PINNED_PI_VERSION`,
// `pinnedPiArtifacts`). It proved `node_modules/.bin/pi` was the pinned PATCHED
// build; its five pinned hashes were five of the twelve dist files the patch
// rewrote. With no patch it has no subject, and bun's lockfile already
// guarantees the integrity of what it installs.
//
// TOMBSTONE: `@/runtime/PiBinary` (`resolvePiBinary`, `TEAM_LAUNCHER_PI_ENV`,
// `piPackageRoot`, `piRpcEntryPath`, `piRpcClientEntryPath`) — the whole module.
//
// THIS ONE IS OPPORTUNISTIC AND NOT A CONSEQUENCE OF THE PATCH, which matters
// because the next reader will otherwise infer a causal link that does not
// exist. It was already dead: the resolver was ported to Rust, where
// `pi_binary` is daemon-wide config (`runtime/launch_hash.rs`) and
// `founder_pi.rs` speaks of "the `@chief/piing` resolvers it called" in the
// past tense. Every remaining reference was this barrel and its own tests, and
// `piPackageRoot`'s last non-test caller was PiAttestation above — so removing
// the attestation is what made the module observably caller-less, not what
// killed it.
export {
  piingExtensionsRoot,
  piingPackageRoot,
  piingSkillsRoot,
  workspaceNodeModulesRoot
} from '@/runtime/PiPaths'

// --- policy --------------------------------------------------------------------
export { BUILTIN_TOOLS } from '@/policy/CapabilityPolicy'

// --- home ------------------------------------------------------------------------
export { identityAccentOrder, organizationPersonAccent } from '@/home/IdentityTheme'

// --- types -----------------------------------------------------------------------
export type { BuiltinTool } from '@/types/ProjectionTypes'
