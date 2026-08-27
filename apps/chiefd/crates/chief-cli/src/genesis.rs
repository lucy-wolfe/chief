//! Company genesis: turning a confirmed name and purpose into a durable
//! company with exactly one CEO, running.
//!
//! Ported from the deleted TypeScript `launcher.ts::runCreateAndBoot` and
//! its wiring half `launcher-wiring.ts`, and from
//! the argv bridge `packages/piing/extensions/founder-launch.ts` used to cross
//! (a hidden `_create-and-boot` subcommand taking `--spec` and `--founder-bootstrap` file paths).
//!
//! # What replaced the bridge
//!
//! The Founder extension no longer spawns a CLI subprocess and no longer writes
//! a spec file and a bootstrap file into a temp directory for another process
//! to read back. `chief` publishes a loopback genesis endpoint into the
//! Founder pane's environment as `CHIEFD_FOUNDER_URL`, and the extension POSTs
//! one JSON document to it through `packages/chiefing`. The hidden
//! `_create-and-boot` subcommand, the two temp files and the retired command
//! namespace that carried it are all gone — there is no second way to create a company.
//!
//! # The sequence, and why each step is where it is
//!
//! 1. **Slug first.** A name that yields no identifier is refused before
//!    anything is claimed. It is a DISPLAY word now — the company's identity is
//!    the directory — but a company with no name is still not a company.
//! 2. **The beacond row must exist before `chiefd run` can register**, so the
//!    claim is made before the only daemon spawn. It is no longer "the
//!    company": the company is `<dir>/.chief/db/chief.db`, and the row is the
//!    box-wide presence record that makes it visible to `chief ls`.
//! 3. **ONE daemon start for the whole flow.** No separate pre-genesis
//!    bootstrap daemon and no restart between genesis and the CEO: every duty
//!    pass already tolerates a company with no manifest yet, and the daemon's
//!    health contract is "schema present", not "company exists". Genesis writes
//!    land directly on the daemon that will serve the CEO.
//! 4. **Genesis is one transaction**, and it is the LAST step. Manifest, model
//!    catalogue, materialization checkpoint and person contracts are seeded
//!    together or not at all — and the same transaction records the CEO-only
//!    start decision in-store, so a company is CEO-only from the instant it
//!    exists rather than from a follow-up write.
//!
//! TOMBSTONE (chief-home-is-cwd §4c): a fifth step, "CEO-only intent, which the
//! daemon's own converge loop actuates" — a `prepare-ceo-only` POST. It ran with
//! NOBODY actuating, because no actuator exists until `chief attach` starts one,
//! so it could never converge here. It did not need to: step 4 already recorded
//! the decision, and a company with no launch intent is CEO-only by the fence's
//! own fail-safe.
//!
//! A failed genesis tears the daemon down — a daemon must never persist with
//! nothing to justify it — and leaves the beacond row as the durable, retryable
//! company claim rather than a second source of truth.

use std::path::Path;

use super::company::{now_iso_millis, CompanyClient};
use super::daemon;
use super::discovery::Discovery;
use super::http::Client;
use super::{LifecycleError, Result};

/// The bounded length of every generated identifier.
const MAX_SLUG_CHARS: usize = 48;

/// What the Founder confirmed.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LaunchRequest {
    /// The confirmed company name.
    pub(crate) name: String,
    /// The confirmed company purpose.
    pub(crate) purpose: String,
}

/// What genesis produced.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchOutcome {
    /// The company slug — its display name.
    pub(crate) slug: String,
    /// The directory the company occupies, which is its identity.
    pub(crate) dir: String,
    /// That directory's company key, stated rather than left to be derived.
    ///
    /// The caller needs it to ADDRESS the company it just created — every
    /// route on `apps/web` is keyed by it — and the only alternative is for
    /// the caller to hash the directory itself. That would be a second
    /// producer of the identity, which is the exact duplication Stage 2
    /// deleted nine of. The server holds the directory, so the server says
    /// the key.
    ///
    /// Without it the founder's "Open <company>" link had nothing to build a
    /// URL from but the display SLUG, and two directories may hold companies
    /// with the same one — so the link resolved to whichever the router found
    /// first, or to nothing.
    pub(crate) key: String,
    /// Its daemon's published URL.
    pub(crate) url: String,
    /// The CEO this company was seeded with, read back from the manifest.
    pub(crate) chief_person_id: String,
    /// The tmux session the company projects onto.
    pub(crate) session: String,
}

