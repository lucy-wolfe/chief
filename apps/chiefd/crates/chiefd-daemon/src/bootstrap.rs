//! SQL-authoritative Chiefd bootstrap and operator controls.
//!
//! Structural organization state is never adopted from disk projections.
//! Fresh creation publishes the normalized manifest row through Chiefd before
//! any duty may run; a missing SQL manifest stays `unknown-company`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::{SharedClock, SystemClock};
use chiefd_core::ChiefdError;

/// Refuse an operator control-plane write whose company is not in the
/// directory it is about to write, naming the path.
///
/// There is nothing to correct any more, and that is the point. The refusal
/// used to carry a second candidate path, because `--data-root` meant the ORGS
/// root one directory below the data root of the same name: pass the obvious
/// one and an empty database was CREATED there, the write applied to it, and
/// success printed. Nothing ever read that file again. It cost a full day on
/// #13 — `chiefd set-actuation-config --mode shadow` reported success every
/// time while the daemon a directory below never saw it, so the failure looked
/// like a defect in the actuation gate rather than a write that had gone
/// somewhere else.
///
/// `--dir` is the company directory, the store is unconditionally
/// `<dir>/.chief/db/chief.db`, and there is no second directory the operator
/// could have meant. `bootstrap-store` deliberately does NOT use this —
/// seeding a store legitimately creates the file.
fn require_existing_company_db(dir: &std::path::Path) -> Result<(), ExitCode> {
    let path = crate::company_dir::store_db_path(dir);
    if path.exists() {
        return Ok(());
    }
    eprintln!(
        "chiefd: refusing to write: {} holds no company (no store at {})",
        dir.display(),
        path.display()
    );
    Err(ExitCode::FAILURE)
}

/// `--dir <company directory>`, the one argument `clear-breaker` takes.
fn parse(mut args: impl Iterator<Item = String>) -> Option<PathBuf> {
    let mut dir = None;
    while let Some(arg) = args.next() {
        if arg == "--dir" {
            dir = args.next().map(PathBuf::from);
        }
    }
    dir
}

