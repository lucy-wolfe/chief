//! The runtime half of chiefd: who should be running, and what should be done
//! about it.
//!
//! Everything here is pure and zero-I/O. [`desired`] holds the manifest +
//! activity model and the one desired-person predicate; [`roster`] publishes
//! that as client-agnostic facts; [`actuation`] turns the delta between those
//! facts and an actuator's report into person-scoped actions, under the
//! admission ramp and the safety budgets.
//!
//! #751/P8-P10: the topology planner that used to live beside them — desired
//! panes and windows, the observed-runtime diff, the ordered plan of pane steps
//! and the layout maths — is GONE from this crate. chiefd decides WHO runs; the
//! operator client (`chief-cli`'s `actuate::plan`) decides where it is shown and
//! does the showing.

pub mod actuation;
pub mod attendance;
pub mod converge_intent;
pub mod delivery_sink;
pub mod desired;
pub mod duty_hooks;
pub mod launch_catalog;
pub mod launch_hash;
pub mod pointer_sweep;
pub mod project;
pub mod roster;
