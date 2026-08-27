//! How a command finds its own directory's daemon.
//!
//! # Why this is a file and not a registry lookup
//!
//! A company used to be found by asking beacond for a slug. That worked only
//! because a slug was globally unique, which it never was: one slug under two
//! data roots was two companies, so the wire key had to become the composite
//! `slug@sha256(orgs_root)[..12]` and every caller had to agree on how to
//! build it. A company is a DIRECTORY now, and a directory already knows where
//! its own daemon is — so the daemon writes the answer into the directory and
//! the client reads it there. No registry is on the path between a command and
//! its own company.
//!
//! beacond survives, and is not this: it is the box-wide presence registry
//! that answers "what is running anywhere on this machine" for `chief ls` and
//! the web app. Nothing in the attach path consults it.
//!
//! # The file is a POINTER, never authority
//!
//! Mandate 5 bans manifests, projections, registries and pid files as durable
//! state, and this is none of them: every durable fact about a company is a
//! row in `<dir>/.chief/db/chief.db`. This file says only "a daemon for this
//! directory was last seen at this URL under this pid", and a reader that
//! finds it must still prove the pid is alive and the listener answers before
//! binding it. A stale file is the ordinary case after a reboot — it is
//! overwritten, never trusted.
//!
//! It lives under `.chief/run/`, which is disposable by construction: deleting
//! the whole directory costs a caller one respawn and nothing else.
//!
//! # WHO WRITES IT AND WHO READS IT — the whole inventory, because a partial
//! one cost a company
//!
//! **One writer:** `chiefd-daemon`, through [`crate::rendezvous`]'s publish.
//!
//! **Two readers, in two languages** — and the second is the point of this
//! section:
//!
//! 1. `apps/chiefd/crates/chief-cli/src/daemon.rs` — the attach ladder that
//!    decides whether to adopt a daemon or spawn one.
//! 2. `apps/chiefd/crates/chief-cli/src/stop.rs` — the stray sweep, which
//!    needs the pid this file names.
//! 3. `packages/chiefing/src/discovery/Rendezvous.ts` — the TypeScript parser
//!    inside every person's Pi pane, in another language, in another
//!    directory, in a package no Rust change ever touches. It is reached by
//!    two extensions: `packages/piing/extensions/organization-intercom.ts` and
//!    `packages/piing/extensions/team-ui.ts`.
//!
//! **The paths above are checked by a guard, not maintained by hope.**
//! `scripts/test/rendezvous-reader-inventory.test.mjs` finds every file that
//! actually reads this record and fails if one of them is missing from this
//! list. A doc that can drift is a doc that will, and this particular doc
//! drifting is what the outage below cost.
//!
//! Verified by grep at the time of writing, not recalled: `chiefd-daemon`
//! writes this file and never reads it back.
//!
//! This section used to say "`chiefd-daemon` WRITES it and `chief-cli` READS
//! it", and that sentence is how the third reader was lost. On 2026-08-26 an
//! additive field was added here, correctly and compatibly; the TypeScript
//! reader refused the record it did not model; every pane in a live company
//! exited 1 at start-up — both extensions, in every person's pane, on a field
//! neither of them reads. **Nobody was careless.** The change was reviewed, the
//! diff touched every reader anyone knew about, and the reader that was not
//! named here was not looked for — because a surface's own doc is where an
//! engineer goes to learn who consumes it, and this one answered with a number
//! that was too small.
//!
//! **The general rule, which is the transferable part: an incomplete reader
//! inventory on a surface's own doc is how a cross-language contract loses a
//! reader.** A reader that announces itself only inside its own file announces
//! itself to nobody. When you add a consumer of this file, add it here in the
//! same commit — that line is not documentation of the contract, it is part of
//! it.
//!
//! # Why it lives in this crate
//!
//! The backend/client boundary guard (rules 5 and 7) forbids `chiefd-daemon`
//! and `chief-cli` from depending on each other. A shape both sides must agree
//! on exactly, with no other home, is precisely what this crate is for — the
//! same reason [`crate::proc`] and [`crate::error`] are here. The TypeScript
//! reader cannot link this crate at all, which is exactly why the tests that
//! hold the two together live at the seam rather than inside either half.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// **THE company identity**: `sha256(canonical <dir>)[..12]`.
///
/// # Why this is one function in a leaf and not a rule two programs follow
///
/// Its predecessor was the composite `slug@sha256(orgs_root)[..12]`, and it
/// was written independently in NINE places — three sha256 implementations
/// plus six re-derivations in test harnesses, deploy scripts and extensions.
/// They drifted, and the failure mode is the worst kind: every `/v1/org/*`
/// route matches a request against the company's identity, so a caller whose
/// derivation disagreed by one byte got `404 unknown-company` for a company
/// that plainly existed.
///
/// A directory needs no composite — it is unique by construction — so the
/// whole rule is one hash of one path. It lives here because `chiefd-daemon`
/// and `chief-cli` both need it and the backend/client boundary guard forbids
/// either from depending on the other; this crate is the one place both may
/// link. Anything else that needs it reads [`DaemonRendezvous::key`] off the
/// wire rather than computing a tenth copy.
///
/// `dir` must already be canonical. Hashing a relative or symlink-laden path
/// would key one company two ways, which is the failure the composite existed
/// to paper over.
#[must_use]
pub fn company_key(dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    let mut key = String::with_capacity(COMPANY_KEY_CHARS);
    for byte in &digest[..COMPANY_KEY_CHARS / 2] {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// How many hex characters a company key carries.
pub const COMPANY_KEY_CHARS: usize = 12;

/// Is this the shape [`company_key`] produces?
///
/// Twelve lowercase hex characters. beacond validates its `key` column with
/// the same rule so that a caller filling it with a slug is refused at the
/// door rather than stored.
#[must_use]
pub fn is_company_key(key: &str) -> bool {
    key.len() == COMPANY_KEY_CHARS
        && key.chars().all(|character| character.is_ascii_hexdigit() && !character.is_uppercase())
}

/// The file name, under `<dir>/.chief/run/`.
pub const RENDEZVOUS_FILENAME: &str = "daemon.json";

/// The mode the rendezvous is published with.
///
/// Not 0600: it carries no credential, and a per-user daemon whose own tools
/// run as the same user gains nothing from tightening it. Stated as a constant
/// anyway so a future field that IS sensitive has to change this line, rather
/// than inheriting a permissive default nobody chose.
pub const RENDEZVOUS_MODE: u32 = 0o644;

/// Where the rendezvous lives for one company directory.
#[must_use]
pub fn rendezvous_path(dir: &Path) -> PathBuf {
    dir.join(".chief").join("run").join(RENDEZVOUS_FILENAME)
}

/// A daemon's published location for one company directory.
///
/// # AN UNKNOWN FIELD IS IGNORED HERE TOO, and the reason is an outage
///
/// This carried `deny_unknown_fields` on the reasoning that two programs which
/// ship together disagreeing about a field is a skew worth failing on. The
/// TypeScript reader mirrored it, and on 2026-08-26 that mirror killed a live
/// company: the daemon began publishing the additive `build` field below, the
/// extension in every person's pane refused the whole record, and every pane
/// died at start-up naming a version skew that was not happening.
///
/// The rule that outage bought applies to EVERY reader of this record, not
/// only the one that was holding the bag: **a reader of somebody else's record
/// is forward compatible, or it is a scheduled outage.** An old `chief-cli`
/// reading a rendezvous from a newer daemon is the identical trap one language
/// over, and it is closed here rather than left as the next incident.
///
/// What a reader still owes its callers is that the fields it USES are present
/// and well-formed — serde enforces exactly that for every field below, and
/// `describes` enforces the one semantic check that matters. What no reader
/// owes anybody is an opinion about a field it has never heard of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonRendezvous {
    /// The company directory this daemon serves, canonical and absolute.
    ///
    /// Carried rather than inferred from the file's own location so a reader
    /// can catch a file copied between directories — which would otherwise
    /// point one company's client at another company's daemon.
    pub dir: PathBuf,
    /// The directory-derived company key, `sha256(dir)[..12]`.
    ///
    /// Serialized rather than recomputed by the reader. The composite key it
    /// replaces was recomputed independently in nine places and drifted; one
    /// producer and one field is the whole point of the change.
    pub key: String,
    /// Where the daemon bound its docstore listener.
    pub url: String,
    /// The daemon process's pid, for the liveness ladder.
    ///
    /// A pid ALONE is not proof — pids are reused — which is why a reader
    /// probes the URL and asks the listener what it is before binding it.
    pub pid: u32,
    /// WHICH BUILD this daemon is actually running, as the daemon itself
    /// measured it at start.
    ///
    /// A REPORT, never an inference. The reader could go behind the process's
    /// back and stat `/proc/<pid>/exe`, and an earlier design did — but that
    /// path does not exist on macOS, and the Darwin call that looks like its
    /// equivalent (`PROC_PIDVNODEPATHINFO`) answers with the process's cwd and
    /// root vnodes, not its executable. A process can always stat its OWN
    /// executable, on every platform, at a moment when the file certainly
    /// still exists. So the daemon says what it is running, exactly as it says
    /// what its pid is, and one rule covers both platforms.
    ///
    /// `Option` because of the BOOTSTRAP GENERATION: a daemon from a build
    /// that predates this field publishes a rendezvous without it. That reads
    /// as "unknowable", is said out loud once, and becomes knowable for ever
    /// after that daemon is next restarted. It is not a hole — it is the one
    /// unavoidable cost of adding a fact to a durable surface, and it closes
    /// itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<ReportedBuild>,
}

/// What a component says about the executable it is running: WHERE it was
/// started from, and WHICH FILE that was.
///
/// Two facts because they answer two different questions and neither answers
/// the other. The PATH decides whether the component is in scope at all — a
/// process running out of a development tree is not a stale install, it is
/// somebody's `cargo run`, and restarting it onto the installed binary would
/// be the rule doing harm where a developer would least expect it. The
/// IDENTITY decides whether an in-scope component is current, and it is the
/// only half that can see a rebuild at an unchanged path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedBuild {
    /// The executable the component was started from.
    pub exe: PathBuf,
    /// Which file that was, as identity rather than name.
    pub identity: BuildIdentity,
}