/// `chiefd bootstrap-store` — explicit migration import of one known typed
/// store from a JSON export.
///
/// This is an operator-invoked migration/repair command, never a startup
/// fallback. The in-memory compatibility ledger accepts the exported JSON,
/// while CompanyDb's sealed persistence dispatch projects the named supported
/// store into normalized tables and rejects unknown names. Structural
/// `org-manifest` is deliberately not imported here: company identity must be
/// created through the typed organization mutation.
pub fn run_bootstrap_store(args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir = None;
    let mut store = None;
    let mut file = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = args.next().map(PathBuf::from),
            "--store" => store = args.next(),
            "--file" => file = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    let (Some(dir), Some(store), Some(file)) = (dir, store, file) else {
        eprintln!(
            "usage: chiefd bootstrap-store --dir <company directory> --store <name> --file <path>"
        );
        return ExitCode::from(2);
    };

    let body = match std::fs::read_to_string(&file) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(path = %file.display(), %error, "cannot read the store content file");
            return ExitCode::FAILURE;
        }
    };
    // Fail loudly on obviously-wrong input rather than writing garbage into a
    // live ledger — this must be a real JSON document, even though the store
    // itself isn't validated further here.
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        tracing::error!(path = %file.display(), "store content is not valid JSON; refusing to write it");
        return ExitCode::FAILURE;
    }

    let clock: SharedClock = Arc::new(SystemClock::default());
    let company = match crate::company_dir::open(&dir, clock) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::error!(%error, "cannot open the company database");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the tokio runtime");
            return ExitCode::FAILURE;
        }
    };

    // Keep the operator command's parsing/IO outside the mutation core. The
    // mutation is still subject to CompanyDb's sealed normalized-store
    // persistence dispatch; no generic blob fallback exists.
    let result = runtime.block_on(put_store_document(&company, &store, body));

    match result {
        Ok((assignment_count, effect_count)) => {
            tracing::info!(
                %store,
                assignment_count,
                effect_count,
                "store seeded from real content"
            );
            println!(
                "{}",
                serde_json::json!({
                    "store": store,
                    "assignments": assignment_count,
                    "effects": effect_count,
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%store, %error, "bootstrap-store refused");
            ExitCode::FAILURE
        }
    }
}

/// The write core of `chiefd bootstrap-store`: import `body` into a named
/// supported store of an already-open [`CompanyDb`], projecting relational
/// sub-data too.
///
/// Some stores keep hot sub-data in relational tables outside their document
/// body (currently just `supervision`'s assignments/effects, plan §5.1 M12) —
/// `chiefd_core::store::seed_relational_extra` projects the document into them
/// generically, so no caller here ever has to name a store itself
/// (`fence_containment.rs` forbids exactly that from outside a store's own
/// module). A no-op for every store that keeps everything in its document.
///
/// This deliberately overwrites the named store as an explicit operator repair.
///
/// Returns `(assignment_count, effect_count)`.
///
/// # Errors
/// Propagates any [`ChiefdError`] from the mutation (e.g. a relational-extra
/// projection that rejects a malformed document body).
async fn put_store_document(
    company: &CompanyDb,
    store: &str,
    body: String,
) -> Result<(usize, usize), ChiefdError> {
    let store_name = store.to_string();
    let body_for_extra = body.clone();
    company
        .mutate(MutationClass::Normal, MutationName("bootstrap-store"), move |ledgers| {
            let (assignments, effects) =
                chiefd_core::store::seed_relational_extra(ledgers, &store_name, &body_for_extra)?;
            ledgers.put_document(&store_name, body);
            Ok((assignments, effects))
        })
        .await
}

/// #105 second half: SEED a company's native `supervision`/`activity` ledger
/// from its own manifest when it is absent — the last step that lets a freshly
/// created company actually run a duty.
///
/// A genuinely fresh company has a typed normalized manifest but no activity or
/// supervision rows yet, so every duty would refuse `store never written`.
///
/// Where the decision sits is the point. This is a STARTUP step, not a
/// read-time fallback: the read primitives keep refusing on
/// [`ChiefdError::Absent`] (see `store/activity.rs`, `store/supervision.rs`), so
/// a ledger that vanishes from a LIVE company is still a loud refusal and can
/// never be silently read as "empty" — which for activity is the input that
/// plans a kill for every staffed person. Absence is only ever resolved here,
/// once, before any duty runs, and only into the deterministic initial state the
/// manifest already implies (the same `initial(...)` the TypeScript
/// `initialSupervisionLedger` writes).
///
/// Ordering: strictly after the typed manifest publish. The seed reads that SQL
/// authority and refuses `unknown-company` when it is absent.
///
/// # Errors
/// [`ChiefdError`] when the manifest cannot be read or the seeded ledger does
/// not validate. Returns `false` — a clean no-op — when the ledger is already
/// present.
pub async fn seed_store_if_absent(company: &CompanyDb, store: &str) -> Result<bool, ChiefdError> {
    if company.read(|snapshot| snapshot.ledgers().document_body(store).is_some()) {
        return Ok(false);
    }
    let store_name = store.to_string();
    company
        .mutate(MutationClass::Normal, MutationName("seed-store"), move |ledgers| {
            // Re-check inside the commit: another startup step may have adopted
            // it between the read above and this mutation, and a real body must
            // never be replaced by a fabricated empty one.
            if ledgers.document_body(&store_name).is_some() {
                return Ok(false);
            }
            let manifest = chiefd_core::store::organization::read(ledgers)?;
            for seeded in chiefd_core::store::BOOT_ADOPTABLE_STORES {
                if seeded != store_name {
                    continue;
                }
                chiefd_core::store::seed_native_ledger(ledgers, &manifest, seeded)?;
                return Ok(true);
            }
            Ok(false)
        })
        .await
}

/// What the boot-time ledger seed found. Two outcomes, because a company with
/// no manifest and a company with a missing ledger are different states and used
/// to be reported as the same failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootSeed {
    /// No organization manifest yet: genesis has not committed. Nothing to seed,
    /// and nothing missing.
    NoManifestYet,
    /// The manifest is durable, so every
    /// [`BOOT_ADOPTABLE_STORES`](chiefd_core::store::BOOT_ADOPTABLE_STORES)
    /// ledger that was absent was seeded from it. A ledger that refused is a
    /// warning of its own, named by store — the count is deliberately not
    /// carried here, because what a caller can act on is the log line that names
    /// the store, not an arithmetic total.
    Seeded,
}

