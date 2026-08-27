//! Cryptographic caller identities (agent-auth P0, the design record).
//!
//! A typed relational table chiefd owns directly, read by the verify-middleware
//! on every `/v1` request. The revocation anchor is the KEY, never the token:
//! `active = 0` locks an identity out, and a JWT's `kid` must equal the row's
//! CURRENT [`Identity::fingerprint`], so rotating the fingerprint invalidates
//! every token an identity ever held without deleting the row.
//!
//! Two identities may share one `principal` (the operator's keypair plus its
//! `operator-pane` and `operator-remote` channels). Authorization keys on the
//! principal; enrolment and revocation key on the identity.
//!
//! These narrow row helpers run only through [`crate::actor::CompanyDb`]. The
//! DDL lives in [`crate::schema::COMPANY_SCHEMA_SQL`], applied idempotently at
//! company open.

use rusqlite::{Connection, OptionalExtension};

/// What kind of principal an identity represents. `channel` identities are
/// attested by the daemon server-side (pi-pane, operator-pane) and carry no
/// public key; every other kind authenticates by signature and MUST have one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKind {
    /// A person/agent in the org, authenticating with its own keypair.
    Person,
    /// The operator principal's own keypair identity (CLI / remote tooling).
    Operator,
    /// A non-human service authenticating with a keypair.
    Service,
    /// A daemon-terminated inbound channel (pi-pane, operator-pane). No
    /// pubkey; its `fingerprint` is a random epoch, and tokens are minted
    /// server-side after channel attestation.
    Channel,
}

impl IdentityKind {
    /// The stored token, matching the `CHECK(kind IN (...))` clause.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Operator => "operator",
            Self::Service => "service",
            Self::Channel => "channel",
        }
    }

    /// Parse a stored token back to the enum, or `None` for an unknown value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "person" => Some(Self::Person),
            "operator" => Some(Self::Operator),
            "service" => Some(Self::Service),
            "channel" => Some(Self::Channel),
            _ => None,
        }
    }

    /// Whether this kind is attested server-side (no pubkey) rather than by a
    /// caller-held keypair signature.
    #[must_use]
    pub fn is_channel(self) -> bool {
        matches!(self, Self::Channel)
    }
}

/// One enrolled identity row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Stable identity id; the JWT `sub`.
    pub identity_id: String,
    /// The principal this identity acts as; authorization keys on this.
    pub principal: String,
    /// Person / operator / service / channel.
    pub kind: IdentityKind,
    /// The company a person belongs to. `Some` iff `kind == Person`; `None` for
    /// every daemon-scoped identity (operator, channel, service, bootstrap).
    pub company_slug: Option<String>,
    /// Opaque SPKI public key value (base64). `None` iff `kind == Channel`.
    pub pubkey: Option<String>,
    /// Current key fingerprint (keypair) or channel epoch. The JWT `kid` must
    /// equal this for a token to verify.
    pub fingerprint: String,
    /// Whether the identity may authenticate at all. `false` = revoked.
    pub active: bool,
    /// When the row was enrolled (ms since epoch).
    pub enrolled_at: i64,
    /// Which identity enrolled this one (`None` for the boot self-enrol).
    pub enrolled_by: Option<String>,
    /// When the identity was revoked, if it has been.
    pub revoked_at: Option<i64>,
}

/// The fields needed to enrol a new identity. `pubkey` MUST be `Some` for a
/// keypair identity and `None` for a channel — the DDL `CHECK` enforces it.
#[derive(Debug, Clone)]
pub struct NewIdentity<'a> {
    /// Stable identity id (the JWT `sub`).
    pub identity_id: &'a str,
    /// The principal the identity acts as.
    pub principal: &'a str,
    /// Person / operator / service / channel.
    pub kind: IdentityKind,
    /// The company a person belongs to. Must be `Some` iff `kind == Person`
    /// (the DDL `CHECK` enforces it).
    pub company_slug: Option<&'a str>,
    /// Opaque SPKI public key value; `None` iff `kind == Channel`.
    pub pubkey: Option<&'a str>,
    /// Key fingerprint or channel epoch. Must be unique across the table.
    pub fingerprint: &'a str,
    /// Which identity enrolled this one; `None` for the boot self-enrol.
    pub enrolled_by: Option<&'a str>,
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Identity> {
    let kind_raw: String = row.get(2)?;
    let kind = IdentityKind::parse(&kind_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            format!("unknown identity kind '{kind_raw}'").into(),
        )
    })?;
    Ok(Identity {
        identity_id: row.get(0)?,
        principal: row.get(1)?,
        kind,
        company_slug: row.get(3)?,
        pubkey: row.get(4)?,
        fingerprint: row.get(5)?,
        active: row.get(6)?,
        enrolled_at: row.get(7)?,
        enrolled_by: row.get(8)?,
        revoked_at: row.get(9)?,
    })
}

