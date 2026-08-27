// #838's manifest: the closed list of every workspace member — both the
// bun `apps/*`/`packages/*` packages and the Cargo crates under
// `apps/chiefd` — and its recorded CI-execution disposition.
//
// Same completeness discipline #877 (`guard-wiring-manifest.mjs`) proved
// for repo-invariant guards, applied here to test EXECUTION instead of
// test EXISTENCE: a workspace member arriving with no recorded CI state is
// invisible in exactly the way an unwired guard was — nothing fails,
// nothing notices, and the member's tests (if any) run nowhere but a
// laptop. `scripts/test/ci-workspace-state.test.mjs` enumerates the real
// tree and fails the moment an entry here goes stale in EITHER direction.
//
// Every entry is one of three shapes:
//   { status: 'vitest' }
//     — a bun workspace member whose own package.json declares
//       "test:unit"; must be covered by `bun run test` running in CI
//       (checked once, not per-package, since the root `test` script is a
//       single turbo-aggregated `turbo run test:unit` that fans out to every
//       member itself — the per-member claim is "this package's test:unit
//       task is REACHABLE by that aggregate", not "this exact string appears
//       in the yaml"). Note the two namespaces: `test:unit` is the TURBO
//       TASK each member declares; `test` is the ROOT script that drives it.
//   { status: 'cargo' }
//     — a Cargo workspace member (`apps/chiefd` itself, or one of its
//       `members` crates); covered by `cargo test --workspace` in the
//       `cargo-test-workspace` job (#865/#857).
//   { status: 'no-tests', reason: '<why>' }
//     — a bun workspace member with no `test:unit` script, and a stated
//       reason rather than a silent omission.
export const CI_WORKSPACE_STATE_MANIFEST = {
  'apps/web': { status: 'vitest' },
  'packages/chiefing': { status: 'vitest' },
  'packages/eslinter': { status: 'vitest' },
  'packages/piing': { status: 'vitest' },
  'packages/testing': { status: 'vitest' },
  'apps/chiefd': {
    status: 'cargo',
    reason: 'Rust Cargo workspace root — deliberately no package.json (the apps/zbox precedent, keeps it invisible to the bun workspace glob); covered by cargo-test-workspace, not test:unit.',
  },
  'apps/chiefd/crates/beacond': { status: 'cargo' },
  'apps/chiefd/crates/chiefd-core': { status: 'cargo' },
  'apps/chiefd/crates/chiefd-host': { status: 'cargo' },
  'apps/chiefd/crates/chiefd-api': { status: 'cargo' },
  'apps/chiefd/crates/chiefd-daemon': { status: 'cargo' },
  'apps/chiefd/crates/chief-cli': { status: 'cargo' },
  'apps/chiefd/crates/host-primitives': { status: 'cargo' },
  'apps/chiefd/crates/identity-keys': { status: 'cargo' },
  'apps/chiefd/crates/chiefd-log': { status: 'cargo' },
  'apps/chiefd/tests/unit-d': { status: 'cargo' },
  'apps/chiefd/tests/wire-boundary': { status: 'cargo' },
}
