//! Publishing a person's minted identity key into chiefd's trust table
//! (#751/P7).
//!
//! # Why this had to exist
//!
//! Every person has had a P-256 private key in their own home since agent-auth
//! P0 — [`crate::identity_key::ensure_identity_key`] mints it once and never
//! regenerates it. Nothing ever enrolled the PUBLIC half. The
//! table the verify-middleware reads (`identities`) was therefore empty of
//! people, which did not matter while chiefd authenticated agents by walking
//! pid ancestry to the terminal pane they descended from. That walk is deleted,
//! so a key nobody enrolled is a person who cannot prove anything.
//!
//! This module closes that gap at the ONE place both facts are in hand: the
//! call that just created the person's home also knows the person, the company,
//! and the directory.
//!
//! # PROVISIONING is the origin; enrolment alone is the repair
//!
//! Riding on materialization was the whole of the next defect. `enrol_people`
//! had exactly ONE caller — the deleted `refresh_materialization` — and the key
//! it enrols was minted by that same pass, so BOTH halves of a person's
//! identity needed a runtime host. A company that had not converged held people
//! who genuinely exist and can prove nothing: `/v1/auth/challenge` answered 401
//! for the CEO of a company created one call earlier, and `chiefd run
//! --serve-only`, which mounts no runtime host at all, could never produce a
//! person bearer however long it ran. It stayed hidden because every wait in
//! the suites sat after a tool call whose reconcile enrolled as a side effect —
//! a tool call was the hidden precondition of the credential that tool call
//! needed.
//!
//! [`provision_person_identity`] is the answer: mint-if-absent and enrol, at
//! the moment the person becomes durable. A credential whose validity depends
//! on some unrelated operation having run first is the bug, not the fix. This
//! is the same shape the daemon already uses for its own two principals —
//! `ensure_identity_key` mints, `build_auth_runtime` enrols, both before a
//! request is served.
//!
//! # It never re-keys
//!
//! [`enrol_person`] on its own still never mints: an absent key is reported, so
//! the repair path can say "this person has no identity" instead of inventing
//! one. Minting belongs to [`provision_person_identity`], and it is
//! create-ONCE through [`crate::identity_key::ensure_identity_key`], the one
//! create-once rule the daemon's own keys share — so no two writers can
//! disagree, and whichever runs first owns the
//! anchor. Enrolment is idempotent on (id, fingerprint), and a DIFFERENT key
//! for an existing id is a deliberate rotation act that must be performed
//! deliberately — here it is reported and stepped over, never silently applied.
//! A person whose key does not parse is likewise reported, not repaired: a
//! placeholder identity would authenticate as that person forever.
//!
//! # Nothing here fails a materialization
//!
//! Every outcome is a value. A company whose homes are correct but whose trust
//! table lags is repaired by the next pass; a company that refuses to
//! materialize because one key would not parse is not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use p256::pkcs8::{DecodePrivateKey, EncodePublicKey};
use p256::SecretKey;
use sha2::{Digest, Sha256};

use chiefd_core::actor::CompanyDb;
use chiefd_core::error::ChiefdError;
use chiefd_core::store::identities::{IdentityKind, NewIdentity};

use crate::agent_home::{chief_identity_key_path, identity_key_path};
use crate::identity_key::{ensure_identity_key, IdentityKeyMint};

/// The refusal code the company actor raises for a same-id, different-key
/// enrolment. Matched rather than re-spelled so a rename cannot turn a rotation
/// into a silent overwrite.
const FINGERPRINT_CONFLICT: &str = "auth-identity-fingerprint-conflict";

/// What one person's enrolment did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonEnrolment {
    /// A new `person` identity row was inserted.
    Enrolled,
    /// The same key was already enrolled — the idempotent steady state.
    AlreadyEnrolled,
    /// No key in that agent home yet (a home this pass did not write).
    NoKey,
    /// The key is present but unreadable or not a P-256 PKCS#8 key.
    Unusable(String),
    /// A DIFFERENT key is already enrolled under this id. Rotation is explicit
    /// and is not performed here.
    RotationPending,
    /// The trust table could not be written.
    Failed(String),
}

impl PersonEnrolment {
    /// Whether this outcome leaves the person able to authenticate.
    #[must_use]
    pub fn is_authenticable(&self) -> bool {
        matches!(self, Self::Enrolled | Self::AlreadyEnrolled)
    }
}

/// The canonical fingerprint of a public key: base64url(SHA-256(SPKI DER)).
///
/// Byte-identical to `chiefd_api::authn::fingerprint_of_spki`. It is computed
/// on both sides rather than shared because `chiefd-host` must not depend on
/// `chiefd-api` — and the two are pinned together by
/// [`tests::the_fingerprint_matches_the_verifiers_definition`], which recomputes
/// the definition from first principles rather than from either implementation.
#[must_use]
pub fn fingerprint_of_spki(spki_der: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(spki_der))
}

