//! `ensure_agent_home` — the whole of chief's involvement in an agent's home.
//!
//! # chief does not manage Pi configuration. Pi does.
//!
//! Operator ruling, 2026-08-27: *"let pi do its own inheritance stuff"*, and
//! when told plainly that this means every person shares one settings file so
//! anybody's `/model` moves everybody's default — *"that is fine. Don't copy on
//! hire. default is fine"*.
//!
//! Pi resolves its own user configuration from `~/.pi/agent` for ANY current
//! directory: `getAgentDir()` reads `$PI_CODING_AGENT_DIR` when set and falls
//! back to the home directory, and the cwd never enters that decision. So an
//! agent inherits the operator's sign-in, provider registry and defaults by
//! doing nothing at all.
//!
//! chief used to set `PI_CODING_AGENT_DIR` to each agent's home. That one line
//! is what made everything below necessary: with config scope moved into the
//! home, chief had to REBUILD the operator's configuration there, which it did
//! with three symlinks — `auth.json`, `settings.json`, `models.json`. Every
//! hazard that followed was downstream of a redirect Pi never asked for: links
//! that dangled forever on a box with no `models.json` (#1283), and a
//! `settings.json` that Pi WRITES in place, so one agent's `/model` rewrote the
//! operator's own file and moved every other agent's default (#1307).
//!
//! The redirect is gone and so are all three links. What is left is the part
//! that genuinely IS per person.
//!
//! # What a home holds now
//!
//! One folder per agent at `<dir>/.chief/agent/<person_id>/`, which is the
//! agent's CWD. It is no longer a Pi agent dir, so everything chief puts in it
//! is either project scope or chief's own:
//!
//! * **`.pi/skills/`** — a real directory holding exactly ONE symlink, named
//!   for the agent's role skill and pointing into the company skill library at
//!   `<dir>/.chief/skills/<role>`. The skill set IS the role. Reconciled on
//!   EVERY pass, because that reconcile is what makes a conversion real: a
//!   worker appointed head has `worker` uninstalled and `manager` installed on
//!   the next pass. Project scope, because the home is the cwd — the same way
//!   the CEO has always read its skills, having never had an agent dir at all.
//! * **`.pi/themes/`** — the generated `organization-<person>-{light,dark}.json`
//!   pair, Chief-owned and refreshed so accessibility fixes reach existing
//!   companies. Project scope for the same reason, and named by the setting in
//!   `.pi/settings.json` beside them.
//! * **`.pi/settings.json`** — theme only, and deliberately nothing else. A
//!   project value outranks the global one Pi persists to, so a `defaultModel`
//!   here would be a pin no `/model` could ever change.
//! * **`sessions/`** — a real directory. Transcripts are the one thing that is
//!   truly per person, so they keep their own directory through Pi's own
//!   first-class `PI_CODING_AGENT_SESSION_DIR`, with the on-disk layout
//!   unchanged.
//! * **`company`** — a symlink to `<dir>` itself, written relative so the
//!   company can be moved or copied.
//! * **`AGENTS.md`** — the one thing that goes stale, ON PURPOSE: the role
//!   contract as it stood at hire.
//!
//! # PROJECT SCOPE IS TRUST-GATED, so `--approve` is load-bearing
//!
//! Pi admits project-scope skills and themes only when the project is trusted
//! (`package-manager.js`, two `if (projectTrusted)` blocks). chief passes
//! `--approve` on every managed launch, and since the role skill and the
//! identity theme now live in project scope, THAT FLAG IS WHAT DELIVERS THEM.
//! Dropping it would not hang a headless agent on a prompt; it would launch
//! every person with no role and no identity, silently. See
//! `spawn_cmd::launch_command`.
//!
//! # Create agent content once; refresh what Chief owns
//!
//! Agent content is written at hire and never changed. The role skill and the
//! two theme files are the exceptions, and the role skill is more than an
//! exception — it is the one asset whose refresh CHANGES WHAT THE PERSON IS.
//!
//! # Recreating a home the user deleted is permitted
//!
//! "Once" is about chief not re-projecting over a live home, not about refusing
//! to build one that is missing. A user who deletes an agent's folder gets a
//! fresh one on the next hire-path call.
//!
use std::path::{Path, PathBuf};

use chiefd_core::store::organization::PersonKind;

use crate::identity_key::IDENTITY_KEY_FILENAME;
use crate::materialize::MaterializeError;

/// The folder chief owns for one agent: `<dir>/.chief/agent/<person_id>/`.
#[must_use]
pub fn agent_home(dir: &Path, person_id: &str) -> PathBuf {
    dir.join(".chief").join("agent").join(person_id)
}

/// The links every agent home carries, as `(name, target)`.
///
/// Written down once, here, because a link this list forgets is a capability
/// the agent silently does not have — and the failure surfaces as Pi behaving
/// as though the operator had no credentials rather than as a missing file.
///
/// `skills` is NOT here. It used to be — one flat
/// `("skills", "../../../.pi/skills")` link that gave every person in the
/// company the identical skill set, which is why a worker read the management
/// skill and a manager had nothing of its own. It is now a reconciled
/// directory; see [`install_role_skill`].
const RELATIVE_LINKS: [(&str, &str); 1] = [
    // `<home>/company` -> `<dir>`: the shared workspace IS the directory.
    ("company", "../../.."),
];

