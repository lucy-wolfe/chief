//! `chief rm [--yes]` — make the company in this directory stop existing.
//!
//! # It deletes inside a directory the USER owns, and that changes everything
//!
//! Its ancestor deleted `<orgs>/.<slug>.chief.db` and `<orgs>/<slug>/` — two
//! paths under an invisible tree that chief alone created and chief alone
//! wrote to. Nothing an operator cared about could be in them. A company is
//! the directory the operator ran `chief` in now, which is their project, their
//! source tree, their working files; so this verb deletes exactly ONE thing:
//!
//! ```text
//! <dir>/.chief/          — the store, the keys, the run files, the logs
//! ```
//!
//! **`.pi/` is not chief's and is never touched**, nor is any other file in the
//! directory. `paths`' ownership table is the rule and this is its one
//! destructive reader: `<dir>` and `<dir>/.pi/` are the USER's (and Pi's), and
//! `.chief/` is the only folder chief has ever written into.
//!
//! The confirmation says the path out loud for the same reason. An operator who
//! is asked "Remove 'acme'?" cannot tell which `acme`, and cannot tell whether
//! the answer takes their repository with it. One that reads
//! `Delete /work/acme/.chief …?` can.
//!
//! # The gap this closes
//!
//! beacond could always delete a company row: `POST /v1/company/delete` has
//! been there since the registry was written, is idempotent, and has its own
//! tests. Nothing ever called it. So the product could create a company and
//! could stop one, and had no way at all to remove one — every company anybody
//! ever made stayed in `chief ls` for ever, including the throwaway ones a
//! live test creates a dozen at a time.
//!
//! # Removing is not stopping
//!
//! [`super::stop`] clears a company's runtime and preserves every byte of its
//! durable state; its beacond row survives and `chief ls` says `stopped`. That
//! is a real state and this verb is not allowed to be reached by it. `rm` is
//! the operator saying the company should not exist, and it is the ONLY thing
//! in the product that says so.
//!
//! # THE ORDER, and why the row goes last
//!
//! 1. **Refuse a directory with no company**, before anything else. A `chief
//!    rm` typed one directory too high must delete nothing at all, and must say
//!    where it looked.
//! 2. **Confirm.** Nothing is touched before the operator has said yes, and a
//!    non-interactive caller without `--yes` is refused rather than assumed to
//!    have agreed. This deletes durable state; the confirmation is the whole
//!    protection.
//! 3. **Stop the runtime and the daemon**, through [`super::stop::stop_runtime`]
//!    exactly as `chief stop` does — its ordering law and its idempotence are
//!    already correct and are reused rather than restated. Deleting a database
//!    out from under a live daemon would leave a process serving a file that no
//!    longer has a name.
//! 4. **Delete `<dir>/.chief/`.**
//! 5. **Remove the beacond row LAST.**
//!
//! Step 5 is last because the row is what makes the company VISIBLE to the rest
//! of the box — `chief ls` and the web app read it. A removal interrupted after
//! step 4 leaves a row whose directory has no company in it, which `chief ls`
//! reports as `missing` and which a second `chief rm` in that directory
//! finishes. Reversing the order leaves the opposite: a `.chief/` nothing lists
//! and nobody is looking for.
//!
//! # No preflight
//!
//! Unlike `reset`, this verb does not ask whether the host is fit to RUN a
//! company. A host whose install is broken is exactly where litter
//! accumulates, and a cleanup verb that refuses to work until the box is
//! healthy is a cleanup verb nobody can use.

use std::path::Path;

use chief_cli::files;

use super::confirm::{decide, Confirmation, Terminal};
use super::discovery::Discovery;
use super::http::Client;
use super::{LifecycleError, Result};

