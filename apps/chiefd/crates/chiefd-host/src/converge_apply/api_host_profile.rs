//! The read-only API-host launch profile.
//!
//! Native converge builds [`super::spawn_cmd::LaunchSpec`] for a runtime pane.
//! That type deliberately carries runtime identity and is paired with resource
//! discovery that may stage credentials immediately before a spawn. An
//! API-owned RPC child has a different boundary: it needs a serializable,
//! non-secret profile, and an HTTP read must not write Pi-home files. This
//! module is the only place that derives that profile from ChiefD's committed
//! manifest plus already-materialized Pi artifacts.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chiefd_core::actor::CompanyDb;
use chiefd_core::store::organization::{self, OrganizationManifest};
use thiserror::Error;

use super::resource_catalog::{
    person_tool_names, read_materialized_resources_for_api_host, MaterializedResources,
};
use super::safety::{self, ActuationMode};

/// The process-owned configuration needed to project an API child. This omits
/// every runtime-only field from [`super::cycle::ActuatorConfig`].
#[derive(Debug, Clone)]
pub struct ApiHostLaunchProfileConfig {
    /// The company DIRECTORY. Mirrors [`super::cycle::ActuatorConfig::dir`],
    /// for the same reason: the `.chief` root is derived by joining, never the
    /// other way round.
    pub dir: PathBuf,
    /// Explicit home passed to a child instead of inheriting an ambient home.
    pub home: PathBuf,
    /// The operator's own Pi agent directory. The Chief uses this directory;
    /// managed non-Chief people use their create-once company homes.
    pub root_pi_agent_dir: PathBuf,
    /// The launcher root required by the materialized intercom extension.
    pub launcher_root: PathBuf,
    /// One-shot latch, set by `chiefd run` once — and only once — this
    /// daemon has been admitted by beacond, has mounted its docstore listener,
    /// and has ensured the schema behind it.
    ///
    /// It used to carry the bound URL itself, because a child was handed that
    /// address as an environment stamp. The address is gone — a child resolves
    /// its own company through beacond — but the LATCH is not, and it never
    /// really was about the string. It is the ordering fact a projected child
    /// needs: beacond has no usable row for this company until the same
    /// moment, so projecting a child before the latch is set produces a child
    /// that cannot reach its own company, and a confusing extension-load
    /// failure instead of the clean typed refusal below.
    pub surface_bound: Arc<OnceLock<()>>,
}

impl ApiHostLaunchProfileConfig {
    /// Everything chief owns for this company: `<dir>/.chief`. Derived, never
    /// stored — see [`super::cycle::ActuatorConfig::data_root`].
    #[must_use]
    pub fn data_root(&self) -> PathBuf {
        self.dir.join(super::cycle::CHIEF_DIR)
    }
}

/// A fully resolved, non-secret input to one hosted agent.
///
/// Every field is a FACT about the person, not an invocation of a program.
/// This used to carry `rpc_args`: a `Vec<String>` of `--tools`/`--thinking`/
/// `--session` flags, because the consumer spawned Pi's CLI as a subprocess
/// and needed argv. The harness is now an in-process library, so argv has no
/// consumer — and the flag encoding actively LOST information a caller needs
/// back: a reader had to scan for `--session` and take the next element to
/// learn the transcript path, and `--tools` had to be re-split on commas. Two
/// parsers for one fact is what mandate 3 forbids, so the facts are carried as
/// facts.
///
/// `env` is an explicit overlay, never a serialization of the daemon's
/// inherited environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiHostLaunchProfile {
    /// Stable person id from the committed manifest.
    pub person_id: String,
    /// Materialized workspace used as the hosted agent's cwd.
    pub cwd: PathBuf,
    /// Explicit non-secret environment overlay for the agent.
    pub env: BTreeMap<String, String>,
    /// The person's own transcript, absent for somebody who has never spoken.
    pub session_file: Option<PathBuf>,
    /// Tool ids this person is allowed to call.
    pub tools: Vec<String>,
    /// How the person is titled to itself: "<company> · <title>".
    pub display_name: String,
}