/// Normalize a human label into the bounded kebab-case company identifier.
///
/// # Truncate BEFORE the final trim
///
/// The order of the last two steps is the whole correctness argument, and it
/// used to be the other way round: trim, then cut at 48. That lets the CUT
/// create the very thing the trim removed —
/// `northstar-operations-engineering-department-e2e-` is exactly 48 characters
/// and the 48th is a hyphen, so the slice landed on a separator and produced a
/// trailing one, which the slug validator then refused. The generator emitted a
/// value its own validator rejects, which means no length of input is safe,
/// only most of them.
///
/// An input that is entirely non-alphanumeric still yields `""`. That is
/// deliberate: inventing a placeholder here would hide a missing name rather
/// than surface it, and the one caller refuses an empty slug explicitly.
#[must_use]
pub(crate) fn slugify(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let mut collapsed = String::with_capacity(lowered.len());
    let mut previous_was_separator = false;
    for character in lowered.chars() {
        if character.is_ascii_alphanumeric() {
            collapsed.push(character);
            previous_was_separator = false;
        } else if !previous_was_separator {
            collapsed.push('-');
            previous_was_separator = true;
        }
    }
    let truncated: String = collapsed.chars().take(MAX_SLUG_CHARS).collect();
    truncated.trim_matches('-').to_string()
}

/// The smallest durable company a Founder may create: one CEO, no eager team.
///
/// This is a company SPEC — the question — not a manifest. chiefd's
/// `organization_spec::normalize_organization_spec` turns it into the manifest,
/// choosing every id, default tool grant, employment state and unit
/// relationship. This function used to build that manifest here and post it,
/// which put the single most consequential decision in the product — what a
/// company IS at birth — outside chiefd, in violation of mandate 3.
///
/// A launch request carries a name, a purpose and the Founder's model route.
/// There is no spec file and no shape a caller can smuggle a roster through:
/// the CEO seed below is a name and nothing else, and no `departments` key is
/// sent at all.
///
/// # How the two halves stay welded after the P6 split
///
/// The invariants this spec is FOR — exactly one CEO, no eager team, the root
/// department headed by that CEO, and the CEO's full builtin tool grant — are
/// properties of the normalizer, which lives in
/// `chiefd-core` and which this crate deliberately no longer links. So the
/// assertions did not move and did not go: this side pins the bytes against
/// `apps/chiefd/fixtures/founder-genesis-spec.json`, and
/// `chiefd_core::store::organization_spec`'s own tests normalize that same
/// fixture and assert the invariants on the result. One file, both ends — a
/// change to the spec that the normalizer would mishandle reddens the other
/// crate's test, which is the property the shared struct used to provide.
fn founder_spec(name: &str, purpose: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "purpose": purpose,
        "chief": { "name": "Chief" },
    })
}

/// What a genesis failure may do to the daemon, and what it may tell the
/// operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenesisFailure {
    /// The daemon was adopted: leave it alone and say so.
    LeaveRunning(String),
    /// The daemon was spawned by this call: stop it and say so.
    StopWhatWeStarted(String),
}

/// Decide a genesis failure from ONE fact: did this call start the daemon?
///
/// A killed Founder pane resumes and re-issues the identical launch. The
/// directory is the same one, so it resolves to the SAME company; the beacond
/// claim is deliberately idempotent; `daemon::start` finds the live daemon and
/// ADOPTS it; genesis then refuses with `organization-exists`.
/// Before this, that path shut down the running company's daemon and reported
/// "no company was created" — destroying the thing it claimed did not exist.
///
/// Adoption is the exact signal that the company pre-existed the call, so it
/// decides both the teardown and the sentence. Nothing here parses the error:
/// a refusal code is a string that can change, while "did I start this
/// process" is a fact this code owns.
pub(crate) fn genesis_failure_action(adopted: bool, error: &str, dir: &Path) -> GenesisFailure {
    let dir = dir.display();
    if adopted {
        return GenesisFailure::LeaveRunning(format!(
            "ChiefD: {dir} already holds a running company; nothing was created or changed by this launch: {error}\nAttach to it with `chief attach` in {dir}."
        ));
    }
    GenesisFailure::StopWhatWeStarted(format!(
        "ChiefD: launch failed during genesis in {dir}: {error}\nThe beacond presence row remains, but no company was created, no CEO is running, and no Founder handoff occurred."
    ))
}

