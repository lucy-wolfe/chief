//! The company's Pi project skills, RECONCILED to the shipped set.
//!
//! Chief copies the shipped company skills into `<dir>/.pi/skills` and holds
//! that root equal to what this release ships, on every launch.
//!
//! # Why this is a reconcile and not a genesis seed
//!
//! It used to be a one-time seed that stopped dead at the existence of the
//! destination: "An existing path is the complete stop condition. Chief does
//! not inspect it, add a newly shipped skill, restore a deleted skill, or
//! overwrite an edited skill." The consequence, unnoticed until the skill tree
//! changed: every company was frozen at whatever shipped the day it was
//! created. A company made a month ago would never have received the
//! `manager`/`worker` split, and the four deleted skills would have stayed in
//! it for ever — including a marketing department still holding chiefd's own
//! engineering-workflow skill.
//!
//! So the root is Chief's, and the shipped set is EXACTLY what it holds. A
//! directory here that this release does not ship is uninstalled, which is how
//! `browser`, `fal-ai`, `market-data`, `project-status-reporting` and
//! `organization-management` leave a company that already has them, with no
//! migration step and no version stamp to read.
//!
//! # It is writeless when converged
//!
//! Every entry is compared to the shipped tree before anything is written, so
//! the steady path stats the tree and returns. That matters for the same
//! reason it matters in `agent_contracts`: a pass that re-stamped every mtime
//! would make extension-drift detection meaningless.
//!
//! # The library is not what a person READS
//!
//! `<dir>/.chief/skills` is the LIBRARY. A skills directory installs exactly
//! ONE of them, because the skill set IS the role:
//!
//! * every ordinary person, through `<home>/.pi/skills` (`agent_home`);
//! * **and the CEO, through `<dir>/.pi/skills`,** which this module reconciles
//!   for the same reason and by the same rule.
//!
//! That second one is not a special case bolted on — it is the CEO's role
//! install, and it has to live here because the CEO is the one person with no
//! agent home at all. The Chief is the operator's own Pi, so Pi discovers its
//! skills as
//! PROJECT skills from its cwd, which is the company directory. Measured live
//! on 2026-08-19: a CEO's pane printed `[Skills] browser, fal-ai, market-data,
//! organization-management, project-status-reporting` — exactly the contents of
//! `<dir>/.pi/skills`, and nothing else.
//!
//! So a library left at `<dir>/.pi/skills` would have handed the CEO BOTH role
//! skills, and a manager reading "You do the work." is this whole change
//! inverted for the one person who manages everybody. The library moved under
//! `.chief/`, which is chief's own area, and `.pi/skills` became one link.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::executor::HostErr;

/// The skill the pre-company Founder session uses. It is shipped, but it is
/// never a company skill: no person in a company is a founder.
const FOUNDER_ONLY_SKILL: &str = "founder";

/// Result of a project skill reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSkillOutcome {
    /// The root already equalled the shipped set; nothing was written.
    Converged,
    /// One or more skills were installed, refreshed or uninstalled.
    Changed,
}

/// Hold the company skill library equal to the shipped company skills, and
/// install the CEO's role skill.
///
/// The source is `packages/piing/skills` from the pinned launcher checkout.
/// Every directory there is a company skill except [`FOUNDER_ONLY_SKILL`].
///
/// # Errors
///
/// Returns [`HostErr::Filesystem`] when the shipped tree cannot be read or the
/// destination cannot be written.
pub fn reconcile_project_skills(
    dir: &Path,
    shipped_skills_root: &Path,
) -> Result<ProjectSkillOutcome, HostErr> {
    if !shipped_skills_root.is_dir() {
        return Err(filesystem(format!(
            "shipped skill root {} is not a directory",
            shipped_skills_root.display()
        )));
    }
    let mut shipped = read_entries(shipped_skills_root)?;
    shipped.retain(|entry| entry.file_name() != OsStr::new(FOUNDER_ONLY_SKILL));
    if shipped.is_empty() {
        return Err(filesystem(format!(
            "shipped skill root {} contains no company skills",
            shipped_skills_root.display()
        )));
    }

    let destination = company_skill_library(dir);
    std::fs::create_dir_all(&destination)
        .map_err(|error| filesystem(format!("cannot create {}: {error}", destination.display())))?;

    let mut changed = false;
    for entry in &shipped {
        let target = destination.join(entry.file_name());
        if trees_match(&entry.path(), &target)? {
            continue;
        }
        // Replace whole rather than patch: a skill is a small tree, and a
        // partial overlay would leave a file the shipped skill has dropped.
        remove_path(&target)?;
        copy_entry(&entry.path(), &target)?;
        changed = true;
    }

    // THE UNINSTALL. Anything the release does not ship leaves, which is the
    // whole of how an existing company loses a retired skill.
    let shipped_names: Vec<_> = shipped.iter().map(std::fs::DirEntry::file_name).collect();
    for present in read_entries(&destination)? {
        if shipped_names.contains(&present.file_name()) {
            continue;
        }
        remove_path(&present.path())?;
        changed = true;
    }

    // THE CEO'S OWN ROLE INSTALL. `<dir>/.pi/skills` is a skills directory with
    // exactly one reader — the Chief, whose Pi discovers it as project skills
    // from its cwd — and the Chief is a manager. So it holds the manager skill
    // and nothing else, by the same rule every agent home follows.
    if install_role_skill(&chief_skills_root(dir), MANAGER_SKILL, Path::new("../../.chief/skills"))?
    {
        changed = true;
    }

    Ok(if changed { ProjectSkillOutcome::Changed } else { ProjectSkillOutcome::Converged })
}