/// Seed every boot-adoptable native ledger this company is missing, ONCE, at
/// startup.
///
/// # Why the manifest is checked first
///
/// `seed_store_if_absent` reads the manifest and refuses `unknown-company`
/// without one, and this loop used to report that refusal as two warnings
/// saying "the company's duties will keep refusing until one exists". On a
/// company being created that sentence was FALSE, and it was printed on every
/// single launch: `chief_cli::genesis` starts this daemon and then posts
/// `/v1/org/manifest/genesis-with-models` to it, because the daemon is the
/// company's single writer — so genesis writes THROUGH the process that has to
/// wait for it (measured at 229 ms on a live box). And what genesis then commits
/// is the manifest AND both of these ledgers in ONE transaction
/// (`CompanyDb::org_manifest_genesis_with_models`), so there was never anything
/// here to seed or to repair.
///
/// Seeding is for the OTHER company: one whose manifest is already durable and
/// whose ledgers are not (#105). That company still gets the identical seed, and
/// a genuine refusal still gets the identical warning — where it is true.
pub async fn seed_boot_ledgers(company: &CompanyDb, company_key: &str) -> BootSeed {
    if !company.read(|snapshot| chiefd_core::store::organization::exists(snapshot.ledgers())) {
        tracing::info!(
            company = %company_key,
            "chiefd run: no organization manifest yet, so there is no initial native ledger to \
             seed; genesis commits the manifest and both of its ledgers in one transaction"
        );
        return BootSeed::NoManifestYet;
    }
    for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
        match seed_store_if_absent(company, store).await {
            Ok(true) => tracing::info!(
                company = %company_key,
                %store,
                "chiefd run: seeded an initial native ledger at startup (fresh company: nothing to adopt)"
            ),
            Ok(false) => {}
            Err(error) => tracing::warn!(
                company = %company_key,
                %store,
                %error,
                "chiefd run: could not seed an initial native ledger; the company's duties will \
                 keep refusing until one exists"
            ),
        }
    }
    BootSeed::Seeded
}

