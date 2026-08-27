//! The one-daemon converge cycle.
//!
//! #751/P8-P10 REMOVED `observe`, `interpret`, `spawn_cmd`, `ever_observed`,
//! the single-writer `probe` and the `summary` of pane steps from this module:
//! chiefd no longer observes or actuates a display, and P10 deleted the
//! backend's copy of the topology walk they served. They live in `chief-cli`'s
//! `actuate` tree.
//!
//! What is left here is the desired set, the ramp, the budgets, the breaker and
//! the single-flight claim — the safety policy that rides on
//! `POST /v1/org/runtime/actions` instead of being applied in-process:
//!
//! * [`cycle`] — one reconcile pass: the activity-fence projection, the
//!   per-person action plan, the pointer sweep, the audit intent row and the
//!   observation publish.
//! * [`resource_catalog`] — derives `--tools`, selects a session, and refuses
//!   an absent non-Chief agent home. Its name is historical; it contains no
//!   installed-resource catalog or materialization state.
//! * [`safety`] — the day-1 safety scaffold (Unit C): the actuation-mode gate,
//!   the circuit breaker, and the single-flight + floor-interval cycle slot.
//!   Consulted by the cycle *before* it publishes anything to act on.
//!
//! The pure pointer sweep lives in `chiefd_core::runtime::pointer_sweep`
//! because `chiefd-core` owns pure logic and cannot depend on this crate.

pub mod api_host_profile;
pub mod cycle;
pub mod person_model;
pub mod resource_catalog;
pub mod safety;

pub use api_host_profile::{
    ActuationFacts, ApiHostLaunchProfile, ApiHostLaunchProfileConfig, ApiHostLaunchProfileError,
    ApiHostLaunchProfileRead, ApiHostLaunchProfileSource,
};
pub use cycle::{
    build_launch_catalog, build_launch_catalog_for_session_epoch, reconcile_cycle,
    root_pi_agent_dir, ActivityProjectionInput, ActuatorConfig, ConvergeActuator,
};
pub use resource_catalog::{read_materialized_resources, MaterializedResources};

/// Serializes the two tests that mutate `BEACOND_URL` (`cycle::tests` and
/// `api_host_profile::tests`).
///
/// Both producers read the same process-global variable, both live in this
/// crate's one test binary, and cargo runs tests in parallel — so without a
/// lock each one's `remove_var` can land inside the other's assertion. A
/// guard that is only usually true is worse than no guard: it teaches the
/// next reader to re-run until green.
#[cfg(test)]
pub(crate) static BEACOND_URL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
