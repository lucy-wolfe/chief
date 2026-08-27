//! Building the person-contracts document from the manifest.
//!
//! Port of the decision half of
//! `apps/cli/src/legacy/organization/org-person-contracts.ts` (deleted by this
//! change). The contract TEXT comes from
//! [`crate::store::agent_contracts::person_agents_guide`]; this module decides
//! which people get an entry, hashes the text, and answers whether the stored
//! document needs replacing at all.
//!
//! # Whole-document regeneration, never a merge
//!
//! [`build_organization_person_contracts`] rebuilds from `people_order`, so a
//! departed person's entry simply stops being produced. Merging onto the stored
//! document instead would keep contract text alive for people who left, and the
//! boot path would go on projecting an `AGENTS.md` for a workspace nobody owns.
//!
//! # The MD5 is a content fingerprint, not a security primitive
//!
//! `person_contracts.md5` is what the boot path compares an on-disk
//! `AGENTS.md` against to decide whether to rewrite it. It is implemented here
//! rather than pulled in as a dependency because it must produce exactly the
//! digest the existing column already holds, and because a checksum used only
//! to answer "did these bytes change" has no threat model that a hash crate
//! would improve. It is never used to authenticate anything.

use std::collections::BTreeMap;

use rusqlite::Transaction;

use crate::error::store_failure;
use crate::error::Refusal;
use crate::store::agent_contracts::person_agents_guide;
use crate::store::organization::OrganizationManifest;
use crate::store::person_contracts::rows::{
    OrganizationPersonContracts, PersonContractEntry, PERSON_CONTRACTS_VERSION,
};
use crate::ChiefdError;

/// Rebuild every person's operating contract from the manifest THIS transaction
/// can see, and publish it in that same transaction — writing nothing when no
/// contract changed.
///
/// Returns `(published, seq)`: `published` is false for the no-change case,
/// where `seq` is the company's current audit sequence. The no-change branch is
/// load-bearing rather than an optimisation — republishing identical text
/// re-stamps every `AGENTS.md` mtime and destroys the drift detection the boot
/// path depends on.
///
/// # The ONE rebuild
///
/// Both entry points call this: the roster mutation that creates people (so a
/// department commits with its people's contracts, exactly as company genesis
/// commits with its own — `org_manifest_genesis_with_models`), and the boot
/// rebuild. They are two callers of one implementation, not two implementations
/// of one fact: a second derivation of a contract is how the durable text and
/// the text on disk would come to disagree.
///
/// # Errors
/// * [`crate::store::organization::MANIFEST_INVALID`] when the manifest is
///   absent or a person references a unit the manifest does not contain.
/// * [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn rebuild_person_contracts(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
) -> Result<(bool, i64), ChiefdError> {
    let manifest = crate::store::organization_rows::reconstruct(tx, slug)?.ok_or_else(|| {
        ChiefdError::Refused(Refusal::new(
            crate::store::organization::MANIFEST_INVALID,
            format!("Company '{slug}' has no organization manifest"),
        ))
    })?;
    let next = build_organization_person_contracts(&manifest).map_err(ChiefdError::Refused)?;
    // `manifest.slug` is the company's DISPLAY name (`org_settings.display_slug`),
    // and `slug` is the row key it is stored under. The document's `organization`
    // is stamped from the former, so it must be VALIDATED against the former too
    // — handing the row key here is what made a freshly built document fail its
    // own identity check with "organization 'a7-seed' is not this company
    // '71a6cc3805dc'".
    let company = manifest.slug.as_str();
    let current = crate::store::person_contracts::rows::reconstruct(tx, slug, company)?;
    if !contracts_changed(current.as_ref(), &next) {
        let seq = crate::store::rows_txn::current_seq(tx, slug)
            .map_err(|e| store_failure("person-contracts-rows", e))?;
        return Ok((false, seq));
    }
    let seq = crate::store::person_contracts::rows::publish(tx, slug, company, at, &next)?;
    Ok((true, seq))
}