/// Who is actuating this company, as a fact rather than a refusal.
///
/// # Why this replaced a gate
///
/// This read used to REFUSE unless the company was in shadow mode, so that a
/// route consumer could not stand up a second actuator beside the live runtime
/// convergence. That was chiefd deciding what a client may do with a fact it
/// asked for, and it made the profile unreadable by the very client the
/// runtime-out-of-chiefd split needs it for: a operator client runs against a company
/// in `apply` by definition, and it is the launch half of its own contract.
///
/// So the facts are published and the decision moves to the reader. Nothing is
/// lost — all three values the refusal carried are here — and `apps/web` makes
/// exactly the same call it did before, in its own words, one layer out. The
/// safety policy itself (which mode the company is in, the breaker, the ramp,
/// the budgets) stays chiefd's, unchanged; only the *sentence about what a
/// client should do next* left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActuationFacts {
    /// What the gate reads: `apply` or `shadow`. `effective_config()` forces
    /// shadow when the breaker is tripped, so this can differ from
    /// [`Self::configured_mode`].
    pub effective_mode: String,
    /// What the durable document says, before the breaker is applied.
    pub configured_mode: String,
    /// Whether the converge breaker is tripped. A tripped breaker forces
    /// shadow, so it explains an effective/configured disagreement in exactly
    /// one direction — and its absence is informative too.
    pub breaker_tripped: bool,
}

/// One read of the API-host launch profile: who is actuating, and the profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiHostLaunchProfileRead {
    /// Who is actuating this company.
    pub actuation: ActuationFacts,
    /// A profile per desired, fully materialized person, in `people_order`.
    pub profiles: Vec<ApiHostLaunchProfile>,
}

/// A typed reason an API-host profile cannot be supplied.
#[derive(Debug, Error)]
pub enum ApiHostLaunchProfileError {
    /// This daemon's docstore surface is not live yet, so a child projected
    /// now could not reach its own company through beacond either.
    #[error("the daemon's docstore surface has not bound yet")]
    SurfaceNotBound,
    /// The normalized manifest cannot be read from the committed authority.
    #[error("the organization manifest could not be read: {0}")]
    Manifest(String),
    /// The durable session epoch cannot be read.
    #[error("the session epoch could not be read: {0}")]
    SessionEpoch(String),
    /// A person's already-materialized Pi artifacts do not satisfy the API
    /// profile precondition.
    #[error("person '{person_id}' has no fully materialized API-host launch profile")]
    NotMaterialized {
        /// Person whose materialized profile is unavailable.
        person_id: String,
    },
}

impl ApiHostLaunchProfileError {
    /// Stable machine-readable refusal code for the HTTP surface.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SurfaceNotBound => "api-host-launch-profile-unavailable",
            Self::Manifest(_) => "api-host-launch-profile-unavailable",
            Self::SessionEpoch(_) => "api-host-launch-profile-unavailable",
            Self::NotMaterialized { .. } => "api-host-launch-profile-not-materialized",
        }
    }
}

/// A live, company-scoped source injected by `chiefd run` into the HTTP
/// surface. It owns no storage and performs no writes.
#[derive(Clone)]
pub struct ApiHostLaunchProfileSource {
    company: Arc<CompanyDb>,
    config: ApiHostLaunchProfileConfig,
}

impl ApiHostLaunchProfileSource {
    /// Construct the source over the same `CompanyDb` and immutable host
    /// configuration used by the live converge actuator.
    #[must_use]
    pub fn new(company: Arc<CompanyDb>, config: ApiHostLaunchProfileConfig) -> Self {
        Self { company, config }
    }