impl ReportedBuild {
    /// What THIS process is running, measured now.
    ///
    /// Called by each component at start, while its own executable certainly
    /// still exists — which is what makes the report possible at all. A
    /// process cannot stat an executable that has already been replaced under
    /// it, and that is exactly the state this value is kept to detect later.
    #[must_use]
    pub fn of_running_process() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let identity = BuildIdentity::of_path(&exe)?;
        Some(Self { exe, identity })
    }
}

/// WHICH FILE a running program was started from, as identity rather than name.
///
/// `(device, inode)` and deliberately not a version string or a path. The
/// incident this exists for was a `0.5.0` → `0.5.0` rebuild: same declared
/// version, same install path, different bytes. A version comparison answers
/// "same, leave it" and fails the exact case that motivated the rule, and a
/// path comparison fails it too, because the path is what stayed the same.
/// The inode is what changed, because a re-release removes `versions/<v>` and
/// writes a new file there.
///
/// INODE REUSE IS NOT RARE, WHICH IS WHY THIS IS FOUR FIELDS AND NOT TWO.
///
/// The first version of this type was `(dev, ino)` alone, and it carried a
/// comment calling a reused inode "vanishingly rare" and accepting it. That
/// claim was WRONG and its own red-first test caught it in CI within the hour:
/// the test removes a file and writes a replacement at the same path — the
/// operator's rebuild, exactly — and the replacement was handed the inode the
/// deleted file had just freed. The check answered `Current` for a file that
/// had genuinely changed. A filesystem reuses a freed inode number readily,
/// and a release that rewrites `versions/<v>` is precisely the workload that
/// frees one and immediately allocates another.
///
/// So the identity is `(dev, ino, size, mtime)`. This is not mtime ARITHMETIC
/// — nothing is compared for older-or-newer, and no clock is trusted — it is
/// simply more of the same fingerprint. Two files are the same build when all
/// four agree, and a rebuild that reused an inode still moves the modification
/// time, and usually the size as well.
///
/// The residual is now genuinely small and is stated rather than dismissed: a
/// replacement that lands on the same device, the same reused inode, the same
/// byte length AND the same nanosecond timestamp. Its failure mode is exactly
/// today's behaviour — a stale component nobody restarted — and there is no
/// cheaper fingerprint that closes it. Hashing the file would, at the cost of
/// reading tens of megabytes during every component's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildIdentity {
    /// The device the executable's inode lives on.
    pub dev: u64,
    /// The executable's inode number.
    pub ino: u64,
    /// The executable's length in bytes.
    pub size: u64,
    /// Seconds of the executable's modification time.
    pub mtime_s: i64,
    /// Nanoseconds of the executable's modification time.
    pub mtime_ns: i64,
}