/// The public half of a PKCS#8 PEM private key, as `(spki_base64, fingerprint)`.
///
/// The ONE place either value is derived. The launch gate below decides by
/// COMPARING a fingerprint the actor computed earlier against one computed now,
/// and two spellings of "the same key" that can drift are two spellings that
/// will: the gate would withhold a person the trust table is perfectly happy
/// with, or admit the one it is not.
fn key_material(private_pem: &str) -> Result<(String, String), String> {
    let spki_b64 = public_spki_base64(private_pem)?;
    let spki_der =
        BASE64_STANDARD.decode(&spki_b64).map_err(|_| "public key is not base64".to_owned())?;
    Ok((spki_b64, fingerprint_of_spki(&spki_der)))
}

/// The SPKI-DER public key, base64 (standard), for a PKCS#8 PEM private key.
fn public_spki_base64(private_pem: &str) -> Result<String, String> {
    let secret = SecretKey::from_pkcs8_pem(private_pem)
        .map_err(|error| format!("not a P-256 PKCS#8 key: {error}"))?;
    let der = secret
        .public_key()
        .to_public_key_der()
        .map_err(|error| format!("public key could not be encoded: {error}"))?;
    Ok(BASE64_STANDARD.encode(der.as_bytes()))
}

// The key's path is [`crate::agent_home::identity_key_path`] and is imported
// above rather than re-composed here. `dir` throughout this module is the
// COMPANY DIRECTORY — the one the operator ran `chief` in — never the `.chief`
// root beneath it. Composing the path a second time is exactly how the `<slug>`
// segment survived in three places after it was deleted from a fourth.

/// Enrol one person's public key, idempotently.
///
/// The company is taken from the actor's OWN label and is deliberately not a
/// parameter. It was one, briefly, and the tool contract caught what that
/// costs: a caller passed `manifest.slug` — the display name — while every
/// `/v1/org/*` route carries and compares the COMPOSITE company key
/// (`slug@sha256(dataRoot)[..12]`), so every enrolled person belonged to a
/// company no route would ever name and the first authenticated staffing call
/// was refused `requester-company-mismatch`. Which of the two identifies a
/// company is not the caller's decision to get right; the actor already knows.
pub async fn enrol_person(db: &CompanyDb, dir: &Path, person_id: &str) -> PersonEnrolment {
    enrol_person_at(db, &identity_key_path(dir, person_id), person_id).await
}

async fn enrol_person_at(db: &CompanyDb, key_path: &Path, person_id: &str) -> PersonEnrolment {
    let company_slug = db.label().to_owned();
    let company_slug = company_slug.as_str();
    let Ok(pem) = std::fs::read_to_string(key_path) else {
        return PersonEnrolment::NoKey;
    };
    let (spki_b64, fingerprint) = match key_material(&pem) {
        Ok(material) => material,
        Err(error) => return PersonEnrolment::Unusable(error),
    };
    // The steady state is a READ. Enrolment now runs on every roster mutation
    // as well as inside the materialization repair, so on a company of any size
    // "already enrolled" is by far the common answer — and answering it through
    // `identity_enroll` costs one `BEGIN IMMEDIATE` per person per pass behind
    // the single writer. This is a fast path and NOT a second decision: a row
    // whose fingerprint differs falls through to the actor, which still owns
    // check-and-insert in one transaction, so the rotation conflict cannot be
    // lost to a race here.
    if let Ok(Some(existing)) = db.identity_read(person_id.to_owned()).await {
        if existing.fingerprint == fingerprint {
            return PersonEnrolment::AlreadyEnrolled;
        }
    }
    match db
        .identity_enroll(NewIdentity {
            identity_id: person_id,
            principal: person_id,
            kind: IdentityKind::Person,
            company_slug: Some(company_slug),
            pubkey: Some(&spki_b64),
            fingerprint: &fingerprint,
            enrolled_by: None,
        })
        .await
    {
        Ok(true) => PersonEnrolment::Enrolled,
        Ok(false) => PersonEnrolment::AlreadyEnrolled,
        Err(ChiefdError::Refused(refusal)) if refusal.code == FINGERPRINT_CONFLICT => {
            PersonEnrolment::RotationPending
        }
        Err(error) => PersonEnrolment::Failed(error.to_string()),
    }
}