    /// Read the current API-host profile from committed durable state and
    /// already-materialized Pi artifacts.
    ///
    /// It no longer refuses `apply` mode. Reading a fact is not actuating on
    /// it, and the reader that must not stand up a second actuator is the only
    /// party that knows whether it is about to. Who is actuating rides on the
    /// answer instead, as [`ActuationFacts`].
    ///
    /// # Errors
    /// Returns a typed error when the daemon URL is unavailable or committed
    /// state cannot be read. A person whose Pi-home is not yet materialized is
    /// skipped, never fatal — one person's problem stays one person's problem.
    pub async fn read(&self) -> Result<ApiHostLaunchProfileRead, ApiHostLaunchProfileError> {
        let effective = safety::read_safety_config(&self.company);
        let (document, _) = self
            .company
            .read(|snapshot| chiefd_core::store::converge_safety::read(snapshot).into_parts());
        let actuation = ActuationFacts {
            effective_mode: mode_name(effective.actuation_mode),
            configured_mode: mode_name(document.actuation_mode),
            breaker_tripped: document.breaker_tripped,
        };
        let snapshot = self.company.snapshot();
        let org = organization::read(&snapshot)
            .map_err(|error| ApiHostLaunchProfileError::Manifest(error.to_string()))?;
        let session_epoch = self
            .company
            .session_epoch_read()
            .await
            .map_err(|error| ApiHostLaunchProfileError::SessionEpoch(error.to_string()))?
            .and_then(|(epoch, _seq)| chiefd_core::isotime::parse_iso_millis(&epoch.epoch_at))
            .and_then(|millis| u64::try_from(millis).ok())
            .map(|millis| UNIX_EPOCH + Duration::from_millis(millis));
        // Only the DESIRED people, judged by chiefd's own predicate.
        //
        // This route used to return a profile for every person in
        // `people_order` and leave "who should actually be running" to the
        // caller — and apps/api answered it with a TypeScript reimplementation
        // (`@chief/cli/roster`'s `desiredOrganizationPeople`) that read
        // `activity.people[id].active`. chiefd writes that field as
        // `lastDesiredActive`, so the TypeScript read `undefined`, defaulted to
        // false, and concluded NOBODY was ever desired. No agent was ever
        // launched under apps/api, and every attempt to talk to one answered
        // 409 `person-not-running`.
        //
        // Two implementations of one rule, in two languages, disagreeing about
        // a field name is precisely what mandate 3 forbids and precisely the
        // failure this program keeps producing. The answer belongs here, where
        // the rule and the data are both chiefd's.
        let activity = self
            .company
            .activity_read()
            .await
            .map_err(|error| ApiHostLaunchProfileError::Manifest(error.to_string()))?;
        // The predicate is reached through the published roster rather than
        // through a topology plan (#751/P10): a launch profile needs the desired
        // SET, and grouping that set into windows was never anything this route
        // read. `project_desired_roster` handles the never-converged company as
        // the same "person carries no decision" branch, so there is no second
        // arm here either.
        let desired = chiefd_core::runtime::roster::project_desired_roster(
            &org,
            activity.as_ref().map(|(ledger, _seq)| ledger),
        )
        .people
        .into_iter()
        .filter(|person| person.desired_active)
        .map(|person| person.id)
        .collect::<std::collections::BTreeSet<String>>();
        // Build ONLY for the desired, and skip a desired person whose Pi home
        // is not materialized rather than failing the whole read.
        //
        // This used to build for every person in `people_order` and `?` on the
        // first failure, so ONE person who could not be materialized made the
        // entire company unlaunchable — the CEO included. A company where
        // somebody had been hired but not yet materialized therefore had no
        // running agents at all, and the operator saw "Agent is dormant" on a
        // person who was perfectly fine. One person's problem must stay one
        // person's problem.
        //
        // The skip is not silent: the person simply does not appear in the
        // profile list, so the API host does not launch them and the operator
        // sees that person dormant — which is true — while everyone else runs.
        let mut profiles = Vec::new();
        for person_id in org.people_order.iter().filter(|id| desired.contains(*id)) {
            match build_api_host_launch_profile(&org, person_id, &self.config, session_epoch) {
                Ok(profile) => profiles.push(profile),
                Err(ApiHostLaunchProfileError::NotMaterialized { .. }) => continue,
                Err(other) => return Err(other),
            }
        }
        Ok(ApiHostLaunchProfileRead { actuation, profiles })
    }
}

/// The wire spelling of an actuation mode: lowercase, stable, and the same
/// vocabulary `chiefd set-actuation-config --mode` accepts.
fn mode_name(mode: ActuationMode) -> String {
    match mode {
        ActuationMode::Apply => "apply".to_owned(),
        ActuationMode::Shadow => "shadow".to_owned(),
    }
}