/// `chiefd set-actuation-config` — the operator control-plane write for a
/// company's durable converge-safety config (actuation mode, pointer-sweep
/// flag, destructive-budget override), next to `clear-breaker`.
///
/// Only the named fields change; the rest keep their stored values. Like any
/// config change this also resumes a tripped breaker (one of the two
/// documented resume paths — see `converge_safety::set_actuation_config`).
///
/// On a LIVE company the write lands in sqlite immediately and the running
/// daemon adopts it at the start of its next reconcile pass
/// (`safety::refresh_safety_doc`), so this is the supported way to flip
/// `budgetOverride` (or drop back to shadow) without a restart. A distinct
/// top-level mode, like `bootstrap-store`/`clear-breaker`,
/// so an operator reading the command line sees exactly what ran.
pub fn run_set_actuation_config(args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir = None;
    let mut mode = None;
    let mut sweep_live = None;
    let mut budget_override = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = args.next().map(PathBuf::from),
            "--mode" => mode = args.next(),
            "--sweep-live" => sweep_live = args.next(),
            "--budget-override" => budget_override = args.next(),
            _ => {}
        }
    }
    let usage = "usage: chiefd set-actuation-config --dir <company directory> \
                 [--mode shadow|apply] [--sweep-live on|off] [--budget-override on|off]";
    let Some(dir) = dir else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let parse_flag = |name: &str, value: Option<String>| -> Result<Option<bool>, ExitCode> {
        match value.as_deref() {
            None => Ok(None),
            Some("on") => Ok(Some(true)),
            Some("off") => Ok(Some(false)),
            Some(other) => {
                eprintln!("{name} must be 'on' or 'off', got '{other}'\n{usage}");
                Err(ExitCode::from(2))
            }
        }
    };
    let mode = match mode.as_deref() {
        None => None,
        Some("shadow") => Some(chiefd_core::store::converge_safety::ActuationMode::Shadow),
        Some("apply") => Some(chiefd_core::store::converge_safety::ActuationMode::Apply),
        Some(other) => {
            eprintln!("--mode must be 'shadow' or 'apply', got '{other}'\n{usage}");
            return ExitCode::from(2);
        }
    };
    let sweep_live = match parse_flag("--sweep-live", sweep_live) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let budget_override = match parse_flag("--budget-override", budget_override) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if mode.is_none() && sweep_live.is_none() && budget_override.is_none() {
        eprintln!(
            "nothing to set: pass at least one of --mode/--sweep-live/--budget-override\n{usage}"
        );
        return ExitCode::from(2);
    }

    if let Err(code) = require_existing_company_db(&dir) {
        return code;
    }
    let clock: SharedClock = Arc::new(SystemClock::default());
    let company = match crate::company_dir::open(&dir, clock) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::error!(%error, "cannot open the company database");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the tokio runtime");
            return ExitCode::FAILURE;
        }
    };

    // Merge over the STORED state (not the breaker-folded effective config):
    // unnamed fields keep exactly what the company had.
    let stored =
        company.read(|snapshot| chiefd_core::store::converge_safety::read(snapshot).into_parts().0);
    let mode = mode.unwrap_or(stored.actuation_mode);
    let sweep_live = sweep_live.unwrap_or(stored.sweep_live);
    let budget_override = budget_override.unwrap_or(stored.budget_override);

    let result = runtime.block_on(chiefd_host::converge_apply::safety::set_actuation_config(
        &company,
        mode,
        sweep_live,
        budget_override,
    ));

    match result {
        Ok(()) => {
            let dir = dir.display().to_string();
            tracing::info!(%dir, ?mode, sweep_live, budget_override, "actuation config set");
            println!(
                "{}",
                serde_json::json!({
                    "dir": dir,
                    "actuationMode": match mode {
                        chiefd_core::store::converge_safety::ActuationMode::Shadow => "shadow",
                        chiefd_core::store::converge_safety::ActuationMode::Apply => "apply",
                    },
                    "sweepLive": sweep_live,
                    "budgetOverride": budget_override,
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(dir = %dir.display(), %error, "set-actuation-config refused");
            ExitCode::FAILURE
        }
    }
}

/// `chiefd clear-breaker` — the explicit operator acknowledgement that resumes
/// a company whose converge circuit breaker has tripped (three consecutive
/// apply-cycle failures; `chiefd-host`'s `converge_apply::safety` durably
/// forces the company to shadow mode until this is called). A distinct
/// top-level mode, like `bootstrap-store`, so an operator
/// reading the command line sees exactly what ran — this does not silently
/// happen as a side effect of anything else.
pub fn run_clear_breaker(args: impl Iterator<Item = String>) -> ExitCode {
    let Some(dir) = parse(args) else {
        eprintln!("usage: chiefd clear-breaker --dir <company directory>");
        return ExitCode::from(2);
    };

    if let Err(code) = require_existing_company_db(&dir) {
        return code;
    }
    let clock: SharedClock = Arc::new(SystemClock::default());
    let company = match crate::company_dir::open(&dir, clock) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::error!(%error, "cannot open the company database");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the tokio runtime");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(chiefd_host::converge_apply::safety::clear_breaker(&company)) {
        Ok(()) => {
            let dir = dir.display().to_string();
            tracing::info!(%dir, "circuit breaker cleared");
            println!("{}", serde_json::json!({"dir": dir, "breakerTripped": false}));
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(dir = %dir.display(), %error, "clear-breaker refused");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiefd_core::store::converge_safety::ActuationMode;
    use chiefd_core::store::{organization, supervision};
    use chiefd_core::test_support::northstar_manifest;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|s| (*s).to_string())
    }

    /// A company writer on a temp database, with nothing in it — schema present,
    /// no manifest — which is exactly what genesis spawns a daemon onto.
    fn empty_company(dir: &tempfile::TempDir) -> CompanyDb {
        let clock: SharedClock = Arc::new(SystemClock::default());
        CompanyDb::open("northstar-conformance", &dir.path().join("chiefd.sqlite"), clock)
            .expect("open company database")
    }

    /// THE STARTUP-RACE REGRESSION. A daemon that boots before genesis has
    /// committed the manifest must report that as the ordinary pre-genesis state
    /// it is, and must NOT refuse: the two refusals this used to log claimed the
    /// company's duties "will keep refusing until one exists", which was false on
    /// every launch — genesis commits the manifest AND both of these ledgers in
    /// one transaction a few hundred milliseconds later.
    #[tokio::test]
    async fn a_company_whose_genesis_has_not_committed_is_not_a_failed_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = empty_company(&dir);

        assert_eq!(
            seed_boot_ledgers(&company, "northstar-conformance").await,
            BootSeed::NoManifestYet
        );
        // And nothing was written: a pre-genesis boot leaves the company exactly
        // as genesis will find it.
        for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
            assert!(
                company.read(|snapshot| snapshot.ledgers().document_body(store).is_none()),
                "{store} must not be fabricated before the manifest it is derived from exists"
            );
        }
    }

    /// The case seeding actually exists for (#105), unchanged: a company whose
    /// manifest IS durable and whose native ledgers are not gets both seeded at
    /// boot.
    #[tokio::test]
    async fn a_manifest_with_no_native_ledgers_still_gets_them_seeded_at_boot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = empty_company(&dir);
        company
            .mutate(MutationClass::Normal, MutationName("test.manifest-only"), |ledgers| {
                organization::create(ledgers, &northstar_manifest(0))
            })
            .await
            .expect("publish the manifest alone");

        assert_eq!(seed_boot_ledgers(&company, "northstar-conformance").await, BootSeed::Seeded);
        for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
            assert!(
                company.read(|snapshot| snapshot.ledgers().document_body(store).is_some()),
                "{store} is seeded from the durable manifest"
            );
        }
    }

    /// Every restart of a live company: both ledgers are already there, so the
    /// boot seed is a clean no-op that overwrites nothing.
    #[tokio::test]
    async fn a_complete_company_is_seeded_again_by_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = empty_company(&dir);
        company
            .mutate(MutationClass::Normal, MutationName("test.genesis"), |ledgers| {
                let manifest = northstar_manifest(0);
                organization::create(ledgers, &manifest)?;
                supervision::seed(ledgers, &manifest)?;
                chiefd_core::store::activity::seed(ledgers, &manifest)?;
                Ok(())
            })
            .await
            .expect("genesis commits");
        let before = company.read(|snapshot| {
            snapshot.ledgers().document_body("supervision").unwrap_or_default().to_string()
        });

        assert_eq!(seed_boot_ledgers(&company, "northstar-conformance").await, BootSeed::Seeded);
        assert_eq!(
            company.read(|snapshot| snapshot
                .ledgers()
                .document_body("supervision")
                .unwrap_or_default()
                .to_string()),
            before,
            "a boot seed never replaces a real ledger with a fabricated empty one"
        );
    }

    #[tokio::test]
    async fn bootstrap_store_returns_projection_counts_not_a_document_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(SystemClock::default());
        let company = CompanyDb::open("bootstrap-test", &dir.path().join("chiefd.sqlite"), clock)
            .expect("open company database");
        company
            .mutate(MutationClass::Normal, MutationName("test.seed"), |ledgers| {
                let manifest = northstar_manifest(0);
                organization::create(ledgers, &manifest)?;
                supervision::seed(ledgers, &manifest)?;
                Ok(())
            })
            .await
            .expect("seed normalized supervision store");
        let body = company.read(|snapshot| {
            snapshot
                .ledgers()
                .document_body("supervision")
                .expect("seeded supervision body")
                .to_string()
        });

        assert_eq!(
            put_store_document(&company, "supervision", body).await.expect("store import"),
            (0, 0),
            "the bootstrap contract reports projected records, never a mutable document version"
        );
    }

    /// Read the stored config through a FRESH actor: the CLI's own actor is
    /// dropped when it returns, and a still-open actor would never observe the
    /// CLI's write (that blindness is exactly the bug this CLI pairs with the
    /// cycle refresh to fix).
    /// Create the company's store the way a real company's daemon already has,
    /// so an operator control-plane write has something to write.
    fn create_company_db(dir: &std::path::Path) {
        let clock: SharedClock = Arc::new(SystemClock::default());
        drop(crate::company_dir::open(dir, clock).expect("create company db"));
    }

    /// An operator control-plane write must REFUSE against a directory that
    /// holds no company, rather than create one and report success.
    ///
    /// The refusal used to carry a correction, because `--data-root` meant the
    /// ORGS root one directory below the data root of the same name: passing
    /// the obvious value CREATED an empty database up there, applied the write,
    /// and printed success while the daemon below never saw the setting. #13
    /// lost a day to it. `--dir` has no second meaning — a directory either
    /// holds `.chief/db/chief.db` or it does not — so what survives is the
    /// refusal itself, and the invariant it protects: NO ORPHAN DATABASE.
    #[test]
    fn an_operator_write_refuses_a_directory_with_no_company() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("not-a-company");
        std::fs::create_dir_all(&empty).expect("an ordinary directory");

        let code = run_set_actuation_config(args(&[
            "--dir",
            empty.to_str().expect("utf8"),
            "--mode",
            "shadow",
        ]));
        assert_eq!(code, ExitCode::FAILURE, "the write must refuse, not create a database");
        assert!(
            !crate::company_dir::store_db_path(&empty).exists(),
            "a refused write must leave no orphan database behind"
        );

        // The same command against a directory that DOES hold a company is
        // accepted, and lands on that company's own store.
        let company = dir.path().join("anvils");
        std::fs::create_dir_all(&company).expect("company dir");
        create_company_db(&company);
        assert_eq!(stored(&company).0, ActuationMode::Shadow, "a fresh row reads shadow");
        assert_eq!(
            run_set_actuation_config(args(&[
                "--dir",
                company.to_str().expect("utf8"),
                "--mode",
                "apply",
            ])),
            ExitCode::SUCCESS
        );
        assert_eq!(stored(&company).0, ActuationMode::Apply);
    }

    fn stored(dir: &std::path::Path) -> (ActuationMode, bool, bool) {
        let clock: SharedClock = Arc::new(SystemClock::default());
        let db = crate::company_dir::open(dir, clock).expect("open");
        db.read(|snapshot| {
            let state = chiefd_core::store::converge_safety::read(snapshot).into_parts().0;
            (state.actuation_mode, state.sweep_live, state.budget_override)
        })
    }

    #[test]
    fn set_actuation_config_updates_only_the_named_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_str().expect("utf8");
        // The company's store, which a real company always has. This command
        // REFUSES to conjure one now (see `require_existing_company_db`): a
        // control-plane write against a company that is not there used to
        // create an orphan file and report success.
        create_company_db(dir.path());

        // What `chiefd run` writes at boot: apply + sweep live, budget kept.
        let _ = run_set_actuation_config(args(&[
            "--dir",
            root,
            "--mode",
            "apply",
            "--sweep-live",
            "on",
        ]));
        assert_eq!(stored(dir.path()), (ActuationMode::Apply, true, false));

        // The operator flips ONLY the override; mode and sweep keep their
        // stored values rather than being reset to defaults.
        let _ = run_set_actuation_config(args(&["--dir", root, "--budget-override", "on"]));
        assert_eq!(
            stored(dir.path()),
            (ActuationMode::Apply, true, true),
            "the named field changed; the others kept their stored values"
        );

        // And the lever moves back: drop to shadow, override off again.
        let _ = run_set_actuation_config(args(&[
            "--dir",
            root,
            "--mode",
            "shadow",
            "--budget-override",
            "off",
        ]));
        assert_eq!(stored(dir.path()), (ActuationMode::Shadow, true, false));
    }

    /// (E10-S2, #763: rewritten, not deleted — the shared-file branch this
    /// test used to exercise via `Some(shared_path)` no longer exists.
    /// `company_dir::open` is a pure function of the DIRECTORY now, so the
    /// SAME invariant this test always cared about — every consumer of one
    /// company sees the same rows — holds because every consumer derives the
    /// identical path from the identical directory, not because they were all
    /// handed the same explicit shared-file override.)
    #[test]
    fn per_company_operator_and_worker_open_the_same_actor_and_see_the_same_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(SystemClock::default());
        let daemon =
            crate::company_dir::open(dir.path(), Arc::clone(&clock)).expect("full daemon actor");
        let daemon_label = daemon.label().to_owned();
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime
            .block_on(chiefd_host::converge_apply::safety::set_actuation_config(
                &daemon,
                ActuationMode::Apply,
                true,
                true,
            ))
            .expect("seed full daemon row");
        drop(daemon);

        for consumer in ["operator control", "bootstrap store"] {
            let reopened = crate::company_dir::open(dir.path(), Arc::clone(&clock))
                .unwrap_or_else(|error| panic!("{consumer} opens the per-company actor: {error}"));
            assert_eq!(
                reopened.label(),
                daemon_label,
                "{consumer} resolves the identical company key"
            );
            let stored = reopened.read(|snapshot| {
                chiefd_core::store::converge_safety::read(snapshot).into_parts().0
            });
            assert_eq!(stored.actuation_mode, ActuationMode::Apply, "{consumer}");
            assert!(stored.sweep_live, "{consumer}");
            assert!(stored.budget_override, "{consumer}");
        }
    }

    #[test]
    fn set_actuation_config_without_any_field_is_a_usage_error_that_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_str().expect("utf8");

        let _ = run_set_actuation_config(args(&["--dir", root]));
        assert!(
            !crate::company_dir::store_db_path(dir.path()).exists(),
            "a usage error never opens (or creates) the company database"
        );
    }

    #[test]
    fn set_actuation_config_rejects_an_unparseable_flag_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_str().expect("utf8");

        let _ = run_set_actuation_config(args(&["--dir", root, "--budget-override", "maybe"]));
        assert!(
            !crate::company_dir::store_db_path(dir.path()).exists(),
            "a bad flag value is refused before any write"
        );
    }
}