/// Mint this person's identity key if they have none, then enrol its public
/// half. The ONE call a caller makes at the moment a person becomes durable.
///
/// Minting goes through [`ensure_identity_key`], the one create-once rule the
/// daemon's own operator/service keys share: whichever writer runs first owns
/// the anchor and the other preserves it.
///
/// Idempotent end to end. A second call finds the key and reports
/// [`PersonEnrolment::AlreadyEnrolled`]; it never re-mints and never re-keys.
pub async fn provision_person_identity(
    db: &CompanyDb,
    dir: &Path,
    person_id: &str,
    mint: &dyn IdentityKeyMint,
) -> PersonEnrolment {
    let key_path = identity_key_path(dir, person_id);
    provision_person_identity_at(db, &key_path, person_id, mint).await
}

async fn provision_person_identity_at(
    db: &CompanyDb,
    key_path: &Path,
    person_id: &str,
    mint: &dyn IdentityKeyMint,
) -> PersonEnrolment {
    if let Err(error) = ensure_identity_key(key_path, mint) {
        return PersonEnrolment::Failed(format!("identity key could not be created: {error}"));
    }
    enrol_person_at(db, key_path, person_id).await
}

/// Where one person's identity key lives.
///
/// The Chief is the exception and always has been: they are the operator's own
/// Pi, they have no agent home, and their key is the company-level one. Written
/// down ONCE because the launch gate and the provisioning pass must look at the
/// same file — a gate reading a path the minter never writes would withhold the
/// Chief of every company, for ever.
#[must_use]
pub fn person_identity_key_path(dir: &Path, chief_person_id: &str, person_id: &str) -> PathBuf {
    if person_id == chief_person_id {
        chief_identity_key_path(dir)
    } else {
        identity_key_path(dir, person_id)
    }
}

/// Which of these people must be WITHHELD from launch because their identity
/// cannot authenticate, keyed by person with the operator-facing reason.
///
/// # Why a launch needs this at all
///
/// A person whose enrolled fingerprint disagrees with the key in their own home
/// authenticates to nothing. Their pane starts, `pi` fails its first
/// `/v1/auth/challenge`, and the process is gone inside a second — and because
/// nothing gated the launch on it, chiefd handed the same person a full launch
/// spec on the very next pass. Five people on a live company respawned about
/// once a second for the whole of a working day, each logging the same
/// `RotationPending` warning seventy-four times, while the one question an
/// operator asks — why is this person not up — had no answer anywhere. Withheld
/// with a named reason is the fix: the crash loop stops, and the reason says
/// what to do about it.
///
/// # It is a READ, and it never repairs
///
/// Nothing here writes. The launch catalog is a pure read and stays one, and
/// rotation stays what this module has always said it is: an explicit act. This
/// answers "can this person prove who they are RIGHT NOW", which is exactly the
/// question a launch should be asking, freshly, rather than inheriting a verdict
/// some earlier pass cached.
///
/// # Which standings withhold, and which deliberately do not
///
/// Only the two that never heal on their own: a conflicting enrolment
/// ([`PersonEnrolment::RotationPending`]) and a key that does not parse
/// ([`PersonEnrolment::Unusable`]). A person with NO key yet is not withheld —
/// [`provision_people`] mints and enrols one on this very pass, so withholding
/// them would be withholding the ordinary first boot of every new hire. A trust
/// table that cannot be READ is likewise not a refusal: a transient store fault
/// must not be able to withhold an entire company at once, and the verify
/// middleware already fails closed on the same fault.
pub async fn identity_launch_refusals(
    db: &CompanyDb,
    dir: &Path,
    chief_person_id: &str,
    people: impl IntoIterator<Item = String>,
) -> BTreeMap<String, String> {
    let mut refusals = BTreeMap::new();
    for person_id in people {
        let key_path = person_identity_key_path(dir, chief_person_id, &person_id);
        // No key on disk is not this gate's business: the provisioning pass
        // mints one. The on-disk launch gate refuses a person with no home.
        let Ok(pem) = std::fs::read_to_string(&key_path) else { continue };
        let reason = match key_material(&pem) {
            Err(error) => format!(
                "this person's identity key is unusable ({error}), so they cannot \
                 authenticate to chiefd and would exit seconds after starting; remove \
                 {} and the next pass mints and enrols a fresh key",
                key_path.display()
            ),
            Ok((_, fingerprint)) => match db.identity_read(person_id.clone()).await {
                Ok(Some(enrolled)) if enrolled.fingerprint != fingerprint => format!(
                    "a different identity key is already enrolled for this person; \
                     rotation is explicit and has not been performed, so they cannot \
                     authenticate to chiefd and would exit seconds after starting; \
                     rotate the enrolled identity deliberately or restore the key that \
                     matches it ({})",
                    key_path.display()
                ),
                _ => continue,
            },
        };
        refusals.insert(person_id, reason);
    }
    refusals
}