/// Derive every API child profile from one committed manifest.
///
/// The function is intentionally separate from the native runtime catalog. It
/// rejects incomplete materialization rather than calling a credential stager,
/// and it emits no runtime socket/session environment entries.
///
/// # Errors
/// Returns a typed refusal for a missing bound URL or an incomplete materialized
/// person profile.
/// One person's API-host launch profile.
///
/// Per-person on purpose: the batch form used to `?` on the first failure, so a
/// single unmaterialized person made the WHOLE company unlaunchable — including
/// people who were perfectly fine. Callers decide what a
/// [`ApiHostLaunchProfileError::NotMaterialized`] means for them; the daemon's
/// own reader skips that person and launches the rest.
///
/// # Errors
/// [`ApiHostLaunchProfileError::NotMaterialized`] when a non-Chief person has
/// no agent home or the Chief has no company identity key, or
/// [`ApiHostLaunchProfileError::SurfaceNotBound`] before this daemon's docstore
/// surface is live.
pub fn build_api_host_launch_profile(
    org: &OrganizationManifest,
    person_id: &str,
    config: &ApiHostLaunchProfileConfig,
    session_epoch: Option<SystemTime>,
) -> Result<ApiHostLaunchProfile, ApiHostLaunchProfileError> {
    if config.surface_bound.get().is_none() {
        return Err(ApiHostLaunchProfileError::SurfaceNotBound);
    }
    let person = org.people.get(person_id).ok_or_else(|| {
        ApiHostLaunchProfileError::NotMaterialized { person_id: person_id.to_owned() }
    })?;
    let is_chief = org.chief_person_id().is_ok_and(|chief| chief == person_id);
    // A non-Chief has ONE managed folder, and it is both the agent directory
    // and cwd. The Chief is the operator's own Pi: its cwd is the company
    // directory and its Pi identity stays in the operator's agent directory.
    let managed_home = crate::agent_home::agent_home(&config.dir, person_id);
    let (cwd, agent_dir, resources) = if is_chief {
        if !crate::agent_home::chief_identity_key_path(&config.dir).is_file() {
            return Err(ApiHostLaunchProfileError::NotMaterialized {
                person_id: person_id.to_owned(),
            });
        }
        (
            config.dir.clone(),
            config.root_pi_agent_dir.clone(),
            MaterializedResources { tools: person_tool_names(person), session: None },
        )
    } else {
        let resources = read_materialized_resources_for_api_host(
            person,
            &managed_home,
            &config.root_pi_agent_dir,
            session_epoch,
        )
        .ok_or_else(|| ApiHostLaunchProfileError::NotMaterialized {
            person_id: person_id.to_owned(),
        })?;
        (managed_home.clone(), managed_home, resources)
    };
    // TOMBSTONE: `ORG_LAUNCHER_RELOAD_HARD_CONTRACT`, read from
    // `.organization-reload-hard-contract.json` in the pi-home. It was a
    // re-projection's receipt, and nothing re-projects.
    let env = api_host_environment(org, person_id, &config.dir, &agent_dir, config);
    Ok(ApiHostLaunchProfile {
        person_id: person_id.to_owned(),
        cwd,
        env,
        session_file: resources.session.clone(),
        tools: resources.tools.clone(),
        // THE HEADER IS THE ROLE, AND ONLY THE ROLE. The username used to lead
        // it, which put a second identity in front of every reader while the
        // footer showed a third. One person is enough identities per pane: the
        // footer carries who you are, the header carries what you do.
        display_name: crate::person_presentation::role(&person.name, &person.title, is_chief),
    })
}

/// Every person's profile, in `people_order`. Refuses if ANY person is
/// incomplete — the strict form, kept for the callers that genuinely want
/// all-or-nothing.
///
/// # Errors
/// The first person's [`ApiHostLaunchProfileError`].
pub fn build_api_host_launch_profiles(
    org: &OrganizationManifest,
    config: &ApiHostLaunchProfileConfig,
    session_epoch: Option<SystemTime>,
) -> Result<Vec<ApiHostLaunchProfile>, ApiHostLaunchProfileError> {
    org.people_order
        .iter()
        .map(|person_id| build_api_host_launch_profile(org, person_id, config, session_epoch))
        .collect()
}