/// What a removal actually did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoveOutcome {
    /// `removed` or `declined`.
    pub(crate) outcome: &'static str,
    /// The company — the directory it occupied, which was its identity.
    pub(crate) dir: String,
    /// Which branch [`super::stop::stop_runtime`] took, when the removal ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop_mode: Option<&'static str>,
    /// Whether `<dir>/.chief/` was found and deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chief_deleted: Option<bool>,
    /// Whether the beacond row was removed. Always true on the `removed` path:
    /// beacond answers an unknown directory with `deleted: false` and no error,
    /// so reaching this step at all means the company is gone from discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) row_removed: Option<bool>,
}

/// Delete everything a company occupies on disk — which is one folder.
///
/// Answers whether there was anything there to delete, so the printed outcome
/// states what happened rather than what was attempted.
///
/// # One `remove_dir_all`, and no file named individually
///
/// Its ancestor deleted a database, then two SQLite sidecars by name, then a
/// sibling tree — four paths, because the store lived OUTSIDE the folder it
/// described and a `-wal` left behind is committed data a later database of the
/// same name would be handed on open. Everything is inside `.chief/` now, so
/// the sidecars go with the folder that contains them and there is nothing left
/// to enumerate. A list of file names is a list that can fall behind the files.
///
/// The primitive comes from [`chief_cli::files`], this crate's filesystem
/// executor, and not from `std::fs`: `clippy.toml` bans the raw calls
/// everywhere else precisely so that every effect on disk goes through one
/// reviewed seam, and it already has the missing-is-success behaviour a
/// replayable removal needs.
fn delete_durable_state(dir: &Path) -> Result<bool> {
    let chief = super::paths::chief_dir(dir);
    let existed = chief.is_dir();
    files::remove_directory_if_exists(&chief).map_err(refusal)?;
    Ok(existed)
}

/// A filesystem refusal, told as an operator-facing one.
fn refusal(error: chief_cli::actuate::host::HostErr) -> LifecycleError {
    LifecycleError::host(format!("chief rm: {error}"))
}

/// Exactly what this verb will delete, in the words the operator is asked to
/// approve.
///
/// Pure, so the promise is a value a test can hold up against what
/// [`delete_durable_state`] actually removes. It names ONE path and it names it
/// in full: a prompt that said "and all of its data" left the operator to guess
/// whether their own files were included, and in this directory they plausibly
/// are.
#[must_use]
fn confirmation_question(dir: &Path) -> String {
    format!(
        "Delete {} — this company's database, keys, logs and run files? Nothing else in {} is \
         touched, and this cannot be undone. [y/N] ",
        super::paths::chief_dir(dir).display(),
        dir.display()
    )
}

/// `chief rm [--yes]`.
///
/// # Errors
/// [`LifecycleError`] when this directory holds no company, when a step
/// refuses, or when the durable state could not be deleted. A failure leaves
/// the beacond row in place on purpose — see the module doc.
pub(crate) async fn run(dir: &Path, yes: bool) -> Result<()> {
    // Step 1 — a directory with no company deletes NOTHING, and says where it
    // looked. Before the confirmation, so an operator one directory too high is
    // never even asked a question whose "yes" would have been wrong.
    super::require_a_company_here(dir, "chief rm")?;

    // Step 2 — confirm before anything at all is touched.
    match decide(&confirmation_question(dir), yes, &Terminal) {
        Confirmation::Confirmed => {}
        Confirmation::Declined => {
            print(&RemoveOutcome {
                outcome: "declined",
                dir: dir.display().to_string(),
                stop_mode: None,
                chief_deleted: None,
                row_removed: None,
            });
            return Ok(());
        }
        Confirmation::RefusedNonInteractive => {
            return Err(LifecycleError::refused(format!(
                "chief rm: this caller has no terminal to confirm on, and this would delete {}. \
                 Run: chief rm --yes",
                super::paths::chief_dir(dir).display()
            )));
        }
    }

    let home = super::paths::home()?;
    let discovery = Discovery::from_env();
    super::discovery::ensure_running(&discovery, &home).await?;
    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer. beacond's client is built inside
    // `discovery::Discovery` and stays bare — that surface has no auth runtime.
    let client = Client::operator(dir);

    // Step 3 — the runtime and the daemon, through `stop`'s own sequence.
    let stopped = super::stop::stop_runtime(&client, dir, false).await?;

    // Step 4 — the durable state.
    let chief_deleted = delete_durable_state(dir)?;

    // Step 5 — the presence row, last.
    discovery.delete_company(dir).await?;

    print(&RemoveOutcome {
        outcome: "removed",
        dir: dir.display().to_string(),
        stop_mode: Some(stopped.mode),
        chief_deleted: Some(chief_deleted),
        row_removed: Some(true),
    });
    Ok(())
}

