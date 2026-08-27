// Types for PersonContractsClient — the normalized `person-contracts` rows
// (`/v1/org/person-contracts/{read,publish}`).
//
// Source: src/organization/org-person-contracts-rows.ts:27-56, re-verified
// against apps/chiefd/crates/chiefd-core/src/store/person_contracts/rows.rs.
//
// #844: neither interface below models the Rust structs' `#[serde(flatten)]
// extra` catch-all — deliberately, and safely; see `RowDocs.ts`'s #844 note
// for why (the write path rejects any non-empty `extra`, read never returns
// one, and `chiefd-core/tests/serde_flatten_catchall_conformance.rs` makes
// that a structural guarantee rather than an implicit convention).

/** Rust authority: person_contracts/rows.rs `PersonContractEntry`
 * (`#[serde(rename_all = "camelCase")]`). One person's stored contract. */
export interface PersonContractEntry {
  text: string
  md5: string
}

/** Rust authority: person_contracts/rows.rs `OrganizationPersonContracts`
 * (`#[serde(rename_all = "camelCase")]`; TS names it
 * `OrganizationPersonContractsDocument`). `version`/`organization` are
 * DERIVED by chiefd, never stored — a read returns them reconstructed, a
 * publish must carry them (chiefd validates identity). */
export interface OrganizationPersonContractsDocument {
  version: 1
  organization: string
  contracts: Record<string, PersonContractEntry>
}

/** A read hit carries the reconstructed document. */
export interface PersonContractsReadResult {
  found: boolean
  document?: OrganizationPersonContractsDocument
}