impl BuildIdentity {
    /// The identity of a file on disk, or `None` when it cannot be read.
    #[must_use]
    pub fn of_path(path: &Path) -> Option<Self> {
        Self::of_metadata(&std::fs::metadata(path).ok()?)
    }

    /// The identity carried by already-read metadata.
    #[must_use]
    pub fn of_metadata(metadata: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.size(),
            mtime_s: metadata.mtime(),
            mtime_ns: metadata.mtime_nsec(),
        })
    }
}

impl std::fmt::Display for BuildIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} ({} bytes, mtime {}.{:09})",
            self.dev, self.ino, self.size, self.mtime_s, self.mtime_ns
        )
    }
}

impl DaemonRendezvous {
    /// Does this file describe the directory the caller is standing in?
    ///
    /// A copied or moved rendezvous names a directory that is not this one,
    /// and answering "no" is what stops a client binding a foreign daemon.
    #[must_use]
    pub fn describes(&self, dir: &Path) -> bool {
        self.dir == dir
    }
}

#[cfg(test)]
mod tests {
    use super::{
        company_key, is_company_key, rendezvous_path, BuildIdentity, DaemonRendezvous,
        ReportedBuild, RENDEZVOUS_FILENAME,
    };
    use std::path::{Path, PathBuf};

    fn sample() -> DaemonRendezvous {
        DaemonRendezvous {
            dir: PathBuf::from("/work/anvils"),
            key: "0123456789ab".to_owned(),
            url: "http://127.0.0.1:8793".to_owned(),
            pid: 4242,
            build: None,
        }
    }