/// The lowercase hex MD5 of `text` — the value `person_contracts.md5` stores.
#[must_use]
pub fn person_contract_md5(text: &str) -> String {
    let digest = md5_digest(text.as_bytes());
    let mut out = String::with_capacity(32);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Rebuild the whole document from the manifest.
///
/// # Errors
/// [`crate::store::organization::MANIFEST_INVALID`] when a person references a
/// unit or head the manifest does not contain.
pub fn build_organization_person_contracts(
    manifest: &OrganizationManifest,
) -> Result<OrganizationPersonContracts, Refusal> {
    let mut contracts: BTreeMap<String, PersonContractEntry> = BTreeMap::new();
    for person_id in &manifest.people_order {
        let Some(person) = manifest.people.get(person_id) else { continue };
        let text = person_agents_guide(manifest, person)?;
        let md5 = person_contract_md5(&text);
        contracts
            .insert(person_id.clone(), PersonContractEntry { text, md5, extra: BTreeMap::new() });
    }
    Ok(OrganizationPersonContracts {
        version: PERSON_CONTRACTS_VERSION,
        organization: manifest.slug.clone(),
        contracts,
        extra: BTreeMap::new(),
    })
}

/// Whether the freshly built document differs from what is stored.
///
/// Compares the entry set and each entry's MD5 — the text is a pure function of
/// the manifest, so equal digests mean equal bytes. Returning `false` is what
/// keeps a boot from re-stamping every `AGENTS.md` mtime and destroying the
/// drift detection that depends on it.
#[must_use]
pub fn contracts_changed(
    current: Option<&OrganizationPersonContracts>,
    next: &OrganizationPersonContracts,
) -> bool {
    let Some(current) = current else { return true };
    if current.contracts.len() != next.contracts.len() {
        return true;
    }
    next.contracts.iter().any(|(person_id, entry)| {
        current.contracts.get(person_id).is_none_or(|stored| stored.md5 != entry.md5)
    })
}

/// The MD5 per-round constants: `floor(abs(sin(i + 1)) * 2^32)`.
const MD5_K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// The MD5 per-round left-rotation amounts.
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// RFC 1321 MD5 over `input`, little-endian digest bytes.
fn md5_digest(input: &[u8]) -> [u8; 16] {
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in message.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            m[index] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(MD5_K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(MD5_S[i]));
            a = tmp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::northstar_manifest;

    fn manifest() -> OrganizationManifest {
        northstar_manifest(1_700_000_000_000)
    }

    /// A company keyed by its directory hash, named something else.
    ///
    /// The row key is `sha256(<dir>)[..12]`; the name the operator typed lives
    /// in `org_settings.display_slug`. Every real company now has this shape,
    /// so a fixture where the two agree proves nothing about either.
    const ROW_KEY: &str = "71a6cc3805dc";
    const DISPLAY_SLUG: &str = "a7-seed";

    fn seeded_company(conn: &mut rusqlite::Connection) {
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("company schema");
        let tx = conn.transaction().expect("txn");
        let mut m = manifest();
        m.slug = DISPLAY_SLUG.to_string();
        crate::store::organization_rows::genesis(&tx, ROW_KEY, &m).expect("genesis");
        tx.commit().expect("commit");
    }

    /// The derived `organization` field means the company's DISPLAY slug — the
    /// name genesis committed — never the row key it is stored under.
    #[test]
    fn a_rebuild_stamps_the_display_slug_not_the_row_key() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        seeded_company(&mut conn);
        let tx = conn.transaction().expect("txn");

        let (published, _seq) =
            rebuild_person_contracts(&tx, ROW_KEY, "2026-08-15T00:00:00.000Z").expect("rebuild");
        assert!(published, "the first rebuild writes every contract");

        let back = crate::store::person_contracts::rows::reconstruct(&tx, ROW_KEY, DISPLAY_SLUG)
            .expect("reconstruct")
            .expect("a published document");
        assert_eq!(back.organization, DISPLAY_SLUG);
    }

    #[test]
    fn md5_matches_the_rfc_1321_test_suite() {
        assert_eq!(person_contract_md5(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(person_contract_md5("a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(person_contract_md5("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(person_contract_md5("message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(
            person_contract_md5("abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(
            person_contract_md5("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
        assert_eq!(
            person_contract_md5(
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn md5_spans_multiple_blocks_correctly() {
        // 1000 'a' characters — well past the 64-byte block boundary and the
        // 56-byte padding edge case.
        let long = "a".repeat(1_000);
        assert_eq!(person_contract_md5(&long), "cabe45dcc9ae5b66ba86600cca6b8ba8");
    }

    #[test]
    fn every_person_in_order_gets_an_entry() {
        let m = manifest();
        let doc = build_organization_person_contracts(&m).expect("contracts");
        assert_eq!(doc.version, PERSON_CONTRACTS_VERSION);
        assert_eq!(doc.organization, "northstar-conformance");
        assert_eq!(doc.contracts.len(), m.people_order.len());
        for person_id in &m.people_order {
            assert!(doc.contracts.contains_key(person_id), "{person_id} missing");
        }
    }

    #[test]
    fn each_entry_carries_the_digest_of_its_own_text() {
        let m = manifest();
        let doc = build_organization_person_contracts(&m).expect("contracts");
        for entry in doc.contracts.values() {
            assert_eq!(entry.md5, person_contract_md5(&entry.text));
        }
    }

    #[test]
    fn a_departed_person_produces_no_entry() {
        let mut m = manifest();
        m.people_order.retain(|id| id != "signal-researcher");
        m.people.remove("signal-researcher");
        let doc = build_organization_person_contracts(&m).expect("contracts");
        assert!(!doc.contracts.contains_key("signal-researcher"));
    }

    #[test]
    fn an_unchanged_rebuild_is_not_a_change() {
        let m = manifest();
        let stored = build_organization_person_contracts(&m).expect("contracts");
        let next = build_organization_person_contracts(&m).expect("contracts");
        assert!(!contracts_changed(Some(&stored), &next));
    }

    #[test]
    fn an_absent_document_is_always_a_change() {
        let m = manifest();
        let next = build_organization_person_contracts(&m).expect("contracts");
        assert!(contracts_changed(None, &next));
    }

    #[test]
    fn an_edited_mandate_changes_the_document() {
        let m = manifest();
        let stored = build_organization_person_contracts(&m).expect("contracts");
        let mut edited = m.clone();
        if let Some(person) = edited.people.get_mut("signal-researcher") {
            person.mandate = "Own something else entirely.".to_string();
        }
        let next = build_organization_person_contracts(&edited).expect("contracts");
        assert!(contracts_changed(Some(&stored), &next));
    }

    #[test]
    fn a_dropped_person_changes_the_document() {
        let m = manifest();
        let stored = build_organization_person_contracts(&m).expect("contracts");
        let mut smaller = m.clone();
        smaller.people_order.retain(|id| id != "it-head");
        let next = build_organization_person_contracts(&smaller).expect("contracts");
        assert!(contracts_changed(Some(&stored), &next));
    }
}