/// The company skill LIBRARY, `<dir>/.chief/skills`.
///
/// Chief's own area, because chief holds it equal to the shipped set. It is
/// deliberately not `<dir>/.pi/skills`: that path is READ by the Chief's Pi as
/// project skills, so a library there is a skill set nobody chose.
#[must_use]
pub fn company_skill_library(dir: &Path) -> PathBuf {
    dir.join(".chief").join("skills")
}

/// The Chief's skills directory, `<dir>/.pi/skills` — the one skills directory
/// that is not inside an agent home, because the Chief has no agent home.
#[must_use]
pub fn chief_skills_root(dir: &Path) -> PathBuf {
    dir.join(".pi").join("skills")
}

/// The skill a manager has installed.
pub const MANAGER_SKILL: &str = "manager";

/// The skill a worker has installed.
pub const WORKER_SKILL: &str = "worker";

/// Reconcile one skills directory to hold EXACTLY the named role skill, linked
/// into the company library.
///
/// The single rule behind every skills directory in a company — the Chief's
/// `<dir>/.pi/skills` and every person's `<home>/skills` alike. `library` is the
/// relative path from INSIDE the skills directory to
/// [`company_skill_library`], which differs by depth and is the caller's to
/// supply.
///
/// Writeless when converged: an already-correct link is left alone, so the
/// steady path costs one `read_dir` and one `read_link` and never re-stamps an
/// mtime. Returns whether anything was installed or uninstalled.
///
/// A SYMLINK at the skills directory itself is the retired flat link every
/// company carried before this release; it is replaced, which is how an
/// existing company crosses over with no migration step.
///
/// # Errors
///
/// Returns [`HostErr::Filesystem`] on any filesystem failure.
pub fn install_role_skill(skills: &Path, role: &str, library: &Path) -> Result<bool, HostErr> {
    let target = library.join(role);
    match symlink_kind(skills)? {
        Some(EntryKind::Directory) => {}
        Some(_) => crate::files::remove_file_if_exists(skills)?,
        None => {}
    }
    std::fs::create_dir_all(skills)
        .map_err(|error| filesystem(format!("cannot create {}: {error}", skills.display())))?;

    let mut installed = false;
    let mut changed = false;
    for entry in read_entries(skills)? {
        let path = entry.path();
        let is_wanted = entry.file_name() == OsStr::new(role)
            && std::fs::read_link(&path).map(|found| found == target).unwrap_or(false);
        if is_wanted {
            installed = true;
            continue;
        }
        // Anything else is a skill this reader no longer has — the other
        // role's, or one of the skills this release deleted.
        remove_path(&path)?;
        changed = true;
    }
    if !installed {
        std::os::unix::fs::symlink(&target, skills.join(role)).map_err(|error| {
            filesystem(format!(
                "cannot install the skill {} -> {}: {error}",
                skills.join(role).display(),
                target.display()
            ))
        })?;
        changed = true;
    }
    Ok(changed)
}