/// [`provision_person_identity`] for a whole roster, in order, reporting each
/// outcome — the shape [`enrol_people`] has, and sequential for the same
/// reason.
pub async fn provision_people(
    db: &CompanyDb,
    dir: &Path,
    chief_person_id: &str,
    people: impl IntoIterator<Item = String>,
    mint: &dyn IdentityKeyMint,
) -> Vec<(String, PersonEnrolment)> {
    let company_slug = db.label().to_owned();
    let mut outcomes = Vec::new();
    for person_id in people {
        let key_path = person_identity_key_path(dir, chief_person_id, &person_id);
        let outcome = provision_person_identity_at(db, &key_path, &person_id, mint).await;
        report(company_slug.as_str(), &key_path, &person_id, &outcome);
        outcomes.push((person_id, outcome));
    }
    summarize(company_slug.as_str(), &outcomes);
    outcomes
}

/// Enrol every named person, in order, and report each outcome.
///
/// Ordered and sequential on purpose: enrolment is one small actor transaction
/// each, and a roster-sized burst of concurrent writes would queue behind the
/// single writer anyway while making the log unreadable.
pub async fn enrol_people(
    db: &CompanyDb,
    dir: &Path,
    people: impl IntoIterator<Item = String>,
) -> Vec<(String, PersonEnrolment)> {
    let company_slug = db.label().to_owned();
    let mut outcomes = Vec::new();
    for person_id in people {
        let outcome = enrol_person(db, dir, &person_id).await;
        report(company_slug.as_str(), &identity_key_path(dir, &person_id), &person_id, &outcome);
        outcomes.push((person_id, outcome));
    }
    summarize(company_slug.as_str(), &outcomes);
    outcomes
}

/// The one log line per person, shared by the enrol-only and the provisioning
/// pass so the two cannot drift into describing the same outcome differently.
fn report(company_slug: &str, key_path: &Path, person_id: &str, outcome: &PersonEnrolment) {
    match outcome {
        PersonEnrolment::Enrolled => tracing::info!(
            company = company_slug,
            person = person_id,
            "agent-auth: person identity enrolled from its key"
        ),
        PersonEnrolment::Unusable(error) => tracing::error!(
            company = company_slug,
            person = person_id,
            %error,
            "agent-auth: person identity key is unusable; that person cannot authenticate"
        ),
        PersonEnrolment::RotationPending => tracing::warn!(
            company = company_slug,
            person = person_id,
            "agent-auth: a different key is already enrolled for this person; rotation is explicit and was not performed"
        ),
        PersonEnrolment::Failed(error) => tracing::error!(
            company = company_slug,
            person = person_id,
            %error,
            "agent-auth: person identity enrolment failed; the next pass retries it"
        ),
        // NOT silent, since #751/P7's follow-up. This is the one state you
        // most need to see — "this person has no identity and therefore
        // cannot authenticate" — and it emitted nothing, so an empty
        // `identities` table looked exactly like a fully enrolled one in
        // the log. Two agents spent an evening on a red test that this line
        // would have explained in one grep. It is unreachable from
        // `provision_people`, which mints what it cannot find.
        PersonEnrolment::NoKey => tracing::warn!(
            company = company_slug,
            person = person_id,
            key = %key_path.display(),
            "agent-auth: no identity key in this person's agent home; they cannot authenticate until one is provisioned"
        ),
        PersonEnrolment::AlreadyEnrolled => {}
    }
}