/// Create the company in `dir`, start its daemon, seed genesis, and narrate
/// each step onto `phases`.
///
/// # Errors
/// [`LifecycleError`] naming the refusal. Every failure emits the phase that
/// explains it BEFORE returning, so a caller reading the stream sees where it
/// stopped before it sees that it stopped.
// The span every launch line hangs off. `company` is `Empty` because the slug
// is DERIVED from the request a few lines in — it is recorded the moment it
// exists, and from then on the layer inherits it into every nested span and
// event, so no step below has to repeat it.
#[tracing::instrument(name = "genesis.launch", skip_all, fields(company = %dir.display()))]
pub(crate) async fn launch_with_phases(
    dir: &Path,
    request: &LaunchRequest,
    phases: &crate::host::phases::PhaseSink,
) -> Result<LaunchOutcome> {
    use crate::host::phases::Phase;

    let started_at = std::time::Instant::now();
    let name = request.name.trim();
    let purpose = request.purpose.trim();
    // THE GENERATOR'S OWN POSTCONDITION, checked. `slugify` is supposed to
    // produce nothing but canonical slugs, and it once did not: it trimmed
    // before truncating, so a 48-character name whose 48th character was a
    // separator yielded a trailing hyphen its own validator rejects. That
    // subsumes the empty case — `""` is not canonical — so this is ONE check
    // where there were two, and it covers the failure the two together missed.
    let slug = slugify(name);
    if !super::paths::is_canonical_slug(&slug) {
        return Err(LifecycleError::refused("Company name needs letters or numbers.".to_owned()));
    }
    // ONE COMPANY PER DIRECTORY, and the store file is the whole check. A
    // second genesis here would claim the beacond row of the company already
    // living in this directory and then post a manifest its daemon refuses —
    // an `organization-exists` minutes later, after a spawn, instead of a
    // sentence now.
    if super::paths::company_present(dir) {
        return Err(LifecycleError::refused(format!(
            "There is already a company in {}. A directory holds exactly one — `cd` somewhere \
             else to create another, or remove this one with `chief rm`.",
            dir.display()
        )));
    }
    tracing::info!(
        event = "genesis.launch.start",
        company = %slug,
        "creating a company"
    );
    let phases = phases.with_slug(slug.as_str());

    let home = super::paths::home()?;
    let key = super::paths::company_key(dir);
    let discovery = Discovery::from_env();
    // #1051: narrated because it can be the LONGEST step and used to be
    // invisible. `ensure_running` returns at once when beacond is answering,
    // and otherwise spawns it and waits on a bounded budget — two very
    // different waits that looked identical from outside (which is to say,
    // looked like nothing at all).
    phases.emit(Phase::BeacondEnsure, discovery.url().to_string());
    super::discovery::ensure_running_with_phases(&discovery, &home, &phases).await?;
    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer. beacond's client is built inside
    // `discovery::Discovery` and stays bare — that surface has no auth runtime.
    let client = Client::operator(dir);
    tracing::info!(
        event = "genesis.discovery.ready",
        elapsed_ms = chiefd_log::elapsed_ms(started_at),
        "beacond is reachable"
    );

    // `chiefd run` refuses to bind for a directory beacond does not hold a row
    // for, so the claim comes before the only daemon spawn. It carries all
    // three facts at once — the directory that IS the company, the key minted
    // from it, and the display word — because beacond records the identity its
    // caller minted rather than hashing the path a second time.
    phases.emit(Phase::CompanyClaim, dir.display().to_string());
    discovery.create_company(dir, &key, &slug).await?;

    // `<dir>/.chief/` is created here and holds nothing else. Everything under
    // it is written by the process that owns it: the daemon mints the store and
    // the keys, and the run and log folders are recreated by whatever needs
    // them. Genesis seeds no file at all.
    std::fs::create_dir_all(super::paths::chief_dir(dir)).map_err(|error| {
        LifecycleError::host(format!(
            "cannot create {}: {error}",
            super::paths::chief_dir(dir).display()
        ))
    })?;

    phases.emit(Phase::CompanyDaemonStart, dir.display().to_string());
    let started = daemon::start(
        &client,
        &home,
        dir,
        &super::company::boot_socket_request(&super::paths::company_key(dir)),
    )
    .await?;

    phases.emit(Phase::CompanyDaemonReady, started.url.clone());

    let at = now_iso_millis();
    let company = CompanyClient::new(&client, &started.url, dir, &key);
    let spec = founder_spec(name, purpose);
    phases.emit(Phase::DurableCreate, "");
    // The manifest, the materialization document and the person operating
    // contracts are all DERIVED from this spec by the route, inside one
    // transaction. They used to be built here and posted alongside it, which
    // let three hand-built documents disagree with each other; a company whose
    // manifest exists without its companions is the half-state Mandate 4 bans,
    // and deriving them together is what makes that unrepresentable.
    let durable_started = std::time::Instant::now();
    let seeded = company.genesis(&spec, &at).await;
    tracing::info!(
        event = "genesis.durable.committed",
        ok = seeded.is_ok(),
        elapsed_ms = chiefd_log::elapsed_ms(durable_started),
        "the genesis transaction returned"
    );
    if let Err(error) = seeded {
        phases.emit(Phase::DurableCreateFailed, error.to_string());
        match genesis_failure_action(started.adopted, &error.to_string(), dir) {
            GenesisFailure::LeaveRunning(message) => return Err(LifecycleError::refused(message)),
            GenesisFailure::StopWhatWeStarted(message) => {
                phases.emit(Phase::CompanyDaemonStop, "");
                match daemon::stop(&client, dir).await {
                    Ok(()) => phases.emit(Phase::CompanyDaemonStopped, ""),
                    Err(stop_error) => {
                        phases.emit(Phase::CompanyDaemonStopFailed, stop_error.to_string());
                    }
                }
                return Err(LifecycleError::refused(message));
            }
        }
    }

    phases.emit(Phase::DurableCreateComplete, "");

    // Read the CEO's id back rather than reconstructing it. chiefd's normalizer
    // mints person ids from the spec seed, and this line used to assume the
    // answer was `executive-ceo` — it is `ceo`, so `/v1/org/person/start`
    // refused `unknown-person` and no company ever reached a running CEO. The
    // id is chiefd's decision (mandate 3); asking for it is the only way to be
    // right about it, and `facts()` reads it off the root department's head.
    let chief_person_id = company
        .facts()
        .await?
        .ok_or_else(|| {
            LifecycleError::refused(format!(
                "ChiefD: created '{slug}' but its manifest could not be read back to name the CEO."
            ))
        })?
        .chief_person_id;
    // TOMBSTONE (chief-home-is-cwd §4c): `Phase::CeoPrepare`, the
    // `prepare_ceo_only` POST it announced, `Phase::CeoPrepareFailed`, and the
    // `genesis.ceo.prepared` span.
    //
    // Genesis stated CEO-only intent with NOBODY actuating — there is no
    // actuator until `chief attach` starts one — so the write never converged
    // here and never could. It did not need to: the transaction that just
    // committed above records the same start decision in-store
    // (`org_ops::prepare_ceo_only`, reached from `org_manifest_genesis`), and
    // a company with no launch intent is CEO-only by the fence's own fail-safe.
    // This was a second write of a fact the first one had already made.
    //
    // NO PHASE REPLACES IT, and that is deliberate rather than an omission. A
    // phase names a step that can fail on its own; this one could only report
    // the success of a write whose failure mode was invisible. What a caller
    // watching the stream sees in its place is `DurableCreateComplete` — which
    // is now the truthful last word of genesis, because after it the company IS
    // durable and IS CEO-only. The steps that can still fail after it belong to
    // the api-host tail and keep their own phases (`ChiefStart`/`ChiefStartFailed`,
    // `host::create::activate_ceo`).
    // The whole launch, on one line, so the first question an incident asks
    // ("how long did it take, and did it finish?") is answered without
    // subtracting timestamps.
    tracing::info!(
        event = "genesis.launch.done",
        url = %started.url,
        elapsed_ms = chiefd_log::elapsed_ms(started_at),
        "the company is created and durable"
    );

    Ok(LaunchOutcome {
        session: super::company::conventional_session_name(&slug, &key),
        slug,
        dir: dir.display().to_string(),
        key,
        url: started.url,
        chief_person_id,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        founder_spec, genesis_failure_action, slugify, GenesisFailure, LaunchRequest,
        MAX_SLUG_CHARS,
    };

    /// THE SHARED ADVERSARIAL CORPUS. Every company-slug producer in this
    /// repository is driven against these exact inputs, in its own crate,
    /// because the two producers may not link each other.
    ///
    /// This copy and `chiefd_core::store::organization_spec`'s are held
    /// character-identical by `scripts/test/slug-producers-agree.test.mjs`.
    /// Editing one alone fails that guard by name: two producers tested against
    /// two different corpora are two producers nobody compared.
    ///
    /// The underscores are the point. `_` is
    /// `chief_cli::placement::SESSION_TERMINATOR`, the one character the whole
    /// tmux prefix-collision proof turns on, and the corpus that stood here
    /// before this test did not contain a single one.
    const SLUG_PRODUCER_CORPUS: &[&str] = &[
        "Acme",
        "Acme_Corp",
        "acme_corp",
        "_leading",
        "trailing_",
        "__",
        "org-acme_",
        "  --Acme Capital--  ",
        "Leo Capital Inc.",
        "A B C D E F G H I J K L M N O P Q R S T U V W X Y Z",
        "Northstar Operations Engineering Department E2E Team",
        "Ünïcødé Ç☃mpany",
        "tab\there",
        "new\nline",
        "null\0byte",
        "dots.and.dots",
        "slash/es and back\\slashes",
        "%_$#@!",
        "...",
        "   ",
        "-- --",
    ];

    #[test]
    fn truncation_happens_before_the_final_trim() {
        // The measured regression: 48 characters whose 48th is a separator.
        // Trim-then-cut leaves a trailing hyphen the slug validator refuses.
        let slug = slugify("Northstar Operations Engineering Department E2E Team");
        assert_eq!(slug.len(), 47);
        assert!(!slug.ends_with('-'), "{slug}");
        assert!(super::super::paths::is_canonical_slug(&slug), "{slug}");
    }

    #[test]
    fn every_generated_slug_satisfies_the_validator_that_guards_the_path_join() {
        for input in [
            "Acme",
            "  --Acme Capital--  ",
            "Leo Capital Inc.",
            "A B C D E F G H I J K L M N O P Q R S T U V W X Y Z",
            &"A".repeat(200),
            &"word ".repeat(40),
        ] {
            let slug = slugify(input);
            assert!(!slug.is_empty(), "{input}");
            assert!(slug.chars().count() <= MAX_SLUG_CHARS, "{slug}");
            assert!(super::super::paths::is_canonical_slug(&slug), "{input} -> {slug}");
        }
    }

    /// THE PROPERTY THE TMUX COLLISION PROOF ACTUALLY NEEDS, asserted here for
    /// this producer and in `chiefd_core::store::organization_spec` for the
    /// other one.
    ///
    /// `placement::session_name_for_slug` proves that no two company sessions
    /// can prefix-collide, and its first fact is "a slug can never contain the
    /// terminator". That fact used to be argued from the producer side by
    /// naming ONE producer, which was never the whole set — the manifest slug
    /// and every id derived from it come from a second implementation in a
    /// crate this one is forbidden to link. The argument does not need a
    /// producer count. It needs this: whatever a producer emits satisfies
    /// `paths::is_canonical_slug`, which refuses `_` exactly.
    ///
    /// Empty is an accepted answer and is NOT a slug — `launch` refuses it
    /// before anything is claimed — so the property is "empty, or canonical",
    /// and the emptiness half is pinned separately below.
    #[test]
    fn no_input_makes_this_producer_emit_a_non_canonical_slug() {
        for input in SLUG_PRODUCER_CORPUS {
            let slug = slugify(input);
            if slug.is_empty() {
                continue;
            }
            assert!(
                super::super::paths::is_canonical_slug(&slug),
                "slugify({input:?}) emitted {slug:?}, which the validator that guards every \
                 company path join refuses"
            );
            assert!(
                !slug.contains(chief_cli::placement::SESSION_TERMINATOR),
                "slugify({input:?}) emitted {slug:?}, which carries the tmux session terminator \
                 — two companies could then mint prefix-colliding sessions"
            );
        }
    }

    #[test]
    fn a_name_with_no_letters_or_digits_yields_an_empty_slug_rather_than_a_placeholder() {
        assert_eq!(slugify("..."), "");
        assert_eq!(slugify("   "), "");
        assert_eq!(slugify("-- --"), "");
    }

    #[test]
    fn ordinary_names_round_trip_the_way_an_operator_expects() {
        assert_eq!(slugify("Acme Capital"), "acme-capital");
        assert_eq!(slugify("Leo Capital Inc."), "leo-capital-inc");
        assert_eq!(slugify("  --Acme Capital--  "), "acme-capital");
    }

    /// The exact bytes this client asks chiefd to make a company out of.
    ///
    /// Before P6 the two assertions this replaces built a manifest by calling
    /// `chiefd_core::store::organization_spec::normalize_organization_spec`
    /// from here — an edge that put the whole store crate in the operator
    /// client's link graph for a test. Deleting the assertions was not an
    /// option (they lock "exactly one CEO, no eager team, root headed by that
    /// CEO" and the CEO's full builtin tool grant), so they
    /// moved to the crate that owns the normalizer and this half pins the input
    /// they run on. `chiefd_core::store::organization_spec`'s
    /// `the_founder_spec_the_operator_client_sends_normalizes_to_one_ceo_
    /// with_bash` reads the same file; changing this builder without changing
    /// the fixture fails here, and changing the fixture in a way the normalizer
    /// mishandles fails there.
    #[test]
    fn the_founder_spec_is_byte_for_byte_the_shared_genesis_fixture() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/founder-genesis-spec.json"))
                .expect("the shared genesis fixture must be JSON");
        assert_eq!(founder_spec("Acme", "Sell anvils"), fixture);
        // And the shape a caller must not be able to reach through it: no
        // roster, ever.
        assert!(fixture.get("departments").is_none(), "the spec must never carry a roster");
        assert!(fixture.get("people").is_none(), "the spec must never carry a roster");
    }

    #[test]
    fn a_launch_request_rejects_an_unknown_field_rather_than_ignoring_it() {
        let good: Result<LaunchRequest, _> =
            serde_json::from_str(r#"{"name":"Acme","purpose":"Sell anvils"}"#);
        assert!(good.is_ok());
        let smuggled: Result<LaunchRequest, _> =
            serde_json::from_str(r#"{"name":"Acme","purpose":"p","departments":[{"name":"eng"}]}"#);
        assert!(smuggled.is_err(), "a caller must not be able to smuggle a roster through genesis");
    }

    /// The replay case, and the destructive bug this function exists to end.
    ///
    /// A killed Founder pane resumes and re-issues the identical launch. The
    /// slug is a pure function of the name, so it lands on the SAME company;
    /// `daemon::start` finds that company's live daemon and adopts it; genesis
    /// refuses `organization-exists`. This branch used to STOP that daemon —
    /// killing the running company — and then report that no company was
    /// created, which was false twice over.
    #[test]
    fn a_genesis_failure_on_an_adopted_daemon_never_stops_it() {
        let action = genesis_failure_action(true, "organization-exists", Path::new("/work/cobalt"));
        let GenesisFailure::LeaveRunning(message) = action else {
            panic!("an adopted daemon is never this call's to stop: {action:?}");
        };
        assert!(message.contains("already holds a running company"));
        assert!(
            message.contains("`chief attach` in /work/cobalt"),
            "the operator is told where it went"
        );
        assert!(
            !message.contains("no company was created"),
            "the company DOES exist — saying otherwise is the lie this fixes"
        );
    }

    /// The fresh-spawn teardown is correct and is deliberately unchanged: a
    /// daemon this call started, with no company to justify it, still goes.
    #[test]
    fn a_genesis_failure_on_a_spawned_daemon_still_tears_it_down() {
        let action =
            genesis_failure_action(false, "store is unwritable", Path::new("/work/cobalt"));
        let GenesisFailure::StopWhatWeStarted(message) = action else {
            panic!("a daemon we spawned with no company behind it must not persist: {action:?}");
        };
        assert!(message.contains("no company was created"));
        assert!(message.contains("store is unwritable"), "the real cause survives");
    }

    /// The decision reads ONE fact and never parses the refusal. A code is a
    /// string that can change; "did I start this process" is a fact this code
    /// owns.
    #[test]
    fn the_decision_is_the_same_whatever_the_error_says() {
        for error in ["organization-exists", "", "some future refusal code"] {
            assert!(matches!(
                genesis_failure_action(true, error, Path::new("/work/cobalt")),
                GenesisFailure::LeaveRunning(_)
            ));
            assert!(matches!(
                genesis_failure_action(false, error, Path::new("/work/cobalt")),
                GenesisFailure::StopWhatWeStarted(_)
            ));
        }
    }
}