/// Construct the API child's explicit, non-secret environment overlay.
fn api_host_environment(
    org: &OrganizationManifest,
    person_id: &str,
    company_dir: &std::path::Path,
    agent_home: &std::path::Path,
    config: &ApiHostLaunchProfileConfig,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("CHIEFD_LAUNCH_MODE".to_owned(), "company".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
        ("HOME".to_owned(), config.home.display().to_string()),
        ("ORG_LAUNCHER_ORGANIZATION".to_owned(), org.slug.clone()),
        // The ONE pointer to the company. `ORG_LAUNCHER_DATA_ROOT` was here
        // too, carrying `config.data_root()` — now exactly `<company_dir>/.chief`
        // and therefore the same fact said twice. See `cycle.rs`.
        ("ORG_LAUNCHER_ORG_DIR".to_owned(), company_dir.display().to_string()),
        ("ORG_LAUNCHER_PERSON".to_owned(), person_id.to_owned()),
        // The USERNAME, display-only — see `cycle.rs` for why the slug alone
        // was not enough.
        (
            "ORG_LAUNCHER_PERSON_NAME".to_owned(),
            org.people
                .get(person_id)
                .map(|person| crate::person_presentation::handle(&person.name))
                .unwrap_or_else(|| "person".to_owned()),
        ),
        ("ORG_LAUNCHER_ROOT".to_owned(), config.launcher_root.display().to_string()),
        // Sessions only — see `cycle.rs`. Pi inherits the operator's own
        // `~/.pi/agent` for everything else, which is what deleted chief's
        // inherited-link machinery.
        (
            "PI_CODING_AGENT_SESSION_DIR".to_owned(),
            agent_home.join("sessions").display().to_string(),
        ),
    ]);
    // The one passthrough: the CA bundle a Pi child must trust to reach its
    // provider at all.
    //
    // This environment is otherwise built from scratch on purpose — a child
    // inherits nothing, so no ambient secret or stray setting can leak into a
    // person's process. But a deployment behind a TLS-intercepting egress
    // (which is the normal shape for a managed network, and is this fleet's
    // shape) presents a certificate chain that Node's BUNDLED CA store does
    // not know. Without the host's bundle the child's every provider call
    // fails "Connection error" with an empty transcript entry — the agent
    // launches, receives its message, and can never answer it.
    //
    // It is a PATH, never a credential, and it is only forwarded when the
    // operator set it: an unset variable stays unset, so nothing changes for
    // a deployment that does not need it.
    if let Ok(bundle) = std::env::var("NODE_EXTRA_CA_CERTS") {
        let bundle = bundle.trim();
        if !bundle.is_empty() {
            env.insert("NODE_EXTRA_CA_CERTS".to_owned(), bundle.to_owned());
        }
    }
    // #983: which company REGISTRY this box runs. Forwarded on exactly the
    // same terms as the CA bundle above — only when the operator set it, and
    // never a credential. It names one service per box, the same one for every
    // company, so unlike a per-company chiefd address it cannot silently point
    // a child at another company's data. Unset, the child uses beacond's own
    // compiled-in default, which is right for every non-test deployment.
    if let Ok(registry) = std::env::var("BEACOND_URL") {
        let registry = registry.trim();
        if !registry.is_empty() {
            env.insert("BEACOND_URL".to_owned(), registry.to_owned());
        }
    }
    env
}