const SELECT_COLUMNS: &str = "identity_id, principal, kind, company_slug, pubkey, \
     fingerprint, active, enrolled_at, enrolled_by, revoked_at";

/// Enrol a new identity, active. Fails if `identity_id` already exists or the
/// `fingerprint` collides — a fresh enrolment is a distinct identity, never a
/// silent overwrite of an existing key.
///
/// # Errors
/// Propagates `rusqlite` failures (including the PK / UNIQUE / CHECK violations).
pub fn enroll(conn: &Connection, new: &NewIdentity<'_>, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO identities\
         (identity_id, principal, kind, company_slug, pubkey, fingerprint, active, enrolled_at, enrolled_by, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, NULL)",
        rusqlite::params![
            new.identity_id,
            new.principal,
            new.kind.as_str(),
            new.company_slug,
            new.pubkey,
            new.fingerprint,
            now,
            new.enrolled_by,
        ],
    )?;
    Ok(())
}

/// Enrol only if `identity_id` is absent. Returns whether a row was inserted.
/// This is the idempotent boot path for the bootstrap operator: re-running at
/// every daemon start is a no-op once enrolled, and it never overwrites a
/// rotated key.
///
/// # Errors
/// Propagates `rusqlite` failures.
pub fn enroll_if_absent(
    conn: &Connection,
    new: &NewIdentity<'_>,
    now: i64,
) -> rusqlite::Result<bool> {
    let inserted = conn.execute(
        "INSERT INTO identities\
         (identity_id, principal, kind, company_slug, pubkey, fingerprint, active, enrolled_at, enrolled_by, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, NULL) \
         ON CONFLICT(identity_id) DO NOTHING",
        rusqlite::params![
            new.identity_id,
            new.principal,
            new.kind.as_str(),
            new.company_slug,
            new.pubkey,
            new.fingerprint,
            now,
            new.enrolled_by,
        ],
    )?;
    Ok(inserted == 1)
}

/// Fetch one identity by id. `None` if it was never enrolled.
///
/// This is the single indexed read the verify-middleware performs per request
/// (PK lookup).
///
/// # Errors
/// Propagates `rusqlite` failures and a corrupt `kind` value.
pub fn get(conn: &Connection, identity_id: &str) -> rusqlite::Result<Option<Identity>> {
    conn.query_row(
        &format!("SELECT {SELECT_COLUMNS} FROM identities WHERE identity_id = ?1"),
        rusqlite::params![identity_id],
        map_row,
    )
    .optional()
}

/// Every identity sharing a principal, id order. Used to resolve the operator's
/// keypair + channel identities together.
///
/// # Errors
/// Propagates `rusqlite` failures and a corrupt `kind` value.
pub fn list_by_principal(conn: &Connection, principal: &str) -> rusqlite::Result<Vec<Identity>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {SELECT_COLUMNS} FROM identities WHERE principal = ?1 ORDER BY identity_id ASC"
    ))?;
    let rows = statement.query_map(rusqlite::params![principal], map_row)?;
    rows.collect()
}

/// Revoke an identity: `active = 0`, stamp `revoked_at`. Idempotent — revoking
/// an already-revoked identity leaves the original `revoked_at`. Returns the
/// number of rows changed (0 if the identity does not exist or was already
/// revoked). Every token the identity holds stops verifying immediately, because
/// the middleware requires `active = 1`.
///
/// # Errors
/// Propagates `rusqlite` failures.
pub fn revoke(conn: &Connection, identity_id: &str, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE identities SET active = 0, revoked_at = ?2 \
         WHERE identity_id = ?1 AND active = 1",
        rusqlite::params![identity_id, now],
    )
}