    /// TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME.
    ///
    /// The case the old global slug registry could not represent at all, and
    /// the reason the identity is the directory rather than the slug.
    #[test]
    fn the_company_key_separates_two_directories_and_is_stable_for_one() {
        let first = company_key(Path::new("/work/acme"));
        let second = company_key(Path::new("/elsewhere/acme"));
        assert_ne!(first, second, "same name, different directories, different companies");
        assert_eq!(first, company_key(Path::new("/work/acme")), "and it is a pure function");
        assert!(is_company_key(&first), "it produces its own shape: {first}");
    }

    /// The shape check is what stops a caller filling the field with a slug.
    #[test]
    fn only_twelve_lowercase_hex_characters_are_a_company_key() {
        assert!(is_company_key("0123456789ab"));
        for bad in
            ["", "0123456789", "0123456789abc", "0123456789AB", "0123456789ag", "anvil-works"]
        {
            assert!(!is_company_key(bad), "{bad} is not a company key");
        }
    }

    /// THE BOOTSTRAP GENERATION, pinned: a rendezvous written by a build that
    /// predates the field must still parse. An old file that failed to parse
    /// would read to the client as "no daemon here" and get a second daemon
    /// spawned over a live one.
    ///
    /// Its mirror — a NEWER file read by an older build — is the direction
    /// that actually took a company down, and is pinned two tests below.
    #[test]
    fn a_rendezvous_without_a_build_identity_still_parses_and_reads_unknowable() {
        let old = r#"{"dir":"/work/anvils","key":"aaaaaaaaaaaa","url":"http://127.0.0.1:8793","pid":4242}"#;
        let parsed: DaemonRendezvous = serde_json::from_str(old).expect("an older rendezvous");
        assert_eq!(parsed.build, None, "nothing was reported, so nothing is known");
        assert_eq!(parsed.pid, 4242, "and every field that WAS written survives");
    }