/// The role whose skill is installed in one agent's home.
///
/// This is the whole of the role model on disk. There are exactly two inside a
/// company — the third shipped skill, `founder`, belongs to the pre-company
/// Founder session, which has no agent home at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleSkill {
    /// An executive or a department head: the person who delegates.
    Manager,
    /// A person who does the work.
    Worker,
}

impl RoleSkill {
    /// The directory name under the company skill library, which is also the
    /// skill's `name:` frontmatter and the name of the link inside the agent's
    /// home.
    #[must_use]
    pub const fn directory_name(self) -> &'static str {
        match self {
            Self::Manager => crate::project_skills::MANAGER_SKILL,
            Self::Worker => crate::project_skills::WORKER_SKILL,
        }
    }

    /// The role a person's kind installs.
    ///
    /// `Executive` and `Head` are both managers — the distinction between them
    /// is scope, never duty, and both delegate. Only `Worker` does the work.
    #[must_use]
    pub const fn of(kind: PersonKind) -> Self {
        match kind {
            PersonKind::Executive | PersonKind::Head => Self::Manager,
            PersonKind::Worker => Self::Worker,
        }
    }
}
/// Reconcile `<home>/skills/` to hold EXACTLY the one role skill.
///
/// The rule itself lives in `project_skills`, because it governs the Chief's
/// `<dir>/.pi/skills` too and one rule with two implementations is two rules.
/// This supplies the depth: the link sits in
/// `<dir>/.chief/agent/<person_id>/skills/`, so up three is `<dir>/.chief` and
/// the library is `skills/<role>` from there.
///
/// It runs on every pass rather than once at hire, because that is what makes a
/// conversion take effect: a worker appointed head is `PersonKind::Head` in the
/// next manifest, and the next pass removes their `worker` link and writes a
/// `manager` one. An install that happened only at hire would leave a new head
/// reading the worker skill for the rest of their life.
///
/// Returns whether anything was installed or uninstalled, which the caller uses
/// to decide whether the role CONTRACT has to be republished with it.
pub fn install_role_skill_for(home: &Path, role: RoleSkill) -> Result<bool, MaterializeError> {
    install_role_skill(home, role)
}

fn install_role_skill(home: &Path, role: RoleSkill) -> Result<bool, MaterializeError> {
    crate::project_skills::install_role_skill(
        &home.join(".pi").join("skills"),
        role.directory_name(),
        // From `<dir>/.chief/agent/<person>/.pi/skills/<role>`: up four is
        // `<dir>/.chief`, and the library is `skills` from there. One level
        // deeper than it was, because the install moved out of the home's own
        // root and into its PROJECT scope.
        Path::new("../../../../skills"),
    )
    .map_err(|error| MaterializeError::filesystem(error.to_string()))
}

/// Create `<dir>/.chief/agent/<person_id>/` if it is absent. For an existing
/// home, refresh the Chief-owned organization theme files and the installed
/// role skill.
///
/// `role` is derived from the person's kind by [`RoleSkill::of`] and decides
/// which single skill is installed. Passing the other one is how a conversion
/// lands: this call uninstalls whatever is there and installs `role`.
///
/// Returns whether it created anything, so a caller can log the difference
/// between a hire and a no-op without stat-ing the tree itself.
///
/// # Errors
/// [`MaterializeError`] on any filesystem failure. Creating a home is the one
/// moment an agent's tree can be wrong, so this fails loudly rather than
/// leaving a half-built home for the launch gate to refuse later.
pub fn ensure_agent_home(
    dir: &Path,
    person_id: &str,
    identity_color: &str,
    agents_guide: &str,
    role: RoleSkill,
) -> Result<AgentHomeOutcome, MaterializeError> {
    let home = agent_home(dir, person_id);
    // Agent content remains create-once. The two `organization-*` theme files
    // are Chief-owned product assets and must follow the running release, and
    // the installed role skill is one too — more than that, it is the one asset
    // whose refresh CHANGES WHAT THE PERSON IS. A conversion is a kind change
    // in SQL; this line is where it reaches disk.
    if home.exists() {
        crate::agent_theme::write_agent_theme_files(&home, person_id, identity_color)?;
        // THE CONTRACT FOLLOWS THE ROLE, AND ONLY THE ROLE.
        //
        // `AGENTS.md` is create-once and goes stale on purpose — it is the role
        // contract as it stood at hire, stable across restarts and transfers.
        // A ROLE CHANGE is the one thing that makes it not stale but WRONG: a
        // worker appointed head would otherwise hold a contract opening "You
        // are a worker in Quant" while the skill installed beside it opens "You
        // are a manager. You do not do the work." The person would have to pick
        // one, and the operator who said "you head Platform now" would have
        // caused exactly the confusion the conversion copy exists to prevent.
        //
        // So the republish is fenced on the install having actually CHANGED,
        // not on this function running. An ordinary pass still rewrites
        // nothing, a mandate or department rename still does not reach a live
        // agent, and only a genuine conversion re-stamps the file.
        if install_role_skill(&home, role)? {
            crate::materialize::publish_text(&home.join("AGENTS.md"), agents_guide, 0o644)?;
        }
        return Ok(AgentHomeOutcome::AlreadyThere);
    }

    std::fs::create_dir_all(&home).map_err(|error| {
        MaterializeError::filesystem(format!("cannot create {}: {error}", home.display()))
    })?;
    // Pi fills this with the agent's own transcripts. A real directory, not a
    // link: sessions belong to this agent and to nobody else.
    std::fs::create_dir_all(home.join("sessions")).map_err(|error| {
        MaterializeError::filesystem(format!("cannot create sessions: {error}"))
    })?;

    // The role contract AS IT STOOD AT HIRE. Deliberately not refreshed later
    // — see the module header.
    crate::materialize::publish_text(&home.join("AGENTS.md"), agents_guide, 0o644)?;
    crate::agent_theme::write_agent_theme(&home, person_id, identity_color)?;

    for (name, target) in RELATIVE_LINKS {
        symlink(Path::new(target), &home.join(name))?;
    }
    install_role_skill(&home, role)?;

    Ok(AgentHomeOutcome::Created)
}