/// One summary per pass, so "did enrolment run at all?" is answerable without
/// inferring it from the absence of per-person lines. An absence is not an
/// answer: it reads identically whether every person was already enrolled or
/// the pass never happened.
fn summarize(company_slug: &str, outcomes: &[(String, PersonEnrolment)]) {
    let authenticable = outcomes.iter().filter(|(_, outcome)| outcome.is_authenticable()).count();
    tracing::info!(
        company = company_slug,
        people = outcomes.len(),
        authenticable,
        "agent-auth: person identity enrolment pass complete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use chiefd_core::store::COMPANY_DB_FILENAME;
    use chiefd_core::test_support::ManualClock;
    use std::sync::Arc;

    use p256::ecdsa::SigningKey;
    use p256::pkcs8::{EncodePrivateKey, LineEnding};

    const SLUG: &str = "cobalt";

    struct Fixture {
        db: Arc<CompanyDb>,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn open() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let db = Arc::new(
                CompanyDb::open(
                    SLUG,
                    &dir.path().join(COMPANY_DB_FILENAME),
                    Arc::new(ManualClock::starting_at(0, 1_700_000_000_000)),
                )
                .expect("open company"),
            );
            Self { db, dir }
        }

        /// The COMPANY DIRECTORY — `<dir>`, the one an operator ran `chief`
        /// in. The agent home hangs off `<dir>/.chief/agent/<id>/`.
        fn company_dir(&self) -> std::path::PathBuf {
            self.dir.path().join("company")
        }

        /// Write a person's identity key exactly where the hire path puts it.
        fn write_key(&self, person_id: &str, seed: u8) -> String {
            let key = SigningKey::from_slice(&[seed; 32]).expect("key");
            let pem = key.to_pkcs8_pem(LineEnding::LF).expect("pem").to_string();
            let path = identity_key_path(&self.company_dir(), person_id);
            crate::files::publish_atomically(&path, &pem, 0o600).expect("write");
            pem
        }
    }

    #[tokio::test]
    async fn a_materialized_key_becomes_an_enrolled_person_identity() {
        let f = Fixture::open();
        let pem = f.write_key("quant-head", 7);

        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "quant-head").await,
            PersonEnrolment::Enrolled
        );

        let identity =
            f.db.identity_read("quant-head".to_owned()).await.expect("read").expect("present");
        assert_eq!(identity.kind, IdentityKind::Person);
        assert_eq!(
            identity.company_slug.as_deref(),
            Some(f.db.label()),
            "the company recorded is the actor's own label — the value /v1/org/* routes compare"
        );
        assert!(identity.active);
        assert_eq!(
            identity.pubkey.as_deref(),
            Some(public_spki_base64(&pem).expect("spki").as_str()),
            "the enrolled key is the public half of the one on disk"
        );

        // A second pass is the steady state, not a re-key.
        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "quant-head").await,
            PersonEnrolment::AlreadyEnrolled
        );
    }

    /// The negative direction. A person with no key on disk must NOT acquire an
    /// identity — an enrolment that invented one would hand out authority the
    /// person cannot exercise and cannot revoke.
    #[tokio::test]
    async fn a_person_with_no_key_is_not_enrolled() {
        let f = Fixture::open();
        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "never-materialized").await,
            PersonEnrolment::NoKey
        );
        assert!(f.db.identity_read("never-materialized".to_owned()).await.expect("read").is_none());
    }

    #[tokio::test]
    async fn an_unparseable_key_is_reported_rather_than_replaced() {
        let f = Fixture::open();
        let path = identity_key_path(&f.company_dir(), "quant-head");
        crate::files::publish_atomically(
            &path,
            "-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----\n",
            0o600,
        )
        .expect("write");

        let outcome = enrol_person(&f.db, &f.company_dir(), "quant-head").await;
        assert!(matches!(outcome, PersonEnrolment::Unusable(_)), "{outcome:?}");
        assert!(!outcome.is_authenticable());
        assert!(f.db.identity_read("quant-head".to_owned()).await.expect("read").is_none());
        // And the file is untouched — this module never mints.
        assert!(std::fs::read_to_string(&path).expect("read").contains("nope"));
    }

    /// Swapping the key file must never silently re-point the enrolled anchor:
    /// that is exactly the blind spot revocation would have.
    #[tokio::test]
    async fn a_swapped_key_is_a_pending_rotation_not_a_silent_re_key() {
        let f = Fixture::open();
        let original = f.write_key("quant-head", 7);
        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "quant-head").await,
            PersonEnrolment::Enrolled
        );

        f.write_key("quant-head", 9);
        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "quant-head").await,
            PersonEnrolment::RotationPending
        );
        assert_eq!(
            f.db.identity_read("quant-head".to_owned())
                .await
                .expect("read")
                .expect("present")
                .pubkey
                .as_deref(),
            Some(public_spki_base64(&original).expect("spki").as_str()),
            "the original anchor still stands"
        );
    }

    /// THE FAST PATH KEYS ON THE CREDENTIAL, NEVER ON THE ROW. A short-circuit
    /// on "this person already has an identity" would keep a stale pubkey
    /// forever once the key on disk changed: the row exists, the write is
    /// skipped, and the person signs with a key the daemon does not hold. That
    /// failure is silent until authentication and reads exactly like the defect
    /// this module exists to close.
    ///
    /// Asserted from BOTH directions on ONE fixture, because either half alone
    /// is passable by a wrong implementation: an unchanged key must take the
    /// read-only path (`AlreadyEnrolled`, no write), and a CHANGED key must
    /// fall through to the actor and be adjudicated there.
    #[tokio::test]
    async fn the_already_enrolled_fast_path_compares_the_key_and_not_the_row() {
        let f = Fixture::open();
        f.write_key("quant-head", 7);
        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "quant-head").await,
            PersonEnrolment::Enrolled
        );
        assert_eq!(
            enrol_person(&f.db, &f.company_dir(), "quant-head").await,
            PersonEnrolment::AlreadyEnrolled,
            "the same key is the steady state and costs a read"
        );

        // Same row, DIFFERENT key on disk. The fast path must not claim this is
        // the steady state.
        f.write_key("quant-head", 9);
        let outcome = enrol_person(&f.db, &f.company_dir(), "quant-head").await;
        assert_ne!(
            outcome,
            PersonEnrolment::AlreadyEnrolled,
            "a re-minted or rotated key must never be reported as already enrolled"
        );
        assert_eq!(
            outcome,
            PersonEnrolment::RotationPending,
            "it reaches the actor, which refuses a silent re-key"
        );
    }

    #[tokio::test]
    async fn a_roster_enrols_every_person_that_has_a_key() {
        let f = Fixture::open();
        f.write_key("quant-head", 3);
        f.write_key("signal-researcher", 4);

        let outcomes = enrol_people(
            &f.db,
            &f.company_dir(),
            ["quant-head".to_owned(), "signal-researcher".to_owned(), "contained".to_owned()],
        )
        .await;

        assert_eq!(
            outcomes,
            vec![
                ("quant-head".to_owned(), PersonEnrolment::Enrolled),
                ("signal-researcher".to_owned(), PersonEnrolment::Enrolled),
                ("contained".to_owned(), PersonEnrolment::NoKey),
            ]
        );
    }

    /// The regression the tool contract caught. A person's company must be the
    /// COMPOSITE key every `/v1/org/*` route carries, not the display slug: the
    /// staffing binder compares `identity.company_slug` against the request's
    /// own `slug`, so a display-slug row refuses the first authenticated call
    /// with `requester-company-mismatch`. Asserted against a label that is not
    /// its own display name, so a fixture whose two forms coincide cannot pass
    /// this by accident.
    #[tokio::test]
    async fn the_company_recorded_is_the_composite_key_the_routes_compare() {
        let dir = tempfile::tempdir().expect("tempdir");
        let composite = "cobalt@79db0b35fb4c";
        let db = Arc::new(
            CompanyDb::open(
                composite,
                &dir.path().join(COMPANY_DB_FILENAME),
                Arc::new(ManualClock::starting_at(0, 1_700_000_000_000)),
            )
            .expect("open company"),
        );
        let company_dir = dir.path().join("company");
        let key = SigningKey::from_slice(&[5; 32]).expect("key");
        let path = identity_key_path(&company_dir, "quant-head");
        crate::files::publish_atomically(
            &path,
            &key.to_pkcs8_pem(LineEnding::LF).expect("pem"),
            0o600,
        )
        .expect("write");

        assert_eq!(enrol_person(&db, &company_dir, "quant-head").await, PersonEnrolment::Enrolled);
        assert_eq!(
            db.identity_read("quant-head".to_owned())
                .await
                .expect("read")
                .expect("present")
                .company_slug
                .as_deref(),
            Some(composite),
            "the display slug 'cobalt' would be refused by every staffing route"
        );
    }

    /// A REAL P-256 mint, deterministic from a seed. `settings::TestKeyMint`
    /// yields a fixture string that does not parse, which is exactly what these
    /// tests must not have: provisioning is only proved by a key the verifier
    /// would accept.
    struct SeededMint(std::sync::atomic::AtomicU8);

    impl crate::identity_key::IdentityKeyMint for SeededMint {
        fn generate_pkcs8_pem(&self) -> Result<String, crate::materialize::MaterializeError> {
            let seed = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let key = SigningKey::from_slice(&[seed; 32]).expect("key");
            Ok(key.to_pkcs8_pem(LineEnding::LF).expect("pem").to_string())
        }
    }

    fn seeded_mint(first: u8) -> SeededMint {
        SeededMint(std::sync::atomic::AtomicU8::new(first))
    }

    /// THE RULE THIS PACKET ADDS. A person who has never been materialized
    /// still ends the call able to authenticate: provisioning mints the key it
    /// cannot find, then enrols it. Enrolment used to ride on
    /// `refresh_materialization`, so this same person was `NoKey` forever on a
    /// company that had not converged.
    #[tokio::test]
    async fn provisioning_mints_the_missing_key_and_enrols_it() {
        let f = Fixture::open();
        let key_path = identity_key_path(&f.company_dir(), "quant-head");
        assert!(!key_path.exists(), "the precondition: no materialization has run");

        assert_eq!(
            provision_person_identity(&f.db, &f.company_dir(), "quant-head", &seeded_mint(3)).await,
            PersonEnrolment::Enrolled
        );

        let pem = std::fs::read_to_string(&key_path).expect("the key was minted here");
        let identity =
            f.db.identity_read("quant-head".to_owned()).await.expect("read").expect("present");
        assert_eq!(identity.kind, IdentityKind::Person);
        assert!(identity.active);
        assert_eq!(
            identity.pubkey.as_deref(),
            Some(public_spki_base64(&pem).expect("spki").as_str()),
            "the enrolled half is the public half of the file the person signs with"
        );
    }

    /// The key is created ONCE. A second provisioning pass — a re-run of
    /// genesis-adjacent work, or a later roster call — must not re-mint, which
    /// would orphan the enrolled anchor and lock the person out.
    #[tokio::test]
    async fn provisioning_twice_never_re_mints_and_never_re_keys() {
        let f = Fixture::open();
        assert_eq!(
            provision_person_identity(&f.db, &f.company_dir(), "quant-head", &seeded_mint(3)).await,
            PersonEnrolment::Enrolled
        );
        let first = std::fs::read_to_string(identity_key_path(&f.company_dir(), "quant-head"))
            .expect("minted");

        // A mint seeded differently: if provisioning re-minted, the file would
        // change and the enrolment would report a pending rotation.
        assert_eq!(
            provision_person_identity(&f.db, &f.company_dir(), "quant-head", &seeded_mint(9)).await,
            PersonEnrolment::AlreadyEnrolled
        );
        assert_eq!(
            std::fs::read_to_string(identity_key_path(&f.company_dir(), "quant-head"))
                .expect("read"),
            first,
            "create-once: the anchor on disk is untouched"
        );
    }

    /// Provisioning does not preempt a key the materializer already wrote — the
    /// two writers share one create-once rule, so whichever ran first owns the
    /// anchor.
    #[tokio::test]
    async fn provisioning_preserves_a_key_that_already_exists() {
        let f = Fixture::open();
        let materialized = f.write_key("quant-head", 7);

        assert_eq!(
            provision_person_identity(&f.db, &f.company_dir(), "quant-head", &seeded_mint(9)).await,
            PersonEnrolment::Enrolled
        );
        assert_eq!(
            f.db.identity_read("quant-head".to_owned())
                .await
                .expect("read")
                .expect("present")
                .pubkey
                .as_deref(),
            Some(public_spki_base64(&materialized).expect("spki").as_str()),
            "the enrolled key is the one that was already on disk"
        );
    }

    /// A roster provisions everybody, and each person gets their OWN key. One
    /// shared key would make every person able to authenticate as every other.
    #[tokio::test]
    async fn a_roster_is_provisioned_person_by_person_with_distinct_keys() {
        let f = Fixture::open();
        let outcomes = provision_people(
            &f.db,
            &f.company_dir(),
            "chief",
            ["chief".to_owned(), "quant-head".to_owned()],
            &seeded_mint(3),
        )
        .await;
        assert_eq!(
            outcomes,
            vec![
                ("chief".to_owned(), PersonEnrolment::Enrolled),
                ("quant-head".to_owned(), PersonEnrolment::Enrolled),
            ]
        );
        let ceo = f.db.identity_read("chief".to_owned()).await.expect("read").expect("present");
        let head =
            f.db.identity_read("quant-head".to_owned()).await.expect("read").expect("present");
        assert_ne!(ceo.fingerprint, head.fingerprint, "one key per person, never a shared one");
        assert!(
            chief_identity_key_path(&f.company_dir()).is_file(),
            "the Chief key belongs directly under .chief"
        );
        assert!(
            !crate::agent_home::agent_home(&f.company_dir(), "chief").exists(),
            "provisioning the Chief identity must not invent an agent home"
        );
        assert!(
            identity_key_path(&f.company_dir(), "quant-head").is_file(),
            "an agent key remains inside that agent's home"
        );
    }

    /// The negative that must survive this change: provisioning enrols the
    /// people it is GIVEN and invents nobody. "Everyone is enrolled" and "the
    /// right people are enrolled" look identical from the positive direction.
    #[tokio::test]
    async fn provisioning_a_roster_enrols_nobody_outside_it() {
        let f = Fixture::open();
        provision_people(&f.db, &f.company_dir(), "chief", ["chief".to_owned()], &seeded_mint(3))
            .await;
        assert!(
            f.db.identity_read("quant-head".to_owned()).await.expect("read").is_none(),
            "a person not named by the roster acquires no identity"
        );
    }

    /// THE DEFECT THIS PACKET CLOSES, END TO END. Four provisioning passes ran
    /// at once inside ONE daemon on a real company (`4cc439341aa9`,
    /// 2026-08-20T00:23:11Z). Two of them minted a key for the same person:
    /// the first enrolled its key, and the second — already past the existence
    /// check and holding a candidate of its own — then published that
    /// candidate over the file. The trust table and the disk disagreed for
    /// ever after, and the launch gate withheld six of twenty-one people with
    /// "a different identity key is already enrolled for this person".
    ///
    /// Reproduced exactly, and without a sleep: the late minter is held INSIDE
    /// its mint, past `path.exists()`, while the winning pass mints and enrols.
    /// Releasing it must change nothing on disk.
    #[tokio::test]
    async fn a_late_minter_never_orphans_the_key_that_was_already_enrolled() {
        use std::sync::mpsc;
        use std::sync::Mutex;

        /// A mint that announces it has been entered and then waits for
        /// permission to return — the seam that makes the two-writer
        /// interleaving deterministic.
        struct GatedMint {
            seed: u8,
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl crate::identity_key::IdentityKeyMint for GatedMint {
            fn generate_pkcs8_pem(&self) -> Result<String, crate::materialize::MaterializeError> {
                self.entered.send(()).expect("announce the mint");
                self.release.lock().expect("release lock").recv().expect("wait for release");
                let key = SigningKey::from_slice(&[self.seed; 32]).expect("key");
                Ok(key.to_pkcs8_pem(LineEnding::LF).expect("pem").to_string())
            }
        }

        let f = Fixture::open();
        let dir = f.company_dir();
        let key_path = identity_key_path(&dir, "quant-head");
        assert!(!key_path.exists(), "the precondition: a first provision");

        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let late_path = key_path.clone();
        let late = std::thread::spawn(move || {
            let mint = GatedMint { seed: 9, entered, release: Mutex::new(release_rx) };
            crate::identity_key::ensure_identity_key(&late_path, &mint).expect("ensure")
        });
        entered_rx.recv().expect("the late pass is past the existence check");

        // The winning pass, while the late one is held mid-mint.
        assert_eq!(
            provision_person_identity(&f.db, &dir, "quant-head", &seeded_mint(3)).await,
            PersonEnrolment::Enrolled
        );
        let enrolled = std::fs::read_to_string(&key_path).expect("the winner minted here");

        release.send(()).expect("release the late minter");
        let late_created = late.join().expect("join");

        // The operator-visible symptom first: this is the card on the glass.
        let refusals =
            identity_launch_refusals(&f.db, &dir, "chief", ["quant-head".to_owned()]).await;
        assert!(
            refusals.is_empty(),
            "a person must never be withheld from launch because two passes raced: {refusals:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&key_path).expect("read"),
            enrolled,
            "create-once: a late minter never publishes over an anchor that exists"
        );
        assert!(
            !late_created,
            "the late minter lost and must not report that it created the anchor"
        );
    }

    /// The refusal keeps its subject. A key genuinely swapped underneath an
    /// enrolled person is still withheld, by name — the case the gate exists
    /// for, and the one the fix above must not soften.
    #[tokio::test]
    async fn a_swapped_key_still_withholds_that_person_from_launch() {
        let f = Fixture::open();
        let dir = f.company_dir();
        assert_eq!(
            provision_person_identity(&f.db, &dir, "quant-head", &seeded_mint(3)).await,
            PersonEnrolment::Enrolled
        );
        f.write_key("quant-head", 9);

        let refusals =
            identity_launch_refusals(&f.db, &dir, "chief", ["quant-head".to_owned()]).await;
        assert!(
            refusals.get("quant-head").is_some_and(
                |reason| reason.contains("a different identity key is already enrolled")
            ),
            "{refusals:?}"
        );
    }

    /// A company created in a directory a PREVIOUS company used keeps that
    /// directory's key material. It is ADOPTED, never refused: the new
    /// company's trust table is empty, so the key on disk becomes the anchor.
    /// Stated at the launch gate as well as at enrolment, because the operator
    /// meets this as "can this person start", not as an enrolment outcome.
    #[tokio::test]
    async fn a_key_left_by_a_previous_company_is_adopted_and_starts_its_person() {
        let f = Fixture::open();
        let dir = f.company_dir();
        let survivor = f.write_key("quant-head", 7);
        assert!(
            f.db.identity_read("quant-head".to_owned()).await.expect("read").is_none(),
            "the precondition: a fresh company enrols nobody yet"
        );

        assert_eq!(
            provision_person_identity(&f.db, &dir, "quant-head", &seeded_mint(9)).await,
            PersonEnrolment::Enrolled
        );
        assert_eq!(
            f.db.identity_read("quant-head".to_owned())
                .await
                .expect("read")
                .expect("present")
                .pubkey
                .as_deref(),
            Some(public_spki_base64(&survivor).expect("spki").as_str())
        );
        assert!(
            identity_launch_refusals(&f.db, &dir, "chief", ["quant-head".to_owned()])
                .await
                .is_empty(),
            "a reused directory must not withhold anybody"
        );
    }

    /// The fingerprint is the JWT `kid` and the revocation anchor, so this
    /// crate's definition and the verifier's must be the same bytes. Recomputed
    /// here from the documented definition rather than from either
    /// implementation, so agreeing on a shared bug is not a pass.
    #[test]
    fn the_fingerprint_matches_the_verifiers_definition() {
        let spki = b"any-spki-der-bytes";
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(<Sha256 as Digest>::digest(spki));
        assert_eq!(fingerprint_of_spki(spki), expected);
        assert_eq!(fingerprint_of_spki(spki).len(), 43, "32 bytes base64url-no-pad");
    }
}