    /// And a rendezvous that carries one round-trips it.
    #[test]
    fn a_reported_build_identity_survives_the_round_trip() {
        let mut published = sample();
        published.build = Some(ReportedBuild {
            exe: PathBuf::from("/home/op/.chief/versions/0.5.0/bin/chiefd"),
            identity: BuildIdentity {
                dev: 24,
                ino: 193_693,
                size: 41_235_968,
                mtime_s: 1_756_000_000,
                mtime_ns: 123_456_789,
            },
        });
        let body = serde_json::to_string(&published).expect("render");
        assert!(body.contains("\"build\""), "the field is written: {body}");
        let parsed: DaemonRendezvous = serde_json::from_str(&body).expect("parse");
        assert_eq!(parsed, published);
    }

    /// THE FIXTURE WRITES REAL FILES, AND MUST. The seam rule that bans
    /// `std::fs::write`/`remove_file` is about production filesystem effects
    /// belonging to a host transaction; there is none in a unit test, and what
    /// is under test is what `stat` answers about real inodes on a real
    /// filesystem. A mock would assert the mock — and the defect this very
    /// test caught (a replacement handed the freed inode, reading as "same
    /// build") belongs to the allocator, not to us, so no mock could show it.
    ///
    /// The identity of a file is its inode, and REPLACING the file at the same
    /// path changes it. This is the operator's `0.5.0` -> `0.5.0` rebuild in
    /// one test: same name, same version, different build.
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn replacing_a_file_at_the_same_path_changes_its_identity() {
        let home = tempfile::tempdir().expect("tempdir");
        let path = home.path().join("chiefd");
        std::fs::write(&path, b"first build").expect("write");
        let before = BuildIdentity::of_path(&path).expect("an identity");
        std::fs::remove_file(&path).expect("remove");
        std::fs::write(&path, b"a second build, of a different length").expect("write again");
        let after = BuildIdentity::of_path(&path).expect("an identity");
        assert_ne!(before, after, "the path is the same and the build is not");
        // AND SAY WHY IT DIFFERS. The inode alone is not enough: this exact
        // sequence handed the replacement the freed inode on CI's filesystem,
        // which is what turned the first version of this type into a check
        // that answered "same" for a file that had changed.
        assert!(
            before.ino != after.ino || before.size != after.size,
            "a replaced file must differ in SOMETHING this type carries: {before} vs {after}"
        );
        assert_eq!(before, BuildIdentity::of_path(&path).map(|_| before).expect("stable"));
    }