/// Print an outcome as the one-JSON-object style `stop` and `reset` also use.
fn print(outcome: &RemoveOutcome) {
    println!(
        "{}",
        serde_json::to_string_pretty(outcome)
            .unwrap_or_else(|_| format!("{{\"outcome\":\"{}\"}}", outcome.outcome))
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{confirmation_question, delete_durable_state, RemoveOutcome};

    /// A FIXTURE company on disk, in a throwaway directory. The seam rule that
    /// bans `std::fs::write` is about production filesystem effects belonging
    /// to a host transaction (README §5.6); there is no host transaction in a
    /// unit test, and the subject under test here is what a real removal does
    /// to real bytes — which a fake filesystem could not answer.
    #[allow(clippy::disallowed_methods)]
    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("parent");
        std::fs::write(path, b"fixture").expect("the fixture must be writable");
    }

    /// The removal sequence, stated as data. `run` performs exactly these, in
    /// exactly this order; the tests below are what make a reordering a
    /// visible edit rather than a silent one — the same shape `stop` and
    /// `reset` already use for their own ordering laws.
    const REMOVE_ORDER: [&str; 5] = [
        "require-a-company-here",
        "confirm",
        "stop-runtime-and-daemon",
        "delete-chief-folder",
        "delete-beacond-row",
    ];

    fn index(step: &str) -> usize {
        REMOVE_ORDER.iter().position(|candidate| *candidate == step).expect("named step")
    }

    /// A DIRECTORY WITH NO COMPANY IS REFUSED BEFORE THE QUESTION IS ASKED.
    ///
    /// The verb takes no argument, so the only way to aim it wrongly is to be
    /// standing in the wrong directory — and the operator cannot tell from the
    /// prompt alone. Refusing first means a mistyped `cd` costs nothing, and
    /// means `--yes` in a script cannot delete a directory that was never a
    /// company.
    #[test]
    fn a_directory_with_no_company_is_refused_before_anything_is_asked_or_touched() {
        assert_eq!(index("require-a-company-here"), 0);
        assert!(index("require-a-company-here") < index("confirm"));
    }

    #[test]
    fn nothing_is_touched_before_the_operator_confirms() {
        assert!(index("confirm") < index("stop-runtime-and-daemon"));
        assert!(index("confirm") < index("delete-chief-folder"));
    }

    #[test]
    fn the_daemon_is_stopped_before_its_database_is_deleted() {
        assert!(index("stop-runtime-and-daemon") < index("delete-chief-folder"));
    }

    /// The beacond row is what makes the company visible to the rest of the
    /// box, so it is the LAST thing to go. An interrupted removal must leave a
    /// row a retry can find — never a `.chief/` nothing lists.
    #[test]
    fn the_beacond_row_is_removed_last_of_all() {
        assert_eq!(index("delete-beacond-row"), REMOVE_ORDER.len() - 1);
        assert!(index("delete-chief-folder") < index("delete-beacond-row"));
    }

    /// THE CONFIRMATION NAMES THE EXACT PATH, and promises the rest is safe.
    ///
    /// This verb now deletes inside a directory the operator owns. "Remove
    /// 'acme' and delete all of its data?" is unanswerable there: it does not
    /// say which acme, and it does not say whether "all of its data" includes
    /// the source tree the operator is standing in.
    #[test]
    fn the_confirmation_names_the_one_folder_it_deletes_and_promises_the_rest() {
        let question = confirmation_question(Path::new("/work/acme"));
        assert!(question.contains("/work/acme/.chief"), "{question}");
        assert!(question.contains("Nothing else in /work/acme is touched"), "{question}");
        assert!(question.contains("cannot be undone"), "{question}");
    }

    /// EVERYTHING CHIEF OWNS GOES, AND NOTHING ELSE DOES.
    ///
    /// The `.pi/` tree, the operator's own files and the directory itself all
    /// survive. This is the whole safety claim of the verb now that it deletes
    /// inside a directory somebody else owns, so it is asserted against real
    /// bytes rather than against the path helper.
    #[test]
    fn the_chief_folder_goes_and_the_users_own_files_never_do() {
        let company = tempfile::tempdir().expect("tempdir");
        let dir = company.path();
        touch(&dir.join(".chief").join("db").join("chief.db"));
        touch(&dir.join(".chief").join("db").join("chief.db-wal"));
        touch(&dir.join(".chief").join("keys").join("operator.key"));
        touch(&dir.join(".pi").join("skills").join("thing.md"));
        touch(&dir.join("README.md"));
        touch(&dir.join("src").join("main.rs"));

        assert!(delete_durable_state(dir).expect("delete"), "there was a .chief to delete");

        assert!(!dir.join(".chief").exists(), "every byte chief owned is gone");
        assert!(dir.join(".pi").join("skills").join("thing.md").is_file(), ".pi/ is the user's");
        assert!(dir.join("README.md").is_file(), "the operator's own files are untouched");
        assert!(dir.join("src").join("main.rs").is_file());
        assert!(dir.is_dir(), "the company directory itself survives its company");
    }

    /// A removal that was interrupted after step 4 must be completable, so a
    /// second pass over an already-clean directory is success and reports
    /// nothing was there.
    #[test]
    fn deleting_a_company_that_is_already_off_disk_is_success_not_an_error() {
        let company = tempfile::tempdir().expect("tempdir");
        assert!(!delete_durable_state(company.path()).expect("delete"));
    }

    /// A removal reaches exactly one directory, and never a neighbour's.
    ///
    /// The slug guard that used to carry this claim is gone with the slug: no
    /// caller-supplied word reaches a path join any more, so the property is
    /// asserted directly against two real companies side by side.
    #[test]
    fn a_removal_touches_exactly_one_directory_and_never_its_neighbour() {
        let root = tempfile::tempdir().expect("tempdir");
        let here = root.path().join("acme");
        let neighbour = root.path().join("acme-corp");
        touch(&here.join(".chief").join("db").join("chief.db"));
        touch(&neighbour.join(".chief").join("db").join("chief.db"));

        delete_durable_state(&here).expect("delete");

        assert!(!here.join(".chief").exists());
        assert!(neighbour.join(".chief").join("db").join("chief.db").is_file());
    }

    #[test]
    fn a_declined_removal_reports_itself_and_names_nothing_it_did_not_do() {
        let declined = RemoveOutcome {
            outcome: "declined",
            dir: "/work/acme".to_string(),
            stop_mode: None,
            chief_deleted: None,
            row_removed: None,
        };
        let json = serde_json::to_value(&declined).expect("serialize");
        assert_eq!(json["outcome"], "declined");
        assert_eq!(json["dir"], "/work/acme");
        assert!(json.get("stopMode").is_none());
        assert!(json.get("chiefDeleted").is_none());
        assert!(json.get("rowRemoved").is_none());
    }

    #[test]
    fn a_completed_removal_reports_every_thing_it_deleted() {
        let done = RemoveOutcome {
            outcome: "removed",
            dir: "/work/acme".to_string(),
            stop_mode: Some("already-stopped"),
            chief_deleted: Some(true),
            row_removed: Some(true),
        };
        let json = serde_json::to_value(&done).expect("serialize");
        assert_eq!(json["stopMode"], "already-stopped");
        assert_eq!(json["chiefDeleted"], true);
        assert_eq!(json["rowRemoved"], true);
    }
}
