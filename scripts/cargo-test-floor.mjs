// #857 established this file as THE ratcheting floor for `cargo test
// --workspace`; #889 split it into two instruments that were always doing
// two different jobs (full reasoning in scripts/cargo-test-derive.mjs's
// header — read that first if this file is confusing on its own):
//
//   VACUITY FLOOR (this file)  — "did this run check roughly anything at
//     all?" Low and wide BY DESIGN: its only job is catching total
//     collapse (a workspace-members glob stopping matching, N tests
//     becoming 0), and a wide floor survives ordinary legitimate deletion
//     without an edit. Still hand-maintained, because that is the CORRECT
//     setting for this job, not a shortcut.
//   LOSS RATCHET (cargo-test-derive.mjs) — "did we silently LOSE tests?"
//     Must be EXACT to do its job at all, so it is now DERIVED from the
//     source tree on every run rather than transcribed here. This is what
//     `CARGO_TEST_EXECUTED_FLOOR`/`CARGO_TEST_BLOCK_FLOOR` used to be
//     before #889, and why a transcribed number failed at both jobs: wide
//     enough that #871's two-test loss hid in the slack (floor 2377 vs
//     true 2379), yet tight enough that ordinary landings made it stale
//     again one cycle later (#879: 2390 vs a true 2430).
//
// HISTORY (kept for provenance; these values no longer gate anything —
// scripts/cargo-test-derive.mjs computes the live equivalent on every run):
//   #871 baseline: 2390 executed / 74 blocks (unit-d wired in: +11 executed
//     across +5 blocks, 9 of unit-d's 20 functions #[ignore]d pending
//     unauthored bodies).
//   #879: five of unit-d's `#[ignore]`d stubs implemented for real (D2.1,
//     D2.2, D1.2, D4.1, D4.4); the remaining four stay `#[ignore]`d with a
//     specific, re-researched reason each. Combined with #764's unrelated
//     canonical growth, the tree measured 2450 executed / 77 blocks at
//     that landing.
//   #889: the last transcription. From here, `bun run test:cargo-test-floor`
//     compares a real run against `deriveExpectedCounts()`'s output, not
//     against a number written down here.
//
// RAISE the vacuity floors below when the workspace legitimately grows by
// enough to make the current values uncomfortably tight (they are meant to
// have generous headroom — see cargo-test-derive.mjs's own
// MIN_PLAUSIBLE_* constants for the derivation's OWN internal vacuity
// guard, a related but separate check). NEVER lower either to make a run
// pass; a drop here is exactly the kind of change that erodes a guard
// while looking like routine maintenance (§2.5) and deserves the same
// scrutiny as any other floor edit.
export const CARGO_TEST_EXECUTED_VACUITY_FLOOR = 1200
export const CARGO_TEST_BLOCK_VACUITY_FLOOR = 35