    /// A file that is not there has no identity, and the caller must be able
    /// to tell that from "same".
    #[test]
    fn an_absent_file_has_no_identity() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(BuildIdentity::of_path(&home.path().join("absent")), None);
    }

    #[test]
    fn the_rendezvous_lives_in_the_directorys_disposable_run_folder() {
        assert_eq!(
            rendezvous_path(Path::new("/work/anvils")),
            PathBuf::from("/work/anvils/.chief/run/daemon.json")
        );
        assert_eq!(RENDEZVOUS_FILENAME, "daemon.json");
    }

    /// The wire shape both programs decode, named field by field.
    ///
    /// Asserted literally rather than through a round trip: a round trip
    /// proves the two halves of ONE declaration agree, and the point of this
    /// crate is that there is only one declaration for two programs.
    #[test]
    fn the_published_shape_is_camel_case_with_the_names_both_sides_decode() {
        let body = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(body["dir"], "/work/anvils");
        assert_eq!(body["key"], "0123456789ab");
        assert_eq!(body["url"], "http://127.0.0.1:8793");
        assert_eq!(body["pid"], 4242);
        assert_eq!(
            body.as_object().expect("object").len(),
            4,
            "no field is unaccounted for; the build identity is absent when nothing reported one"
        );

        // AND THE SAME LITERALLY FOR THE REPORTED CASE, because the whole
        // value of this field is that two programs decode the same names. A
        // round trip would agree with itself here and prove nothing.
        let mut reported = sample();
        reported.build = Some(ReportedBuild {
            exe: PathBuf::from("/home/op/.chief/versions/0.5.0/bin/chiefd"),
            identity: BuildIdentity {
                dev: 24,
                ino: 193_693,
                size: 41_235_968,
                mtime_s: 1_756_000_000,
                mtime_ns: 123_456_789,
            },
        });
        let body = serde_json::to_value(reported).expect("serialize");
        assert_eq!(body["build"]["exe"], "/home/op/.chief/versions/0.5.0/bin/chiefd");
        assert_eq!(body["build"]["identity"]["dev"], 24);
        assert_eq!(body["build"]["identity"]["ino"], 193_693);
        assert_eq!(body["build"]["identity"]["size"], 41_235_968);
        assert_eq!(body["build"]["identity"]["mtimeS"], 1_756_000_000);
        assert_eq!(body["build"]["identity"]["mtimeNs"], 123_456_789);
        assert_eq!(body.as_object().expect("object").len(), 5, "one more field, and only one");
    }

    /// THE DIRECTION AN OUTAGE REVERSED. This asserted the opposite until
    /// 2026-08-26, when the mirror of that strictness in the TypeScript reader
    /// killed every person in a live company over an additive field. A reader
    /// of somebody else's record is forward compatible or it is a scheduled
    /// outage — in both languages, since either can be the old half.
    #[test]
    fn a_field_this_build_does_not_model_is_ignored_rather_than_refused() {
        let mut body = serde_json::to_value(sample()).expect("serialize");
        body["orgsRoot"] = serde_json::json!("/home/op/.chiefd/orgs");
        let parsed: DaemonRendezvous =
            serde_json::from_value(body).expect("a newer field must not break an older reader");
        assert_eq!(parsed, sample(), "and every field this build DOES model survives it");
    }

    /// AND THE FIELDS IT USES ARE STILL A CONTRACT. Forward compatibility is
    /// about fields nobody modeled, never about the ones a caller depends on.
    #[test]
    fn a_rendezvous_missing_a_field_this_build_depends_on_is_still_refused() {
        for absent in ["dir", "key", "url", "pid"] {
            let mut body = serde_json::to_value(sample()).expect("serialize");
            body.as_object_mut().expect("object").remove(absent);
            assert!(
                serde_json::from_value::<DaemonRendezvous>(body).is_err(),
                "a rendezvous with no {absent} is unusable, not merely unfamiliar"
            );
        }
    }

    /// THE COPIED-FILE CASE, which is the one a bare path check would miss.
    ///
    /// `.chief/` is inside the company directory, so copying a project copies
    /// its rendezvous. Without this the copy would point the new directory's
    /// client at the ORIGINAL directory's daemon — a client talking to a
    /// company it is not standing in, which is precisely the split-brain the
    /// composite key existed to prevent.
    #[test]
    fn a_rendezvous_copied_into_another_directory_does_not_describe_it() {
        let subject = sample();
        assert!(subject.describes(Path::new("/work/anvils")));
        assert!(!subject.describes(Path::new("/work/anvils-copy")));
        assert!(!subject.describes(Path::new("/work")));
    }
}