/// Rotate an identity's fingerprint to a new value (a new key fingerprint for a
/// keypair identity, or a fresh epoch for a channel). Every previously-minted
/// token 403s afterwards (its `kid` no longer matches) while the identity stays
/// `active` — the non-disabling invalidation channels rely on. Returns rows
/// changed.
///
/// # Errors
/// Propagates `rusqlite` failures (including a UNIQUE collision on the new
/// fingerprint).
pub fn rotate_fingerprint(
    conn: &Connection,
    identity_id: &str,
    new_fingerprint: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE identities SET fingerprint = ?2 WHERE identity_id = ?1",
        rusqlite::params![identity_id, new_fingerprint],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("company schema applies");
        conn
    }

    fn keypair<'a>(id: &'a str, principal: &'a str, fp: &'a str) -> NewIdentity<'a> {
        NewIdentity {
            identity_id: id,
            principal,
            kind: IdentityKind::Person,
            // A person MUST carry a company slug (DDL CHECK).
            company_slug: Some("acme"),
            pubkey: Some("spki-test-pubkey"),
            fingerprint: fp,
            enrolled_by: None,
        }
    }

    #[test]
    fn enroll_then_get_roundtrips_active() {
        let conn = conn();
        enroll(&conn, &keypair("p1", "person:p1", "fp-1"), 100).expect("enroll");
        let got = get(&conn, "p1").expect("get").expect("present");
        assert_eq!(got.identity_id, "p1");
        assert_eq!(got.principal, "person:p1");
        assert_eq!(got.kind, IdentityKind::Person);
        assert_eq!(got.company_slug.as_deref(), Some("acme"));
        assert_eq!(got.pubkey.as_deref(), Some("spki-test-pubkey"));
        assert_eq!(got.fingerprint, "fp-1");
        assert!(got.active);
        assert_eq!(got.revoked_at, None);
    }

    #[test]
    fn get_absent_is_none() {
        let conn = conn();
        assert_eq!(get(&conn, "nobody").expect("get"), None);
    }

    #[test]
    fn duplicate_fingerprint_is_rejected() {
        let conn = conn();
        enroll(&conn, &keypair("p1", "person:p1", "shared-fp"), 1).expect("first");
        let err = enroll(&conn, &keypair("p2", "person:p2", "shared-fp"), 2);
        assert!(err.is_err(), "UNIQUE(fingerprint) must reject a collision");
    }

    #[test]
    fn channel_without_pubkey_is_allowed_keypair_without_is_not() {
        let conn = conn();
        // A channel row: no pubkey, epoch fingerprint — permitted.
        conn.execute(
            "INSERT INTO identities(identity_id, principal, kind, pubkey, fingerprint, active, enrolled_at) \
             VALUES('operator-pane', 'operator', 'channel', NULL, 'epoch-1', 1, 5)",
            [],
        )
        .expect("channel row inserts");
        // A person row WITHOUT a pubkey violates the CHECK (company_slug set so
        // ONLY the pubkey coherence CHECK can be the cause).
        let bad = conn.execute(
            "INSERT INTO identities(identity_id, principal, kind, company_slug, pubkey, fingerprint, active, enrolled_at) \
             VALUES('p3', 'person:p3', 'person', 'acme', NULL, 'fp-3', 1, 6)",
            [],
        );
        assert!(bad.is_err(), "non-channel without pubkey must fail the CHECK");
        // A channel WITH a pubkey also violates the CHECK.
        let bad2 = conn.execute(
            "INSERT INTO identities(identity_id, principal, kind, pubkey, fingerprint, active, enrolled_at) \
             VALUES('ch2', 'operator', 'channel', 'spki-x', 'epoch-2', 1, 7)",
            [],
        );
        assert!(bad2.is_err(), "channel with a pubkey must fail the CHECK");
    }

    #[test]
    fn person_requires_a_company_slug_daemon_scoped_kinds_forbid_it() {
        let conn = conn();
        // A person WITHOUT a company slug violates the coherence CHECK.
        let bad_person = enroll(
            &conn,
            &NewIdentity {
                identity_id: "p-noco",
                principal: "person:p",
                kind: IdentityKind::Person,
                company_slug: None,
                pubkey: Some("spki"),
                fingerprint: "fp-noco",
                enrolled_by: None,
            },
            1,
        );
        assert!(bad_person.is_err(), "a person MUST carry a company slug");
        // An operator (daemon-scoped) WITH a company slug also violates it.
        let bad_operator = enroll(
            &conn,
            &NewIdentity {
                identity_id: "op-withco",
                principal: "operator",
                kind: IdentityKind::Operator,
                company_slug: Some("acme"),
                pubkey: Some("spki"),
                fingerprint: "fp-opco",
                enrolled_by: None,
            },
            1,
        );
        assert!(bad_operator.is_err(), "a daemon-scoped identity MUST NOT carry a slug");
        // A daemon-scoped operator with NO slug is fine.
        enroll(
            &conn,
            &NewIdentity {
                identity_id: "op-ok",
                principal: "operator",
                kind: IdentityKind::Operator,
                company_slug: None,
                pubkey: Some("spki"),
                fingerprint: "fp-opok",
                enrolled_by: None,
            },
            1,
        )
        .expect("daemon-scoped operator enrols");
        assert_eq!(get(&conn, "op-ok").expect("get").expect("present").company_slug, None);
    }

    #[test]
    fn enroll_if_absent_is_idempotent() {
        let conn = conn();
        assert!(enroll_if_absent(&conn, &keypair("op", "operator", "fp-op"), 1).expect("first"));
        // Second call: same id, DIFFERENT fingerprint must NOT overwrite.
        assert!(!enroll_if_absent(&conn, &keypair("op", "operator", "fp-op-2"), 2).expect("second"));
        let got = get(&conn, "op").expect("get").expect("present");
        assert_eq!(got.fingerprint, "fp-op", "boot re-enrol must never rotate the key");
    }

    #[test]
    fn revoke_flips_active_and_is_idempotent() {
        let conn = conn();
        enroll(&conn, &keypair("p1", "person:p1", "fp-1"), 1).expect("enroll");
        assert_eq!(revoke(&conn, "p1", 50).expect("revoke"), 1);
        let got = get(&conn, "p1").expect("get").expect("present");
        assert!(!got.active);
        assert_eq!(got.revoked_at, Some(50));
        // Second revoke changes nothing and keeps the original revoked_at.
        assert_eq!(revoke(&conn, "p1", 999).expect("revoke again"), 0);
        assert_eq!(get(&conn, "p1").expect("get").expect("present").revoked_at, Some(50));
    }

    #[test]
    fn rotate_fingerprint_changes_kid_anchor() {
        let conn = conn();
        enroll(&conn, &keypair("ch", "operator", "epoch-1"), 1).expect("enroll");
        assert_eq!(rotate_fingerprint(&conn, "ch", "epoch-2").expect("rotate"), 1);
        let got = get(&conn, "ch").expect("get").expect("present");
        assert_eq!(got.fingerprint, "epoch-2");
        assert!(got.active, "rotation invalidates tokens WITHOUT disabling the identity");
    }

    #[test]
    fn list_by_principal_groups_operator_identities() {
        let conn = conn();
        enroll(&conn, &keypair("operator-key", "operator", "fp-key"), 1).expect("key");
        conn.execute(
            "INSERT INTO identities(identity_id, principal, kind, pubkey, fingerprint, active, enrolled_at) \
             VALUES('operator-pane', 'operator', 'channel', NULL, 'epoch-pane', 1, 2)",
            [],
        )
        .expect("pane channel");
        let ids: Vec<String> = list_by_principal(&conn, "operator")
            .expect("list")
            .into_iter()
            .map(|i| i.identity_id)
            .collect();
        assert_eq!(ids, vec!["operator-key".to_string(), "operator-pane".to_string()]);
    }
}