/// Build only the supplemental RPC flags. The `RpcClient` owns `--mode rpc`
/// and adds provider/model from its dedicated options, so neither may appear
/// here.
#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::Path;

    use chiefd_core::test_support::northstar_manifest;

    /// Fixture writes still use the host filesystem seam. This keeps the
    /// no-write regression honest: the code under test must not use this helper
    /// or otherwise touch the materialized files after their snapshots.
    fn write(path: &Path, contents: &str) {
        crate::files::publish_atomically(path, contents, 0o644).expect("write fixture");
    }

    fn one_person_org() -> OrganizationManifest {
        let mut org = northstar_manifest(0);
        org.slug = "api-profile-cobalt".to_owned();
        // The profile builder consumes the committed `people_order`; keeping
        // one non-standard identity makes the theme, tools, thinking, and
        // session assertions focused without weakening the real manifest's
        // person record.
        org.people_order = vec!["signal-researcher".to_owned()];
        org
    }

    fn profile_config(root: &Path, surface_bound: bool) -> ApiHostLaunchProfileConfig {
        let latch = Arc::new(OnceLock::new());
        if surface_bound {
            latch.set(()).expect("latch the surface once");
        }
        ApiHostLaunchProfileConfig {
            dir: root.to_path_buf(),
            home: root.join("operator-home"),
            root_pi_agent_dir: root.join("operator-pi-agent"),
            launcher_root: root.join("launcher"),
            surface_bound: latch,
        }
    }

    /// The agent home the gate checks, as `ensure_agent_home` writes it.
    fn agent_home(config: &ApiHostLaunchProfileConfig, person_id: &str) -> PathBuf {
        let home = crate::agent_home::agent_home(&config.dir, person_id);
        fs::create_dir_all(home.join("sessions")).expect("sessions");
        // The inherited credential link, RESOLVING: the gate refuses a home
        // that reaches no provider, and this fixture's subject is the profile.
        fs::create_dir_all(&config.root_pi_agent_dir).expect("operator agent dir");
        write(&config.root_pi_agent_dir.join("auth.json"), "{}");
        std::os::unix::fs::symlink(
            config.root_pi_agent_dir.join("auth.json"),
            home.join("auth.json"),
        )
        .expect("the credential link");
        home
    }

    #[test]
    fn non_chief_profile_uses_its_managed_home_for_cwd_and_pi_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let org = one_person_org();
        let config = profile_config(dir.path(), true);
        let home = agent_home(&config, "signal-researcher");
        // THE LAYOUT PI ACTUALLY WRITES: a cwd-slug directory under
        // `sessions/`, and the transcript inside it. A flat fixture passed
        // while `latest_session` answered `None` for every real person.
        let session = home.join("sessions/--w--/2026-08-19T05-43-08-380Z_01a0188b.jsonl");
        let session_string = session.display().to_string();
        let home_string = home.display().to_string();
        write(&session, "{\"type\":\"session\",\"version\":3,\"id\":\"01a0188b\"}\n");

        let plans = build_api_host_launch_profiles(&org, &config, None).expect("profile");
        let [plan] = plans.as_slice() else { panic!("expected exactly one profile") };

        assert_eq!(plan.person_id, "signal-researcher");
        // THE HOME IS CWD, and cwd is now the whole of it: chief no longer
        // redirects Pi's agent dir, so the person inherits the operator's own
        // `~/.pi/agent` exactly as a person running Pi by hand would.
        assert_eq!(plan.cwd, home);
        assert!(
            !plan.env.contains_key("PI_CODING_AGENT_DIR"),
            "chief must not redirect Pi's config scope for anybody (#1307)"
        );
        assert_eq!(
            plan.env.get("PI_CODING_AGENT_SESSION_DIR"),
            Some(&format!("{home_string}/sessions")),
            "transcripts are the one thing that stays per person"
        );
        assert!(
            !plan.env.contains_key("PI_OFFLINE"),
            "plain Pi owns model refresh and the supported thinking-level list"
        );
        // The reload hard contract is DELETED, not merely unset here: it was a
        // re-projection's receipt and nothing re-projects.
        assert!(!plan.env.contains_key("ORG_LAUNCHER_RELOAD_HARD_CONTRACT"));

        for key in
            ["ORG_LAUNCHER_RUNTIME_SOCKET", "ORG_LAUNCHER_RUNTIME_SESSION", "OPENROUTER_API_KEY"]
        {
            assert!(!plan.env.contains_key(key), "API profile must not expose {key}");
        }
        // The transcript as a PATH. It used to be found by scanning argv for
        // `--session` and taking the next element, which is a parser the caller
        // had to own; getting it wrong hosts the agent on a new transcript and
        // silently loses the conversation.
        assert_eq!(
            plan.session_file.as_ref().map(|path| path.display().to_string()).as_deref(),
            Some(session_string.as_str())
        );
        // Tool ids as a LIST. They used to be one comma-joined string, so a
        // caller had to re-split it and a tool id containing a comma would have
        // split into two tools that do not exist.
        assert!(plan.tools.iter().any(|tool| tool == "read"));
        assert!(plan.tools.iter().any(|tool| tool == "org_send"));
    }

    #[test]
    fn chief_profile_uses_company_cwd_and_operator_pi_identity_without_a_managed_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let org = northstar_manifest(0);
        let config = profile_config(dir.path(), true);
        write(&crate::agent_home::chief_identity_key_path(&config.dir), "chief-key");

        let plan = build_api_host_launch_profile(&org, "chief", &config, None).expect("profile");

        assert_eq!(plan.cwd, config.dir);
        // The Chief never had a redirect and still does not; it is now the
        // same shape as everybody else rather than the exception.
        assert!(!plan.env.contains_key("PI_CODING_AGENT_DIR"));
        // The Chief's session store is the operator's own, so naming it
        // explicitly says the same thing Pi's default would have. It is stated
        // rather than left implicit because every other person's is redirected.
        assert_eq!(
            plan.env.get("PI_CODING_AGENT_SESSION_DIR"),
            Some(&format!("{}/sessions", config.root_pi_agent_dir.display()))
        );
        assert_eq!(plan.session_file, None);
        assert!(plan.tools.iter().any(|tool| tool == "org_send"));
        assert!(!crate::agent_home::agent_home(&config.dir, "chief").exists());
    }

    #[test]
    fn chief_profile_requires_the_company_identity_key_not_a_managed_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let org = northstar_manifest(0);
        let config = profile_config(dir.path(), true);
        agent_home(&config, "chief");

        let error = build_api_host_launch_profile(&org, "chief", &config, None)
            .expect_err("the managed Chief home cannot satisfy the Chief gate");

        assert!(matches!(
            error,
            ApiHostLaunchProfileError::NotMaterialized { person_id } if person_id == "chief"
        ));
    }

    #[test]
    fn beacond_url_reaches_the_api_child_only_when_the_operator_set_one() {
        // #983: a hosted person's extensions resolve THEIR OWN company's
        // daemon from beacond, so the child has to know which registry this
        // box runs. Forwarded on the same terms as the CA bundle — present
        // only when set, never as an empty assignment the client would then
        // have to parse into a malformed URL.
        let dir = tempfile::tempdir().expect("tempdir");
        let org = one_person_org();
        let config = profile_config(dir.path(), true);
        agent_home(&config, "signal-researcher");

        let _guard =
            crate::converge_apply::BEACOND_URL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("BEACOND_URL", "http://127.0.0.1:47302");
        let plans = build_api_host_launch_profiles(&org, &config, None).expect("profile");
        let [set] = plans.as_slice() else { panic!("expected exactly one profile") };
        assert_eq!(set.env.get("BEACOND_URL"), Some(&"http://127.0.0.1:47302".to_owned()));

        std::env::set_var("BEACOND_URL", "   ");
        let plans = build_api_host_launch_profiles(&org, &config, None).expect("profile");
        let [blank] = plans.as_slice() else { panic!("expected exactly one profile") };
        assert_eq!(blank.env.get("BEACOND_URL"), None);

        std::env::remove_var("BEACOND_URL");
        let plans = build_api_host_launch_profiles(&org, &config, None).expect("profile");
        let [unset] = plans.as_slice() else { panic!("expected exactly one profile") };
        assert_eq!(unset.env.get("BEACOND_URL"), None);
    }

    #[test]
    fn profile_refuses_until_this_daemons_docstore_surface_has_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let org = one_person_org();
        let config = profile_config(dir.path(), false);

        let error = build_api_host_launch_profiles(&org, &config, None).expect_err("must refuse");
        assert!(matches!(error, ApiHostLaunchProfileError::SurfaceNotBound));
    }
}
