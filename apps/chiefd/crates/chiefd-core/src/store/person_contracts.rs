//! The `person-contracts` store port (org-data-normalization P0, N2-contracts).
//!
//! Per-person operating-contract TEXT on the dedicated `person_contracts`
//! table. Own DTO + diff; shares only the `rows_txn` publish scaffold.

pub mod build;
pub mod rows;