/// Whether a call built a home or found one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHomeOutcome {
    /// The home was absent and has been written.
    Created,
    /// The home was already there; only Chief-owned theme files were refreshed.
    AlreadyThere,
}

/// The identity key inside an agent home.
#[must_use]
pub fn identity_key_path(dir: &Path, person_id: &str) -> PathBuf {
    agent_home(dir, person_id).join(IDENTITY_KEY_FILENAME)
}

/// The Chief is the operator's own Pi and has no agent home. Its company
/// credential therefore lives directly under chief's private company data,
/// not in the `agent/` namespace.
#[must_use]
pub fn chief_identity_key_path(dir: &Path) -> PathBuf {
    dir.join(".chief").join(IDENTITY_KEY_FILENAME)
}
/// Write one link, tolerating a link that is ALREADY exactly the one we wanted.
///
/// Two production callers build the same person's home concurrently — the
/// roster-mutation route and the converge pass. When both pass the
/// `home.exists()` check before either has written anything, both take the
/// create branch. `create_dir_all` and the publish-by-rename writers are all
/// idempotent under that race; this was the one step that was not, so the
/// loser died on `EEXIST` and the operator got a warning about a home that was
/// in fact being built correctly by the winner.
///
/// The tolerance is deliberately narrow: only when the existing link already
/// points at the target we were about to write. A link pointing SOMEWHERE ELSE
/// is a real disagreement about the tree and still fails, because silently
/// accepting it would hide a home wired to the wrong company.
fn symlink(target: &Path, link: &Path) -> Result<(), MaterializeError> {
    match std::os::unix::fs::symlink(target, link) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::read_link(link) {
                Ok(existing) if existing == target => Ok(()),
                _ => Err(MaterializeError::filesystem(format!(
                    "cannot link {} -> {}: {error}",
                    link.display(),
                    target.display()
                ))),
            }
        }
        Err(error) => Err(MaterializeError::filesystem(format!(
            "cannot link {} -> {}: {error}",
            link.display(),
            target.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// The company skill library the launch path reconciles, written here so a
    /// home test can read a real skill THROUGH the install it makes.
    fn library(dir: &Path) {
        for role in ["manager", "worker"] {
            let skill = crate::project_skills::company_skill_library(dir).join(role);
            std::fs::create_dir_all(&skill).expect("skill dir");
            crate::files::publish_atomically(&skill.join("SKILL.md"), &format!("{role}\n"), 0o644)
                .expect("SKILL.md");
        }
    }

    /// The skills installed in one person's home, by name, sorted.
    fn installed(dir: &Path, person_id: &str) -> Vec<String> {
        let skills = agent_home(dir, person_id).join(".pi/skills");
        let mut names: Vec<String> = std::fs::read_dir(&skills)
            .expect("the install directory")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A directory standing in for the operator's own Pi agent dir.
    ///
    /// Nothing in a home points at it any more — `ensure_agent_home` does not
    /// take it, and Pi reaches the operator's configuration by its own
    /// inheritance. It survives here only as the LAUNCH GATE's subject, which
    /// asks whether that directory holds a provider configuration.
    fn operator_dir(root: &Path) -> PathBuf {
        let agent = root.join("operator-pi-agent");
        std::fs::create_dir_all(&agent).expect("operator agent dir");
        agent
    }

    /// THE TREE, asserted link by link rather than "some files exist".
    #[test]
    fn a_created_home_is_the_documented_tree() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        let operator = operator_dir(dir);

        let outcome = ensure_agent_home(
            dir,
            "quant-head",
            "#e24033",
            "# Chief — Head of Quant\n",
            RoleSkill::Manager,
        )
        .expect("create");
        assert_eq!(outcome, AgentHomeOutcome::Created);

        let home = agent_home(dir, "quant-head");
        assert!(home.join("sessions").is_dir(), "sessions is a REAL directory");
        assert_eq!(
            std::fs::read_to_string(home.join("AGENTS.md")).expect("guide"),
            "# Chief — Head of Quant\n"
        );

        // Relative, so moving or copying the whole company keeps them valid.
        for (name, target) in RELATIVE_LINKS {
            let link = home.join(name);
            assert_eq!(
                std::fs::read_link(&link).expect("a symlink"),
                PathBuf::from(target),
                "{name} must be a RELATIVE link"
            );
        }
        // THE HOME HOLDS NOTHING OF THE OPERATOR'S. This is the whole of the
        // 2026-08-27 ruling on disk: chief stopped redirecting
        // `PI_CODING_AGENT_DIR`, so the home is not a Pi agent dir and there is
        // nothing to reconstruct inside it. Pi reaches the operator's own
        // configuration by its own inheritance.
        for name in ["auth.json", "settings.json", "models.json"] {
            assert!(
                std::fs::symlink_metadata(home.join(name)).is_err(),
                "{name} must not exist in the home at all — not a link, not a copy"
            );
        }
        // And chief wrote nothing into the operator's directory either. It is
        // read-only to us, and now not even read.
        assert!(
            std::fs::read_dir(&operator).expect("operator dir").next().is_none(),
            "the operator's own agent dir must be untouched"
        );

        // PROJECT SCOPE is where a person's resources live, because the home is
        // the cwd and no longer an agent dir.
        assert!(
            home.join(".pi/skills/manager").is_symlink(),
            "the role skill installs into PROJECT scope"
        );
        for mode in ["light", "dark"] {
            assert!(
                home.join(format!(".pi/themes/organization-quant-head-{mode}.json")).is_file(),
                "the identity theme is a project resource beside the setting that names it"
            );
        }
    }

    /// A worker's own project setting selects one Pi-native automatic pair.
    /// The global settings link stays live and untouched; only the project
    /// theme key is person-specific. Both theme variants carry the same
    /// identity hue, but each has mode-correct backgrounds. The Chief never
    /// enters this writer and therefore keeps Pi's ordinary `light/dark` pair.
    ///
    /// # This file's ABSENCE of a model key is load-bearing, not incidental
    ///
    /// Pi resolves its default model from `deepMerge(global, project)` where
    /// PROJECT WINS, while `setModel` persists to GLOBAL only — so any
    /// `defaultModel` in a project settings file is a pin that no `/model`,
    /// and no `org_maintain_session set_model`, can ever change. The operator
    /// hit exactly that: their own `<company>/.pi/settings.json` pinned a
    /// model, the Chief's cwd is the company directory, and the Chief pane
    /// reverted on every restart while all nineteen workers kept the new
    /// model. The workers were spared only because THIS file — chief's own,
    /// in chief's own directory — carries a theme and nothing else.
    ///
    /// Operator ruling: nothing outside `/model` and an explicit instruction
    /// may change a person's model. Adding `defaultModel`, `defaultProvider`
    /// or `defaultThinkingLevel` here would hand every worker the Chief's bug
    /// silently, so the exact-equality assertion below is the guarantee and
    /// the named-key assertion after it survives anyone loosening that
    /// equality into a subset check.
    #[test]
    fn a_new_worker_home_has_one_adaptive_identity_theme_and_no_other_project_setting() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();

        ensure_agent_home(dir, "quant-head", "#e24033", "# Quant\n", RoleSkill::Manager)
            .expect("create");

        let home = agent_home(dir, "quant-head");
        let project_settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".pi/settings.json")).expect("project settings"),
        )
        .expect("valid settings");
        assert_eq!(
            project_settings,
            serde_json::json!({
                "theme": "organization-quant-head-light/organization-quant-head-dark"
            }),
            "the one-time project file must not override any unrelated Pi setting"
        );
        // Named explicitly, because these three are the ones that BITE: a
        // project value outranks the global one Pi writes, so any of them here
        // is a model, provider or reasoning level nobody can change again.
        for pinned in ["defaultModel", "defaultProvider", "defaultThinkingLevel"] {
            assert!(
                project_settings.get(pinned).is_none(),
                "chief must never pin {pinned} in a worker's project settings: project scope \
                 outranks the global scope Pi persists to, so it can never be changed again"
            );
        }
        // SUPERSEDED, and changed openly. This asserted that `settings.json`
        // stayed a LINK into the operator's file so ordinary settings remained
        // live. The operator ruled on 2026-08-27 that Pi should do its own
        // inheritance, so there is no file here at all: Pi resolves global
        // scope to the operator's own agent dir directly. The rule this test
        // exists for is untouched — the PROJECT file still holds a theme and
        // nothing else.
        assert!(
            std::fs::symlink_metadata(home.join("settings.json")).is_err(),
            "the home holds no global settings of its own (#1307)"
        );

        for mode in ["light", "dark"] {
            let theme: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(
                    home.join(format!(".pi/themes/organization-quant-head-{mode}.json")),
                )
                .unwrap_or_else(|_| panic!("missing {mode} theme")),
            )
            .expect("valid theme");
            assert_eq!(theme["name"], format!("organization-quant-head-{mode}"));
            assert_eq!(theme["colors"]["text"], "identity");
            assert_eq!(theme["colors"]["thinkingText"], "identity");
            assert!(
                theme["vars"]["identity"].as_str().is_some_and(|value| value.starts_with('#')),
                "{mode} identity text must resolve to one explicit color"
            );
        }
    }

    /// The installed skill resolves to the company's own library entry THROUGH
    /// the link, which is the mechanism the whole role model rests on.
    #[test]
    fn the_installed_skill_resolves_to_the_companys_own_skill_tree() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);

        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager).expect("create");

        // Resolved by the filesystem, not recomposed by the test: a link that
        // merely LOOKS right is what a string comparison would accept.
        let through = agent_home(dir, "vera").join(".pi/skills/manager/SKILL.md");
        assert_eq!(std::fs::read_to_string(through).expect("read through the link"), "manager\n");
    }

    /// THE ROLE IS THE INSTALLED SKILL SET, and the negative half is the half
    /// that was missing: before this, every person's home linked the WHOLE
    /// company skill tree, so a worker read the management skill — whose first
    /// line is "Your primary job is to delegate" — exactly as readily as a
    /// manager did.
    #[test]
    fn a_home_installs_its_own_role_skill_and_not_the_other_one() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);

        ensure_agent_home(dir, "ada", "#e24033", "# head\n", RoleSkill::Manager).expect("manager");
        ensure_agent_home(dir, "milo", "#3c7adf", "# worker\n", RoleSkill::Worker).expect("worker");

        assert_eq!(installed(dir, "ada"), vec!["manager".to_string()]);
        assert_eq!(installed(dir, "milo"), vec!["worker".to_string()]);
    }

    /// CONVERSION. The operator says "Milo, you head Platform now"; the
    /// appointment sets `PersonKind::Head`; the next pass must UNINSTALL the
    /// worker skill and INSTALL the manager one. An install that happened only
    /// at hire would leave a new head reading the worker skill for ever.
    #[test]
    fn converting_a_worker_to_a_manager_swaps_the_installed_skill() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);

        ensure_agent_home(dir, "milo", "#3c7adf", "# worker\n", RoleSkill::Worker).expect("hire");
        assert_eq!(installed(dir, "milo"), vec!["worker".to_string()]);

        let outcome = ensure_agent_home(
            dir,
            "milo",
            "#3c7adf",
            "# Department head — Milo\n",
            RoleSkill::Manager,
        )
        .expect("appoint");
        assert_eq!(outcome, AgentHomeOutcome::AlreadyThere, "conversion is not a rebuild");
        // The contract follows the role. A converted head holding a "# Worker"
        // contract beside a manager skill would have to choose which of the two
        // it believed.
        assert_eq!(
            std::fs::read_to_string(agent_home(dir, "milo").join("AGENTS.md")).expect("contract"),
            "# Department head — Milo\n"
        );
        assert_eq!(installed(dir, "milo"), vec!["manager".to_string()]);
        assert_eq!(
            std::fs::read_to_string(agent_home(dir, "milo").join(".pi/skills/manager/SKILL.md"))
                .expect("the manager skill is readable through the new link"),
            "manager\n"
        );
    }

    /// And back again — a manager who becomes a worker stops being able to
    /// read the management skill, which is what makes "uninstall the
    /// management skill and add the worker skill" a real statement about this
    /// product rather than a metaphor.
    #[test]
    fn converting_a_manager_back_to_a_worker_swaps_it_back() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);

        ensure_agent_home(dir, "ada", "#e24033", "# head\n", RoleSkill::Manager).expect("hire");
        ensure_agent_home(dir, "ada", "#e24033", "# head\n", RoleSkill::Worker).expect("step down");

        assert_eq!(installed(dir, "ada"), vec!["worker".to_string()]);
        assert!(
            !agent_home(dir, "ada").join(".pi/skills/manager").exists(),
            "the management skill is UNINSTALLED, not merely shadowed"
        );
    }

    /// A company created before this release has the retired flat `skills`
    /// SYMLINK in every home. It is replaced on the next pass, which is how an
    /// existing company crosses over with no migration step.
    #[test]
    fn the_retired_flat_skills_link_is_replaced_by_the_role_install() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager).expect("create");

        // Put the company back the way the previous release left it.
        let skills = agent_home(dir, "vera").join(".pi/skills");
        std::fs::remove_dir_all(&skills).expect("drop the role install");
        std::os::unix::fs::symlink("../../../.pi/skills", &skills).expect("the retired link");

        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager)
            .expect("next pass");
        assert!(
            std::fs::symlink_metadata(&skills).expect("skills").is_dir(),
            "the flat link is gone and a real directory stands in its place"
        );
        assert_eq!(installed(dir, "vera"), vec!["manager".to_string()]);
    }

    /// Writeless when converged, like every other Chief-owned asset here.
    #[test]
    fn a_converged_install_is_left_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager).expect("create");
        let link = agent_home(dir, "vera").join(".pi/skills/manager");
        let before = std::fs::symlink_metadata(&link).expect("link").mtime();

        let contract_before =
            std::fs::metadata(agent_home(dir, "vera").join("AGENTS.md")).expect("contract").mtime();
        ensure_agent_home(dir, "vera", "#e24033", "# a DIFFERENT guide\n", RoleSkill::Manager)
            .expect("second pass");
        assert_eq!(
            std::fs::symlink_metadata(&link).expect("link").mtime(),
            before,
            "an already-correct install must not be rewritten"
        );
        // And the contract stays create-once when the ROLE did not change, even
        // though the text handed in differs — that is the standing rule, and
        // the conversion republish above is fenced so as not to widen it.
        assert_eq!(
            std::fs::metadata(agent_home(dir, "vera").join("AGENTS.md")).expect("contract").mtime(),
            contract_before
        );
        assert_eq!(
            std::fs::read_to_string(agent_home(dir, "vera").join("AGENTS.md")).expect("contract"),
            "# guide\n"
        );
    }

    /// A second call preserves agent-owned content and settings but refreshes
    /// the two Chief-owned organization theme files.
    #[test]
    fn an_existing_home_gets_the_current_product_theme_and_keeps_agent_content() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        ensure_agent_home(dir, "vera", "#e24033", "# hire-time guide\n", RoleSkill::Manager)
            .expect("create");

        let home = agent_home(dir, "vera");
        let guide = home.join("AGENTS.md");
        crate::files::publish_atomically(&guide, "# the agent edited this\n", 0o644)
            .expect("the agent writes in its own home");
        let project_settings = home.join(".pi/settings.json");
        crate::files::publish_atomically(
            &project_settings,
            "{\"theme\":\"the-agent-chose-this\"}\n",
            0o644,
        )
        .expect("the agent changes its project theme");
        let light_theme = home.join(".pi/themes/organization-vera-light.json");
        let dark_theme = home.join(".pi/themes/organization-vera-dark.json");
        crate::files::publish_atomically(&light_theme, "{\"agent\":\"owns this\"}\n", 0o644)
            .expect("seed a stale generated theme");
        crate::files::publish_atomically(&dark_theme, "{\"stale\":true}\n", 0o644)
            .expect("seed the other stale generated theme");
        let extra = home.join("notes.md");
        crate::files::publish_atomically(&extra, "mine\n", 0o644).expect("and adds a file");

        let outcome =
            ensure_agent_home(dir, "vera", "#000000", "# a NEWER guide\n", RoleSkill::Manager)
                .expect("second call");

        assert_eq!(outcome, AgentHomeOutcome::AlreadyThere);
        assert_eq!(
            std::fs::read_to_string(&guide).expect("guide"),
            "# the agent edited this\n",
            "AGENTS.md is the hire-time contract and goes stale ON PURPOSE"
        );
        assert!(extra.exists(), "and nothing the agent added is swept");
        assert_eq!(
            std::fs::read_to_string(&project_settings).expect("project settings"),
            "{\"theme\":\"the-agent-chose-this\"}\n",
            "the create-once rule includes the project theme selection"
        );
        let refreshed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&light_theme).expect("light theme"))
                .expect("current generated theme");
        assert_eq!(refreshed["name"], "organization-vera-light");
        assert_eq!(refreshed["colors"]["customMessageText"], "identity");
        assert_eq!(refreshed["vars"]["customMsgBg"], "#ede7f6");
        let refreshed_dark: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dark_theme).expect("dark theme"))
                .expect("current generated theme");
        assert_eq!(refreshed_dark["name"], "organization-vera-dark");
        assert_eq!(refreshed_dark["vars"]["customMsgBg"], "#2d2838");

        let light_inode = std::fs::metadata(&light_theme).expect("light metadata").ino();
        let dark_inode = std::fs::metadata(&dark_theme).expect("dark metadata").ino();
        ensure_agent_home(dir, "vera", "#000000", "# newer again\n", RoleSkill::Manager)
            .expect("idempotent refresh");
        assert_eq!(std::fs::metadata(light_theme).expect("light metadata").ino(), light_inode);
        assert_eq!(std::fs::metadata(dark_theme).expect("dark metadata").ino(), dark_inode);
        assert_eq!(std::fs::read_to_string(guide).expect("guide"), "# the agent edited this\n");
        assert_eq!(
            std::fs::read_to_string(&project_settings).expect("project settings"),
            "{\"theme\":\"the-agent-chose-this\"}\n"
        );
        assert_eq!(std::fs::read_to_string(extra).expect("extra"), "mine\n");
    }

    /// THE REFRESH BRANCH MUST REPAIR EVERY ABSENCE IT CAN MEET.
    ///
    /// A home can exist without its theme directory. Two production callers run
    /// the home writer for the same new person at once — the roster-mutation
    /// route and the converge pass — so one of them can create the folder while
    /// the other, a moment later, sees `home.exists()` and takes the REFRESH
    /// branch against a home that is still being built.
    ///
    /// The refresh publishes through a trusted-parent primitive that opens
    /// `<home>/.pi/themes` with `O_DIRECTORY|O_NOFOLLOW` and fails ENOENT when
    /// it is not there. Nothing in the refresh branch created it — only the
    /// create branch did — so such a home errored on EVERY pass and was
    /// repaired by NONE, while the warning above the call promised that the
    /// next pass would repair it. That promise was false for exactly this
    /// error.
    ///
    /// The `?` also aborted BEFORE `install_role_skill`, so the half-built home
    /// never got its role skill either: the person stayed roleless for as long
    /// as the loop ran. Both halves are asserted here.
    #[test]
    fn a_home_that_exists_without_its_theme_directory_is_repaired_not_warned_forever() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);

        // The shape a concurrent creator leaves behind: the folder exists, and
        // nothing else does.
        let home = agent_home(dir, "vera");
        std::fs::create_dir_all(home.join("sessions")).expect("the folder, mid-create");

        let outcome = ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Worker)
            .expect("the refresh must REPAIR this home, not refuse it every pass");
        assert_eq!(outcome, AgentHomeOutcome::AlreadyThere);

        for mode in ["light", "dark"] {
            assert!(
                home.join(format!(".pi/themes/organization-vera-{mode}.json")).is_file(),
                "the refresh creates the directory it refreshes into"
            );
        }
        assert!(
            home.join(".pi/skills/worker").is_symlink(),
            "and the role skill is installed — the error used to abort before this line, \
             leaving the person with no role at all"
        );
    }

    /// AND THE REPAIR IS ONCE, not on every pass. A repair that rewrote the
    /// theme files each time would make mtime-based drift detection meaningless
    /// for every home that ever took this path.
    #[test]
    fn a_repaired_home_is_left_alone_by_the_next_pass() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);
        let home = agent_home(dir, "vera");
        std::fs::create_dir_all(home.join("sessions")).expect("the folder, mid-create");
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Worker).expect("repair");

        let light = home.join(".pi/themes/organization-vera-light.json");
        let dark = home.join(".pi/themes/organization-vera-dark.json");
        let light_inode = std::fs::metadata(&light).expect("light metadata").ino();
        let dark_inode = std::fs::metadata(&dark).expect("dark metadata").ino();

        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Worker)
            .expect("idempotent pass");

        assert_eq!(std::fs::metadata(&light).expect("light metadata").ino(), light_inode);
        assert_eq!(std::fs::metadata(&dark).expect("dark metadata").ino(), dark_inode);
    }

    /// THE OTHER INTERLEAVE OF THE SAME RACE: both callers pass the
    /// `exists()` check before either writes, so both run the CREATE branch.
    ///
    /// Everything else on that branch is idempotent — `create_dir_all`, and
    /// writers that publish by rename — so the `company` link was the only step
    /// that failed the second time through, with `EEXIST`. The loser printed a
    /// warning about a home the winner had just built correctly.
    #[test]
    fn a_second_create_pass_over_a_finished_home_is_silent() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Worker).expect("winner");

        // Exactly what the losing caller does: it already decided the home was
        // absent, so it runs the create branch over a home that now exists.
        let home = agent_home(dir, "vera");
        symlink(Path::new("../../.."), &home.join("company"))
            .expect("re-linking the SAME target must be silent, not EEXIST");
        assert_eq!(
            std::fs::read_link(home.join("company")).expect("the link"),
            PathBuf::from("../../..")
        );
    }

    /// AND THE TOLERANCE IS NARROW. A link that points somewhere else is a real
    /// disagreement about the tree, not a race, and must still fail — accepting
    /// it would leave a home wired to the wrong company and say nothing.
    #[test]
    fn a_company_link_pointing_elsewhere_is_still_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Worker).expect("create");
        let link = agent_home(dir, "vera").join("company");
        crate::files::remove_file_if_exists(&link).expect("drop the good link");
        std::os::unix::fs::symlink("../../../somewhere-else", &link).expect("a WRONG link");

        assert!(
            symlink(Path::new("../../.."), &link).is_err(),
            "a link to a different target is a disagreement, not a concurrent create"
        );
    }

    /// A DANGLING THEME-DIRECTORY SYMLINK IS REFUSED BY THE PRIMITIVE THAT
    /// OWNS THAT DECISION, not incidentally by the repair.
    ///
    /// The repair above acts only when the directory is genuinely ABSENT, and
    /// it asks with `symlink_metadata` — "is there an entry here?" — rather
    /// than `metadata`, which asks "does something resolve here?" and answers
    /// no for a symlink pointing at nothing.
    ///
    /// # What was CHECKED rather than assumed, because the obvious rationale
    /// # for that choice is wrong
    ///
    /// The tempting explanation is that `metadata` would let `create_dir_all`
    /// follow the link and create its target outside the home. Measured: it
    /// does not. `mkdir(2)` does not follow a final symlink, so `create_dir_all`
    /// on a dangling link fails `EEXIST` and creates nothing. Both probes are
    /// safe, and writing that hazard into the code would have been a false
    /// rationale for a correct line — the kind that survives review because it
    /// sounds right.
    ///
    /// The REAL difference is who refuses and what the operator is told. With
    /// `symlink_metadata` the entry is left alone and reaches the trusted-parent
    /// open, which refuses with `cannot open trusted directory …` — the
    /// component that owns the no-symlinked-parent rule, saying so in its own
    /// words. With `metadata` the repair grabs it first and dies on an
    /// incidental `File exists`, which names neither the rule nor the reason.
    /// That is what this test pins: not merely THAT it refuses, but that the
    /// refusal still comes from the trusted-parent check.
    ///
    /// The sibling test below uses a link to a directory that EXISTS, where
    /// both probes behave identically — so it cannot distinguish them, and
    /// without this test the choice would be unpinned.
    #[test]
    fn a_dangling_theme_directory_symlink_is_refused_and_its_target_is_not_created() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        library(dir);
        let home = agent_home(dir, "vera");
        std::fs::create_dir_all(home.join(".pi")).expect("project dir");
        let never_created = root.path().join("outside-target");
        std::os::unix::fs::symlink(&never_created, home.join(".pi/themes"))
            .expect("a theme directory that points at nothing");

        let outcome = ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Worker);

        let refusal = outcome.expect_err("a dangling theme parent must be REFUSED, not repaired");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("cannot open trusted directory"),
            "the trusted-parent check must be the thing that refuses, so the operator is told \
             which rule stopped them rather than being handed an incidental error from the \
             repair step: {refusal}"
        );
        assert!(
            !never_created.exists(),
            "and nothing may be created at the link's target: following it is the whole hazard"
        );
        assert!(
            std::fs::symlink_metadata(home.join(".pi/themes")).expect("the link").is_symlink(),
            "the link is left exactly as found — it is not ours to replace"
        );
    }

    #[test]
    fn an_existing_home_refuses_a_symlinked_theme_parent_without_writing_outside() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager).expect("create");

        let home = agent_home(dir, "vera");
        std::fs::remove_dir_all(home.join(".pi/themes")).expect("remove generated themes");
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).expect("outside directory");
        std::os::unix::fs::symlink(&outside, home.join(".pi/themes")).expect("redirect themes");

        let error = ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager)
            .expect_err("a symlinked managed parent is not trusted");
        assert!(error.to_string().contains("trusted directory"), "{error}");
        assert_eq!(std::fs::read_dir(outside).expect("outside").count(), 0);
    }

    /// A home the user deleted by hand comes back. "Once" is about not
    /// re-projecting over a LIVE home, never about refusing to build a missing
    /// one.
    #[test]
    fn a_home_the_user_deleted_is_created_again() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager).expect("create");

        std::fs::remove_dir_all(agent_home(dir, "vera")).expect("the user deletes it");

        assert_eq!(
            ensure_agent_home(dir, "vera", "#e24033", "# guide\n", RoleSkill::Manager)
                .expect("recreate"),
            AgentHomeOutcome::Created
        );
        // The install is present even though this company has no library yet:
        // the inner link is legitimately DANGLING between a hire and the
        // launch pass that reconciles the library, so it is inspected with
        // `symlink_metadata` rather than followed. Asserting through it would
        // demand an ordering the product does not promise.
        assert_eq!(installed(dir, "vera"), vec!["manager".to_string()]);
    }

    /// Two agents get two homes, and neither can reach into the other's.
    #[test]
    fn each_agent_gets_its_own_home() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path();
        ensure_agent_home(dir, "vera", "#e24033", "# vera\n", RoleSkill::Manager).expect("vera");
        ensure_agent_home(dir, "ada", "#3c7adf", "# ada\n", RoleSkill::Manager).expect("ada");

        assert_ne!(agent_home(dir, "vera"), agent_home(dir, "ada"));
        assert_eq!(
            std::fs::read_to_string(agent_home(dir, "ada").join("AGENTS.md")).expect("guide"),
            "# ada\n"
        );
        let identity = |person_id: &str| {
            let theme = std::fs::read_to_string(
                agent_home(dir, person_id)
                    .join(format!(".pi/themes/organization-{person_id}-dark.json")),
            )
            .expect("theme");
            serde_json::from_str::<serde_json::Value>(&theme).expect("valid theme")["vars"]
                ["identity"]
                .as_str()
                .expect("identity")
                .to_owned()
        };
        assert_ne!(identity("vera"), identity("ada"), "two workers need distinct identities");
    }

    #[test]
    fn the_chief_key_is_not_inside_an_agent_home() {
        let root = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            chief_identity_key_path(root.path()),
            root.path().join(".chief").join(IDENTITY_KEY_FILENAME)
        );
        assert!(!chief_identity_key_path(root.path()).starts_with(root.path().join(".chief/agent")));
    }
}