/// Whether two paths are the same tree, byte for byte.
///
/// An absent destination is not a match, which is what makes a first
/// reconcile install everything.
fn trees_match(source: &Path, destination: &Path) -> Result<bool, HostErr> {
    let (Some(source_kind), Some(destination_kind)) =
        (symlink_kind(source)?, symlink_kind(destination)?)
    else {
        return Ok(false);
    };
    if source_kind != destination_kind {
        return Ok(false);
    }
    match source_kind {
        EntryKind::Symlink => {
            Ok(std::fs::read_link(source).ok() == std::fs::read_link(destination).ok())
        }
        EntryKind::File => Ok(std::fs::read(source).ok() == std::fs::read(destination).ok()),
        EntryKind::Directory => {
            let source_entries = read_entries(source)?;
            let destination_entries = read_entries(destination)?;
            if source_entries.len() != destination_entries.len() {
                return Ok(false);
            }
            for (left, right) in source_entries.iter().zip(destination_entries.iter()) {
                if left.file_name() != right.file_name() {
                    return Ok(false);
                }
                if !trees_match(&left.path(), &right.path())? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Symlink,
    File,
    Directory,
}

fn symlink_kind(path: &Path) -> Result<Option<EntryKind>, HostErr> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(Some(EntryKind::Symlink)),
        Ok(metadata) if metadata.is_dir() => Ok(Some(EntryKind::Directory)),
        Ok(metadata) if metadata.is_file() => Ok(Some(EntryKind::File)),
        Ok(_) => Err(filesystem(format!("{} has an unsupported file type", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(filesystem(format!("cannot inspect {}: {error}", path.display()))),
    }
}

fn remove_path(path: &Path) -> Result<(), HostErr> {
    match symlink_kind(path)? {
        None => Ok(()),
        Some(EntryKind::Directory) => crate::files::remove_directory_if_exists(path),
        Some(_) => crate::files::remove_file_if_exists(path),
    }
}

fn copy_entry(source: &Path, destination: &Path) -> Result<(), HostErr> {
    let metadata = std::fs::symlink_metadata(source).map_err(|error| {
        filesystem(format!("cannot inspect shipped skill {}: {error}", source.display()))
    })?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(source).map_err(|error| {
            filesystem(format!("cannot read shipped skill link {}: {error}", source.display()))
        })?;
        std::os::unix::fs::symlink(&target, destination).map_err(|error| {
            filesystem(format!(
                "cannot copy shipped skill link {} to {}: {error}",
                source.display(),
                destination.display()
            ))
        })?;
        return Ok(());
    }
    if metadata.is_dir() {
        std::fs::create_dir(destination).map_err(|error| {
            filesystem(format!("cannot create {}: {error}", destination.display()))
        })?;
        for entry in read_entries(source)? {
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        std::fs::set_permissions(destination, metadata.permissions()).map_err(|error| {
            filesystem(format!("cannot set permissions on {}: {error}", destination.display()))
        })?;
        return Ok(());
    }
    if metadata.is_file() {
        std::fs::copy(source, destination).map_err(|error| {
            filesystem(format!(
                "cannot copy shipped skill {} to {}: {error}",
                source.display(),
                destination.display()
            ))
        })?;
        return Ok(());
    }
    Err(filesystem(format!("shipped skill {} has an unsupported file type", source.display())))
}

fn read_entries(path: &Path) -> Result<Vec<std::fs::DirEntry>, HostErr> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|error| filesystem(format!("cannot read {}: {error}", path.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| filesystem(format!("cannot read {}: {error}", path.display())))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Ok(entries)
}

fn filesystem(detail: String) -> HostErr {
    HostErr::Filesystem { detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(path: &Path, contents: &str) {
        crate::files::publish_atomically(path, contents, 0o644).expect("fixture file");
    }

    fn shipped(root: &Path) -> PathBuf {
        let skills = root.join("shipped-skills");
        write(&skills.join("manager/SKILL.md"), "manager v1\n");
        write(&skills.join("worker/SKILL.md"), "worker v1\n");
        write(&skills.join("founder/SKILL.md"), "founder only\n");
        skills
    }

    #[test]
    fn a_fresh_root_gets_the_shipped_company_skills_not_the_founder_skill() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());

        assert_eq!(
            reconcile_project_skills(root.path(), &source).expect("reconcile"),
            ProjectSkillOutcome::Changed
        );
        let skills = company_skill_library(root.path());
        assert_eq!(
            std::fs::read_to_string(skills.join("manager/SKILL.md")).expect("manager skill"),
            "manager v1\n"
        );
        assert_eq!(
            std::fs::read_to_string(skills.join("worker/SKILL.md")).expect("worker skill"),
            "worker v1\n"
        );
        assert!(!skills.join(FOUNDER_ONLY_SKILL).exists(), "Founder skill is pre-company only");
    }

    /// THE COMPANY THAT WAS FROZEN. The seed this replaced stopped dead at the
    /// existence of `<dir>/.pi/skills`, so a company created before a skill
    /// change never received it — the whole `manager`/`worker` split would have
    /// reached new companies only. A root that already exists is reconciled.
    #[test]
    fn an_existing_root_created_before_this_release_receives_the_shipped_set() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());
        let skills = company_skill_library(root.path());
        // Exactly what a company stood up last month holds.
        write(&skills.join("organization-management/SKILL.md"), "the old management skill\n");
        write(&skills.join("market-data/SKILL.md"), "market data\n");
        write(&skills.join("project-status-reporting/SKILL.md"), "epics and QA columns\n");

        assert_eq!(
            reconcile_project_skills(root.path(), &source).expect("reconcile"),
            ProjectSkillOutcome::Changed
        );
        assert!(skills.join("manager/SKILL.md").is_file(), "the manager skill arrives");
        assert!(skills.join("worker/SKILL.md").is_file(), "the worker skill arrives");
        for retired in ["organization-management", "market-data", "project-status-reporting"] {
            assert!(!skills.join(retired).exists(), "{retired} is uninstalled");
        }
    }

    /// A newly shipped version of a skill REACHES a company that has the old
    /// one. The seed explicitly did not do this ("does not ... overwrite an
    /// edited skill"), which is the same freeze from the other side.
    #[test]
    fn a_changed_shipped_skill_replaces_the_installed_one() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());
        reconcile_project_skills(root.path(), &source).expect("first");
        let skills = company_skill_library(root.path());

        write(&source.join("manager/SKILL.md"), "manager v2 — you do not do the work\n");
        assert_eq!(
            reconcile_project_skills(root.path(), &source).expect("second"),
            ProjectSkillOutcome::Changed
        );
        assert_eq!(
            std::fs::read_to_string(skills.join("manager/SKILL.md")).expect("manager skill"),
            "manager v2 — you do not do the work\n"
        );
    }

    /// Writeless when converged — the property `agent_contracts` needs from
    /// every Chief-owned asset, so a pass does not re-stamp every mtime and
    /// defeat extension drift detection.
    #[test]
    fn a_converged_root_is_reported_converged_and_is_not_rewritten() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());
        reconcile_project_skills(root.path(), &source).expect("first");
        let skills = company_skill_library(root.path());
        let stamped = std::fs::metadata(skills.join("manager/SKILL.md"))
            .expect("metadata")
            .modified()
            .expect("mtime");

        assert_eq!(
            reconcile_project_skills(root.path(), &source).expect("second"),
            ProjectSkillOutcome::Converged
        );
        assert_eq!(
            std::fs::metadata(skills.join("manager/SKILL.md"))
                .expect("metadata")
                .modified()
                .expect("mtime"),
            stamped,
            "a converged pass must not rewrite a skill it already holds"
        );
    }

    /// THE CEO'S OWN INSTALL. The Chief has no agent home — it is the operator's
    /// own Pi — so Pi discovers its skills as PROJECT skills from its cwd, the
    /// company directory. Measured live: a CEO's pane printed exactly the
    /// contents of `<dir>/.pi/skills` and nothing else. A library left there
    /// would therefore hand the CEO BOTH role skills, and a manager reading
    /// "You do the work." is this whole change inverted for the one person who
    /// manages everybody.
    #[test]
    fn the_chiefs_skills_root_holds_the_manager_skill_and_never_the_worker_one() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());
        reconcile_project_skills(root.path(), &source).expect("reconcile");

        let chief = chief_skills_root(root.path());
        let mut present: Vec<String> = read_entries(&chief)
            .expect("entries")
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        present.sort();
        assert_eq!(present, vec!["manager".to_string()], "the Chief is a manager");
        assert_eq!(
            std::fs::read_to_string(chief.join("manager/SKILL.md"))
                .expect("readable through the link"),
            "manager v1\n"
        );
    }

    /// A company created before this release has the whole library sitting at
    /// `<dir>/.pi/skills`. The reconcile replaces it with the CEO's one install,
    /// which is how that company's CEO stops reading five skills.
    #[test]
    fn an_old_library_at_the_chiefs_skills_root_becomes_one_install() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());
        let chief = chief_skills_root(root.path());
        write(&chief.join("organization-management/SKILL.md"), "old\n");
        write(&chief.join("project-status-reporting/SKILL.md"), "epics\n");
        write(&chief.join("market-data/SKILL.md"), "prices\n");

        reconcile_project_skills(root.path(), &source).expect("reconcile");

        let mut present: Vec<String> = read_entries(&chief)
            .expect("entries")
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        present.sort();
        assert_eq!(present, vec!["manager".to_string()]);
    }

    /// The shipped set is EXACT. Anything else in the root is uninstalled —
    /// including something a user put there. That is the product decision this
    /// release makes: the company ships three skills and nothing else, so the
    /// root is Chief's rather than shared.
    #[test]
    fn the_root_holds_the_shipped_set_and_nothing_else() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = shipped(root.path());
        let skills = company_skill_library(root.path());
        write(&skills.join("something-else/SKILL.md"), "not shipped\n");

        reconcile_project_skills(root.path(), &source).expect("reconcile");
        let mut present: Vec<String> = read_entries(&skills)
            .expect("entries")
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        present.sort();
        assert_eq!(present, vec!["manager".to_string(), "worker".to_string()]);
    }
}
