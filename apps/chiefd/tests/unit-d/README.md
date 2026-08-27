# Unit D integration tests (LIVE, #871)

`apps/chiefd/tests/unit-d/` is a real workspace member (`chiefd-unit-d`, see
`apps/chiefd/Cargo.toml` `members`): a harness `cargo test --workspace` does
not run is a harness that rots. `cargo test -p chiefd-unit-d` (or
`--workspace`) compiles and runs
all 5 files / 20 test functions.

## History

Originally scaffolded as INERT files never wired into the workspace, on the
theory that the reconciler source they reference (`chiefd-core/src/runtime/**`,
`chiefd-host/src/converge_apply/**`) was unmerged. By #871 that source had
landed — `compute_converge_plan`, `apply_plan`, `store::converge_safety`, the
host `converge_apply::safety` wrapper, `converge_intent::abort_open`,
`MailboxDeliverySink`, and `converge_apply::cycle::reconcile_cycle` (the M2
orchestrator) all exist on main — so #871 wired the directory in directly as
one package rather than executing the original per-file "activation map" (git-mv
each file into its own target crate's `tests/` dir), which was superseded by
that landing and by the explicit instruction to wire the whole directory in
as a single member.

Wiring in surfaced real, never-before-compiled bugs, fixed as part of #871:
- `crash_injection.rs`: `ConvergeIntentBody` no longer has an
  `organization_revision` field — the test literal was stale against the
  landed struct.
- `safety_gate_integration.rs`: `ConvergePlan` gained `warnings`,
  `owned_panes`, `owned_pane_generations`, `owned_windows` fields — the test's
  plan builder needed them added. Also: `d4_3`'s budget scenario asserted
  `kills + respawns` together exceed the limit, but `check_budget` only counts
  `predicted_respawn_persons` (kills are deliberately exempt, see that
  function's own doc comment) — the scenario was corrected to make respawns
  alone exceed the limit rather than weakening the assertion.
- `clippy.toml`'s `allow-expect-in-tests`/`allow-unwrap-in-tests` are keyed to
  `#[test]`-attributed functions, not to "this file lives under `tests/`" — a
  handful of non-`#[test]` helper functions (`seed`, `open`, `kill_plan`,
  `IdempotentSink`'s methods) needed an explicit file-level
  `#![allow(clippy::expect_used)]`/`#![allow(clippy::unwrap_used)]`.

## #879: five more implemented for real

#871 left 9 functions `#[ignore]`d as an explicit follow-up rather than a
"mechanical un-ignore." #879 picked that follow-up up and implemented 5 of
the 9 for real against `reconcile_cycle` — `d2_1_a_crash_mid_observe_leaves_no_partial_actuation`,
`d2_2_a_crash_mid_apply_is_fail_stop_and_re_plans`,
`d1_2_already_converged_cycle_plans_and_actuates_nothing`,
`d4_1_three_failed_apply_cycles_trip_the_breaker_end_to_end`,
`d4_4_tripped_breaker_suppresses_actuation_through_the_running_cycle` — each
built on `toctou_assembled.rs`'s already-proven identity-mismatch fail-stop
pattern (a scripted tmux reply carrying the wrong pane identity), and each
tamper-verified: temporarily broken to confirm the assertion actually fires
for the right reason, then restored.

The remaining 4 (D1.1, D1.3, D1.4, D2.4-concrete) stay `#[ignore]`d with a
specific, re-researched reason each — not "not done yet." Each fn's own doc
comment states the exact finding:
- **D1.1**: a genuinely successful `CreateSession`+spawn actuation from an
  empty session has no proven tmux reply-script precedent anywhere in this
  codebase to build on; scripting one blind risked a test that passes
  regardless of correctness.
- **D1.3**: needs a different, heavier harness (`Daemon::run_supervision_reconcile`
  + a `CycleInputGatherer`, from the `chiefd` BINARY crate) than the
  `reconcile_cycle`-direct pattern every other case in this suite uses —
  `chiefd-unit-d` depends on neither, and adding that dependency is a real
  layering decision this ticket shouldn't make unilaterally.
- **D1.4**: its own premise doesn't match the current code — `ReconcileReport`
  has no `admission_ms` field. The stub was written against a different or
  earlier shape than what's on main.
- **D2.4-concrete**: needs a genuine mid-transaction crash between
  `MailboxDeliverySink`'s staging commit and the scheduler's separate
  `mark_delivered` commit — `CompanyDb::mutate` has no injectable failure seam
  anywhere in this codebase to reproduce that for real (by design: the
  single-writer actor's mutations are atomic).

## Current status

16 of 20 test functions are REAL and pass. 4 stay `#[ignore]`d, each with a
specific reason above (and in the fn's own doc comment) naming exactly what's
still missing.

| File | Real | Ignored |
|---|---|---|
| `toctou_assembled.rs` | 2/2 (D3.1, D3.2) | 0 |
| `safety_gate_integration.rs` | 6/6 (D4.2, D4.2b, D4.3, D4.5, D4.1, D4.4) | 0 |
| `single_flight.rs` | 3/3 (D5.1–D5.3) | 0 |
| `crash_injection.rs` | 4/5 (D2.1, D2.2, D2.3, D2.4-fake-sink) | 1 (D2.4-concrete-sink) |
| `end_to_end_cycle.rs` | 1/4 (D1.2) | 3 (D1.1, D1.3, D1.4) |

Every ignored case's comment names the exact, specific gap — not a generic
"not authored yet."
