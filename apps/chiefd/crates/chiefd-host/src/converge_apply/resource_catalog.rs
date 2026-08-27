//! Deriving one person's launch resources under the SA-2/SA-3 boot contract.
//!
//! Two derived inputs, and one gate.
//!
//! * **`--tools`**, composed from the person's SQL record: the Pi builtin
//!   floor, their declared grants, the intercom baseline, the subtree surface,
//!   and the root-executive tool for the structural root.
//! * **`--session`**, the newest Pi transcript for this person. Pi keys its
//!   transcripts by the cwd they were made in, so they live one directory down
//!   — `sessions/<cwd slug>/<timestamp>_<uuid>.jsonl` — and a person may have
//!   several slug directories. See [`latest_session`], and [`is_pi_transcript`]
//!   for what makes a file one of Pi's rather than one an agent left there.
//! * **THE GATE**, two questions. Does `<dir>/.chief/agent/<person_id>/` exist,
//!   and does it REACH A PROVIDER — `auth.json` or `models.json` resolving
//!   through the symlink into the operator's own Pi agent directory? A person
//!   failing either is omitted from the launch catalog and named in
//!   `LaunchCatalog::refusals`, so the client refuses that person's start step
//!   with a real reason and the next hire-path touch recreates the home.
//!
//! The second question is not a return to validating a projection. It asks
//! whether a link chief itself wrote RESOLVES, which the home's existence
//! cannot answer and which decides whether the person can think at all: a home
//! whose provider links both dangle used to pass, so chief started a whole
//! company of people whose only symptom was Pi's own banner inside their pane.
//!
//! # TOMBSTONE: the five-path managed set, the theme trio, and the credential
//!
//! The gate used to `symlink_metadata` five directories under
//! `people/<id>/{workspace,pi-home,pi-home/skills,pi-home/extensions}`, require
//! a generated theme trio for every non-standard identity, and STAGE the
//! operator's provider credential into the person's 0600 `auth.json` on its way
//! past. Every one of those validated a PROJECTION of SQL that no longer
//! exists: `ensure_agent_home` writes one folder, once, and it is symlinks by
//! design.
//!
//! Existence is therefore the whole check, and it is not a weakening. The five
//! paths were only ever a proxy for "did the materializer run for this person",
//! and that question now has a direct answer — the folder is there or it is
//! not. Checking the contents would be checking a projection again, which is
//! the thing this stage deletes.
//!
//! The symlink REJECTION is gone for a stated reason rather than a tidy-up: it
//! existed because a symlinked pi-home could redirect the staged credential
//! into another person's home. Chief stages no credential — `auth.json` is a
//! symlink into the operator's own agent dir, live by construction — so the
//! rule outlived its subject, which is the one condition under which a safety
//! check is deleted rather than weakened.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chiefd_core::store::organization::{PersonKind, PersonRecord};

// The builtin floor lives in `materialize::plan` because `RustToolListParity`
// pins it to that path against the TypeScript `BUILTIN_TOOLS`. It is imported
// rather than re-declared so the floor stays ONE list.
use crate::materialize::plan::BUILTIN_TOOLS;

/// Resolved per-person launch resources for the pane command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedResources {
    /// Granted tool names (`--tools`), derived from the person record.
    pub tools: Vec<String>,
    /// The transcript to resume, or `None` to start fresh.
    pub session: Option<PathBuf>,
}

// --- tool derivation (ported constants) ------------------------------------

/// `ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES`
/// (`packages/piing/src/extensionruntime/OrganizationTools.ts`, re-exported
/// through `packages/piing/extensions/organization-intercom.ts`): the baseline
/// intercom surface — including the seven unified task/subtask tools (E1,
/// #247), which are baseline surface exactly like the original three
/// (`ORGANIZATION_BASELINE_TOOL_NAMES`), including the self-service durable
/// reminder controls — plus the runtime-fenced contract tools.
///
/// This list is gate 3 of the three-gate tool grant (TS extension registers +
/// TS `planPerson` grants + this chiefd derivation) and MUST stay byte-for-byte
/// in sync with the TS constant: a name here that the extension never registers
/// is an allowlist entry Pi can never satisfy, and a name missing here is a
/// registered tool the pane is launched without.
const ACTIVE_RUNTIME_TOOLS: [&str; 5] =
    ["org_send", "org_roster", "org_create_reminder", "org_list_reminders", "org_stop_reminder"];

/// `ORGANIZATION_SUBTREE_TOOL_NAMES`
/// (`packages/piing/src/extensionruntime/OrganizationTools.ts`): the
/// subtree-growth surface EVERY person carries, whatever their kind.
///
/// # Why this is unconditional
///
/// Every leaf can become a parent — anyone may create a department beneath
/// themselves, head it, and staff it. The intercom's own
/// `authorityRootDepartmentId` has always said so ("creating a child unit
/// takes no authority over anybody who already exists, and the creator becomes
/// the new unit's head"), and each handler below checks SUBTREE SCOPE, never a
/// job title. This grant sat behind [`is_manager`] anyway, so a worker's pane
/// was launched without `org_add_department` and the rule was unreachable: the
/// authority layer said yes and the model was never offered the tool to ask
/// with. Exactly the shape of the builtin-floor decision above — a default is
/// overridable, a floor is not, and this is a floor.
///
/// Granting these takes authority over nobody. A person who heads no unit has
/// an empty `departmentIsInScope`, so every verb here refuses for them until
/// they create a unit of their own; the refusals in between ARE the safety
/// model. Growth is downward only — nothing here reaches a peer or a manager.
///
/// Same gate-3 parity rule as every other list: byte-for-byte with the TS
/// constant, because Pi filters the model toolset to this `--tools` allowlist
/// and a name missing here is SILENTLY STRIPPED on the live daemon (the #30
/// class). The TS list is the authority; it currently has 23 entries.
const SUBTREE_TOOLS: [&str; 21] = [
    "org_launch_department",
    "org_stop_department",
    "org_remove_department",
    "org_launch_contract",
    "org_stop_contract",
    "org_remove_contract",
    "org_add_department",
    "org_pause_department",
    "org_resume_department",
    "org_resume_departments",
    "org_hire",
    "org_bench",
    "org_recall",
    "org_start_person",
    "org_stop_person",
    "org_transfer",
    "org_offboard",
    // #restructure keystones (TS "Operator ruling 2026-07-24"): a converged
    // manager MUST be able to move a department and appoint/replace a head.
    "org_reparent_department",
    "org_appoint_department_head",
    // org-ops R5: move only a department's PEOPLE to another node in one atomic
    // revision (the head stays).
    "org_move_department_members",
    // The former `MANAGER_TOOLS`. Fenced SERVER-SIDE now, so the pane's
    // `--tools` allowlist no longer has to withhold it from a leaf:
    // `org_lifecycle_status` reaches a board whose scope the server derives
    // from the caller rather than taking from the request.
    //
    // TOMBSTONE: `org_maintain_session` stood beside it for the same reason.
    // The whole tool is deleted — operator ruling, 2026-08-24.
    "org_lifecycle_status",
];

/// `ORGANIZATION_MANAGER_TOOL_NAMES` (`packages/piing/extensions/organization-intercom.ts`).
/// MUST stay in exact sync with that TS list — this is gate 3 of the three-gate
/// tool grant (extension registers + TS `planPerson` grants + this chiefd
/// re-derivation grants), and Pi filters the model toolset to this `--tools`
/// allowlist, so anything the TS grants but this omits is SILENTLY STRIPPED on
/// the live daemon (the #30 class). The TS list is the authority; it currently
/// has 0 entries.
///
/// IT IS EMPTY, and the emptiness is the statement.
///
/// It held the lifecycle control board, session maintenance and the managed
/// thinking change — the three whose handlers refused a non-manager outright.
/// They were role-gated because their ROUTES enforced nothing, so the
/// TypeScript check was the authorization itself rather than a pre-flight in
/// front of one. Every one of them is now fenced server-side, so all three
/// moved to [`SUBTREE_TOOLS`], which every person carries.
///
/// DO NOT DELETE THIS AS DEAD CODE. It is empty on purpose, and the emptiness
/// is load-bearing: the parity guard pins it against the TS authority, so an
/// EMPTY list that is CHECKED says "no tool is granted by kind" and FAILS the
/// moment somebody adds one back without an argument. A deleted constant says
/// nothing at all — and the guard that reads it would have to go too, which is
/// how a retired model quietly returns: the thing being guarded and the guard
/// leave in the same tidy-up. The grant block below stays with it so the list
/// remains the single control point; re-arming the grant is one edit here, not
/// a re-invention of the wiring.
const MANAGER_TOOLS: [&str; 0] = [];

/// `ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES`
/// (`packages/piing/extensions/organization-intercom.ts:144`). The intercom extension registers
/// these tools only for the structural root — the person whose direct
/// manager resolves to undefined. Pi filters the model's toolset to this launch
/// `--tools` allowlist, so the grant must include them or the CEO can never see
/// a tool the extension registered (#15, this is the THIRD gate: extension
/// registers, TS `planPerson` grants, and this Rust actuator grants). `kind ==
/// Executive` is that structural root for every well-formed manifest (the same
/// executive signal); a department head is `kind == Head`
/// and must NOT receive it. Order mirrors the TS constant.
///
/// `org_stand_down` and `org_resume` are here because an operator's "stop all
/// work" is a decision about the COMPANY, and this gate is the only one whose
/// subject is the company. A live company was given exactly that instruction,
/// the CEO obeyed it, and everybody came back forty-five seconds later; the
/// tool that could have expressed it did not exist. See
/// `chiefd_core::store::stand_down`.
const ROOT_EXECUTIVE_TOOLS: [&str; 3] =
    ["org_escalate_to_operator", "org_stand_down", "org_resume"];

/// The exact `--tools` grant for one person, derived from the person record —
/// the port of `organizationPersonToolNames` (`org-materialize.ts:471-473`)
/// restricted to the record-only parts of `planPerson`: resource-alias
/// resolution and conflict validation belong to materialization, which already
/// ran them.
///
/// # The Pi builtin floor is UNCONDITIONAL (operator decision, 2026-08-10)
///
/// "The basic PI tools, the standard ones, every agent should have those
/// tools. Do not block them... every node, every agent should be the same. You
/// should give them the basic Pi. The only difference is they get more skills
/// for their role."
///
/// So [`BUILTIN_TOOLS`] is composed here, ahead of everything, for every
/// person on every path — not defaulted into `person.tools` when a seed omits
/// the field. The distinction is the whole point: a default can be defeated by
/// naming one tool (`tools: ["read"]` used to silently cost you the other
/// six), and a default written at genesis is a second mechanism that
/// create/hire never had, which is exactly how a live company ended up with 23
/// people who could not open a file. Composed, the floor cannot be amputated,
/// cannot drift between creation paths, and needs no migration to reach people
/// who already exist.
///
/// Order: the builtin floor, then the person's own declared grants (what their
/// extensions and packages export), then intercom + memory + subtree tools
/// (+ manager tools for a manager), root-executive tools, loop tools for
/// monitored workers; first occurrence wins on duplicates.
#[must_use]
pub fn person_tool_names(person: &PersonRecord) -> Vec<String> {
    let mut tools: Vec<String> = Vec::new();
    let mut push = |tool: &str| {
        if !tools.iter().any(|granted| granted == tool) {
            tools.push(tool.to_owned());
        }
    };
    for tool in BUILTIN_TOOLS {
        push(tool);
    }
    for tool in &person.tools {
        push(tool);
    }
    for tool in ACTIVE_RUNTIME_TOOLS {
        push(tool);
    }
    // Unconditional, like the builtin floor above and for the same reason: the
    // authority layer already lets any person grow a unit beneath themselves,
    // and a tool the catalog never offers is a rule that cannot be exercised.
    for tool in SUBTREE_TOOLS {
        push(tool);
    }
    // The kind-gated grant, retained deliberately over an EMPTY list rather
    // than deleted with its contents. `MANAGER_TOOLS` is the single control
    // point: adding a name back to it re-arms this grant without a second
    // edit, and the parity guard fails first if the TS authority disagrees.
    // Deleting the block would leave the constant unused and the next person
    // re-inventing the wiring.
    if is_manager(person) {
        for tool in MANAGER_TOOLS {
            push(tool);
        }
    }
    if person.kind == PersonKind::Executive {
        for tool in ROOT_EXECUTIVE_TOOLS {
            push(tool);
        }
    }
    tools
}

fn is_manager(person: &PersonRecord) -> bool {
    matches!(person.kind, PersonKind::Executive | PersonKind::Head)
}

// --- the read-back ----------------------------------------------------------

/// Read one person's launch resources for the pane command.
///
/// `agent_home` is `<dir>/.chief/agent/<person_id>/` — the folder that IS the
/// agent's Pi agent dir and its cwd.
///
/// Returns `None` — the person is omitted from the launch catalog, which
/// surfaces as a named refusal the client reports and the next pass retries —
/// when that folder does not exist.
#[must_use]
pub fn read_materialized_resources(
    person: &PersonRecord,
    agent_home: &Path,
    operator_agent_dir: &Path,
) -> Option<MaterializedResources> {
    read_materialized_resources_for_launch(person, agent_home, operator_agent_dir, None, false)
}

/// Read one launch catalog entry with the durable session-selection inputs the
/// Rust actuator owns for this pass.
#[must_use]
pub fn read_materialized_resources_for_launch(
    person: &PersonRecord,
    agent_home: &Path,
    operator_agent_dir: &Path,
    session_epoch: Option<SystemTime>,
    force_fresh: bool,
) -> Option<MaterializedResources> {
    // `metadata` FOLLOWS, everywhere in this gate. The home is a real directory
    // by construction and the role skill inside it is a link, so a gate that
    // refused a symlink would refuse every person.
    if !fs::metadata(agent_home).is_ok_and(|metadata| metadata.is_dir()) {
        return None;
    }
    // AND THE COMPANY MUST BE ABLE TO REACH A PROVIDER. The question is asked
    // of the OPERATOR's directory now, not this home — every person inherits
    // that one directory, so it is one answer rather than one per person. See
    // [`provider_configuration_resolves`].
    if !provider_configuration_resolves(operator_agent_dir) {
        return None;
    }
    Some(MaterializedResources {
        tools: person_tool_names(person),
        session: latest_session(&agent_home.join("sessions"), session_epoch, force_fresh),
    })
}

/// Read the already-created launch facts an API-hosted RPC child may use.
///
/// The same gate and the same session selection as the pane path. It was a
/// separate function because the pane path had a SIDE EFFECT this one had to
/// refuse — staging the selected provider credential — and there is no
/// credential to stage. It stays separate only because API hosting has no
/// pane-only forced-fresh request and therefore always passes `false`.
#[must_use]
pub fn read_materialized_resources_for_api_host(
    person: &PersonRecord,
    agent_home: &Path,
    operator_agent_dir: &Path,
    session_epoch: Option<SystemTime>,
) -> Option<MaterializedResources> {
    read_materialized_resources_for_launch(
        person,
        agent_home,
        operator_agent_dir,
        session_epoch,
        false,
    )
}

/// Diagnostic-only re-derivation of WHY
/// [`read_materialized_resources_for_launch`] produced its `None`, for a caller
/// that already observed it and is about to report a refusal to a human. Never
/// called on the hot path, and it never changes what that function returns:
/// it asks the identical question purely so the answer can NAME the missing
/// home instead of collapsing into an interchangeable "refused".
#[must_use]
pub fn explain_launch_refusal(agent_home: &Path, operator_agent_dir: &Path) -> Option<String> {
    if !fs::metadata(agent_home).is_ok_and(|metadata| metadata.is_dir()) {
        return Some(format!(
            "this person has no agent home ({}); the next hire-path pass creates it",
            agent_home.display()
        ));
    }
    if !provider_configuration_resolves(operator_agent_dir) {
        return Some(format!(
            "the operator's own Pi agent directory ({}) reaches no provider configuration: \
             neither {} nor {} is a file there. Every person inherits that directory directly, so \
             this refuses the whole company at once rather than one person — sign Pi in, or write \
             its models.json, and the next pass starts them",
            operator_agent_dir.display(),
            PROVIDER_FILES[0],
            PROVIDER_FILES[1]
        ));
    }
    None
}

/// The two files that decide whether a person can reach a model at all, in the
/// order the refusal names them.
///
/// They are read in the OPERATOR's own Pi agent directory. They used to be
/// read through symlinks inside each home, which is the same question asked
/// once per person about copies of one answer.
const PROVIDER_FILES: [&str; 2] = ["auth.json", "models.json"];

/// Does this home reach a provider configuration — does at least one of
/// [`PROVIDER_FILES`] RESOLVE to a file?
///
/// # The company of people who could not think
///
/// The gate was the home's existence alone, and a home is symlinks by design,
/// so a home whose `auth.json` and `models.json` both DANGLE passed it. chiefd
/// then published a full launch spec for every person, the client started them
/// all, and each pane came up as a Pi with no way to reach a model — printing
/// its own banner inside the pane and nowhere else. Every surface chief owns
/// said the company was healthy. The suite's own instructions carry the
/// work-around this replaces: "confirm the operator Pi agent directory has a
/// usable provider … chief's launch gate does not check this".
///
/// # Why EITHER file, and not both
///
/// They are different halves of the same question and an operator legitimately
/// has one without the other. `auth.json` is what a Pi sign-in writes;
/// `models.json` is a provider registry an operator may write by hand against
/// an environment key, which is exactly the shape of the test box this bug was
/// found on. Requiring both would refuse a working company; requiring neither
/// is the bug. One resolving file means there is a path to a model, and what is
/// IN it is Pi's question, not chief's — chief holds no credential and must not
/// start reading one.
///
/// # `metadata`, so it FOLLOWS
///
/// A dangling symlink is the whole subject: `symlink_metadata` would answer
/// "there is a link here" for exactly the state that cannot work.
fn provider_configuration_resolves(operator_agent_dir: &Path) -> bool {
    PROVIDER_FILES.iter().any(|name| fs::metadata(operator_agent_dir.join(name)).is_ok())
}

/// The newest Pi transcript for this person, by modification time (ties broken
/// by the lexicographically smallest path — matching
/// `latestOrganizationPersonSession`'s sort, `org-runtime.ts:338-345`). `None`
/// when there is no transcript to resume.
///
/// # PI DOES NOT WRITE INTO `sessions/`. IT WRITES ONE DIRECTORY DOWN.
///
/// This read was non-recursive and kept only entries whose own extension was
/// `.jsonl`, so it answered `None` for every person, on every pass, for the
/// whole life of the feature — session resume was dead code in production and
/// nothing said so, because `None` is also the correct answer for a first boot.
///
/// Pi keys its transcripts by the working directory they were made in
/// (`@earendil-works/pi-coding-agent`, `core/session-manager.js`):
///
/// ```text
/// const safePath = `--${resolvedCwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`;
/// return join(resolvedAgentDir, "sessions", safePath);
/// ```
///
/// Leading `--`, trailing `--`, and `/`, `\` and `:` folded to `-`. Measured on
/// a live company:
///
/// ```text
/// .chief/agent/dana/sessions/
///   --root-companies-closegaps-labs-.chief-agent-dana--/
///     2026-08-19T05-43-08-380Z_01a0188b-….jsonl
/// ```
///
/// The slug directory's own name has no extension, so the old filter dropped
/// the one entry that mattered and never looked inside it.
///
/// **More than one slug directory per person is ordinary, not an edge case.**
/// The name is derived from a cwd, so a person whose workspace path ever
/// differs — a company moved, a launch from a different directory — has a
/// second directory beside the first, each with its own transcripts. Pi agrees:
/// its own `SessionManager.listAll` reads EVERY subdirectory of `sessions/` and
/// merges them. So does this: the newest transcript ACROSS every slug
/// directory wins, because that is the conversation the person was last having,
/// wherever it happened.
///
/// Files sitting directly in `sessions/` are not candidates at all. Pi has
/// never written one there, so anything at that level was put there by
/// something else — see [`is_pi_transcript`].
///
/// # The epoch
///
/// Unlike the TypeScript original this does not filter by a clean-session
/// epoch (`org-session-epoch.ts`): chiefd's Rust actuator has no epoch
/// equivalent yet (tracked as a follow-up). The TypeScript behavior when no
/// epoch record exists is to resume normally ("fails open" — an absent epoch
/// reads as `0`, so every transcript is strictly newer than it), so always
/// resuming the newest transcript here matches that same default-open case,
/// not a narrower one. A `company ceo` clean-boot's "everyone gets a fresh
/// session" guarantee is not yet wired to this actuator; nothing here makes
/// that worse than it already was.
fn latest_session(
    sessions_dir: &Path,
    session_epoch: Option<SystemTime>,
    force_fresh: bool,
) -> Option<PathBuf> {
    select_launch_session(
        pi_transcript_candidates(sessions_dir),
        session_epoch,
        force_fresh,
        is_pi_transcript,
    )
}

/// Every `.jsonl` transcript Pi may have written under `sessions/`: the files
/// sitting DIRECTLY in it, and the files one level below it in a slug
/// directory. Both, because Pi writes one or the other depending on how the
/// directory was chosen, and chief reads homes of both shapes.
///
/// # TWO LAYOUTS, AND WHY BOTH ARE PI'S
///
/// Pi's DEFAULT session directory is `<agentDir>/sessions/--<cwd-slug>--/`
/// (`session-manager.js` `getDefaultSessionDirPath`), so a transcript sits one
/// level down. An EXPLICIT directory — which is what
/// `PI_CODING_AGENT_SESSION_DIR` supplies — is used AS IT IS:
/// `const dir = sessionDir ? normalizePath(sessionDir) : getDefaultSessionDir(cwd)`.
/// No slug is appended, so the transcript sits directly in it.
///
/// chief now hands every managed person an explicit directory, so their
/// transcripts are top-level; the Chief reads the operator's own sessions
/// directory, which Pi chose by default, so those are slug-nested. Scanning
/// only one of the two would answer `None` for every person in the other
/// shape — and `None` is ALSO the correct first-boot answer, so the failure
/// would be silent and would present as every restart starting a fresh
/// conversation. That is the defect this file already records in
/// `is_pi_transcript`'s history, reached by layout instead of by parsing.
///
/// Two levels and no deeper. A general walk would start selecting whatever an
/// agent happened to leave in a subdirectory of its own. Unreadable
/// directories and entries are skipped rather than aborting the scan — one bad
/// slug directory must not cost a person the transcript in the next one.
fn pi_transcript_candidates(sessions_dir: &Path) -> impl Iterator<Item = (PathBuf, SystemTime)> {
    let entries: Vec<_> =
        fs::read_dir(sessions_dir).into_iter().flatten().filter_map(Result::ok).collect();
    let top_level = entries
        .iter()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    let nested = entries
        .iter()
        .filter(|slug| slug.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|slug| fs::read_dir(slug.path()).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    top_level
        .into_iter()
        .chain(nested)
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((path, modified))
        })
}

/// Whether this file is a transcript Pi wrote, judged by Pi's own rule.
///
/// # THE THIRD DEFECT: AN AGENT COULD WEDGE ITSELF WITH A LOG FILE
///
/// Selection validated nothing about a file's contents. Any agent that wrote a
/// `.jsonl` file into its own sessions directory therefore handed itself to
/// `--session` on its next wake and never came back. That is not a contrived
/// case: it happened to a live person told, through ordinary product messaging,
/// to "keep a small JSON-lines log of your own status". They wrote a valid
/// two-line JSONL file and wedged on the next wake.
///
/// The rule here is Pi's, not one chief invented. `buildSessionInfo`
/// (`core/session-manager.js`) reads the file, skips lines that do not parse as
/// JSON, and **requires the first line that does to be `type === "session"`** —
/// otherwise the file is not a session and Pi returns `null` for it. Reusing
/// that rule means one definition of "is this a transcript", held by the
/// program that writes them.
///
/// The alternative was to restrict selection to the filename shape Pi writes,
/// `<timestamp>_<uuid>.jsonl`. It was rejected: it is chief re-deriving a
/// convention it does not own, and a correctly-named file that is not a
/// transcript would still wedge the person.
///
/// Only the header is read, and the file is streamed rather than slurped — a
/// transcript is routinely megabytes, and the answer is on its first line.
fn is_pi_transcript(path: &Path) -> bool {
    let Ok(mut file) = crate::files::ObservedFile::open(path) else {
        return false;
    };
    // `read_range` treats a short file as an error rather than a truncated
    // observation, so the request is clamped to what is there. A transcript is
    // routinely far larger than the limit; a first-boot file may be smaller.
    let size = file.metadata().size;
    let wanted = size.min(TRANSCRIPT_HEADER_BYTES as u64);
    let Ok(head) = file.read_range(0, usize::try_from(wanted).unwrap_or(0)) else {
        return false;
    };
    let Ok(head) = String::from_utf8(head) else {
        return false;
    };
    // COMPLETE lines only when the read stopped short of the end. The last
    // piece of a bounded read is whatever the limit cut in half, and a
    // truncated line that fails to parse must not be skipped over as if it were
    // a blank — a real Pi header is the first line and a few hundred bytes, so
    // a file whose header does not fit here is not one.
    let complete = if wanted < size {
        head.rsplit_once('\n').map_or("", |(lines, _)| lines)
    } else {
        head.as_str()
    };
    for line in complete.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        return entry.get("type").and_then(serde_json::Value::as_str) == Some("session");
    }
    false
}

/// The transcript a launch in `company_dir` may resume, from a sessions
/// directory that is NOT the launcher's own.
///
/// # THE PRIVACY BOUNDARY, and it is the whole reason this function exists
///
/// The Chief runs in the OPERATOR'S OWN Pi agent directory, not in a
/// per-person home — that is what makes it their own Pi rather than an agent.
/// So `sessions/` there holds the operator's PERSONAL transcripts from every
/// other directory they have ever run Pi in: on a live box a
/// `--root-Developer-chief--` slug sits next to the company's own. Selecting
/// "the newest transcript" across all of them would resume the operator's
/// private session into the company's front door.
///
/// So candidates are filtered by the transcript HEADER's own `cwd` field —
/// verified live as `{"type":"session",…,"cwd":"/root/workspace"}` on the first
/// line of every Pi transcript. **Pi's own rule, not a convention chief
/// re-derived**: the 2026-08-19 decision rejects chief deriving Pi's filename
/// or path conventions, and the slug directory name is exactly such a
/// convention. The header is the program's own statement of where it ran.
///
/// Everything else is shared with [`latest_session`] unchanged — the same
/// candidate walk, the same epoch and force-fresh semantics, the same
/// `is_pi_transcript` validation.
fn latest_session_for_cwd(
    sessions_dir: &Path,
    company_dir: &Path,
    session_epoch: Option<SystemTime>,
    force_fresh: bool,
) -> Option<PathBuf> {
    let company_dir = company_dir.to_path_buf();
    select_launch_session(pi_transcript_candidates(sessions_dir), session_epoch, force_fresh, {
        move |path: &Path| is_pi_transcript(path) && transcript_cwd_is(path, &company_dir)
    })
}

/// Whether a transcript's header says it ran in `company_dir`.
///
/// Reads the same bounded header as [`is_pi_transcript`] and asks Pi's own
/// `cwd` field. A header without one answers `false`: an unstated directory is
/// not evidence of this one, and the cost of guessing wrong is the operator's
/// private session.
fn transcript_cwd_is(path: &Path, company_dir: &Path) -> bool {
    let Some(entry) = transcript_header(path) else { return false };
    entry
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|cwd| Path::new(cwd) == company_dir)
}

/// The first JSON-parsing line of a candidate, bounded exactly as
/// [`is_pi_transcript`] bounds it.
fn transcript_header(path: &Path) -> Option<serde_json::Value> {
    let mut file = crate::files::ObservedFile::open(path).ok()?;
    let size = file.metadata().size;
    let wanted = size.min(TRANSCRIPT_HEADER_BYTES as u64);
    let head = file.read_range(0, usize::try_from(wanted).unwrap_or(0)).ok()?;
    let head = String::from_utf8(head).ok()?;
    let complete = if wanted < size {
        head.rsplit_once('\n').map_or("", |(lines, _)| lines)
    } else {
        head.as_str()
    };
    complete.lines().find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
}

/// The Chief's launch resources: the operator's own Pi, resuming the session it
/// last had IN THIS COMPANY.
///
/// # Why the Chief had none, and why that was an oversight rather than a rule
///
/// `session: None` was hard-coded in `cycle.rs`'s `is_chief` arm from the
/// commit that converted the Chief to the operator's own Pi, with no decision
/// recorded anywhere. The transcripts exist and are simply never handed back,
/// so the Chief starts with a clean context on every boot while everybody else
/// resumes.
///
/// The gate the non-chief path applies — a materialized agent home with a
/// resolvable provider — deliberately does NOT apply here: the Chief has no
/// agent home by design (the theme and accent writers skip it), and its
/// provider is the operator's own.
pub fn chief_launch_resources(
    person: &PersonRecord,
    root_pi_agent_dir: &Path,
    company_dir: &Path,
    session_epoch: Option<SystemTime>,
    force_fresh: bool,
) -> MaterializedResources {
    MaterializedResources {
        tools: person_tool_names(person),
        // SAME `session_epoch` AND `force_fresh` AS EVERYBODY ELSE, and that is
        // load-bearing: a Chief wedged on a poisoned transcript needs the same
        // machine escape every other person has. Without them the only recovery
        // would be a human deleting a file.
        session: latest_session_for_cwd(
            &root_pi_agent_dir.join("sessions"),
            company_dir,
            session_epoch,
            force_fresh,
        ),
    }
}

/// How much of a candidate file is read to answer [`is_pi_transcript`].
///
/// The whole answer is on the first line and a Pi header is a few hundred
/// bytes, while a transcript itself is routinely megabytes — Cobalt's was 4.4
/// MB — and this runs on a pass that repeats once a second. Bounded rather than
/// generous: a file needing more than this to state what it is, is not a
/// transcript.
const TRANSCRIPT_HEADER_BYTES: usize = 8 * 1024;

/// Select the transcript a launch may resume. A forced fresh launch resumes
/// nothing; otherwise a clean-session epoch admits only transcripts strictly
/// newer than the epoch, and the newest admitted file that is actually a Pi
/// transcript wins.
///
/// `is_transcript` is a parameter so the ORDER is testable with synthetic
/// timestamps, without depending on real filesystem mtime granularity — and so
/// the order is visibly independent of the contents check.
///
/// Ordered first and validated second, deliberately. Validating every candidate
/// would read the head of every transcript a person has ever had, on a pass
/// that runs once a second; taking them newest-first and stopping at the first
/// real one reads exactly one file in the ordinary case.
fn select_launch_session(
    candidates: impl Iterator<Item = (PathBuf, SystemTime)>,
    epoch: Option<SystemTime>,
    force_fresh: bool,
    is_transcript: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if force_fresh {
        return None;
    }
    let mut admitted = candidates
        .filter(|(_, modified)| epoch.is_none_or(|boundary| *modified > boundary))
        .collect::<Vec<_>>();
    // Newest first, ties broken by the lexicographically SMALLEST path —
    // matching the TS sort's ascending-path tiebreak.
    admitted.sort_by(|(path_a, modified_a), (path_b, modified_b)| {
        modified_b.cmp(modified_a).then_with(|| path_a.cmp(path_b))
    });
    admitted.into_iter().map(|(path, _)| path).find(|path| is_transcript(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiefd_core::store::organization::EmploymentState;
    use std::collections::BTreeMap;
    use std::time::{Duration, UNIX_EPOCH};

    /// `clippy.toml` bans `std::fs::write` outside `chiefd_host::files` (the
    /// filesystem-effects seam, README §5.6); fixture writes go through the
    /// crate's own atomic-publish primitive instead, exactly as
    /// `files.rs`'s own tests do.
    fn write(path: &Path, contents: &str) {
        crate::files::publish_atomically(path, contents, 0o644).expect("write fixture");
    }

    fn person(kind: PersonKind) -> PersonRecord {
        PersonRecord {
            id: "vera".to_owned(),
            name: "Vera".to_owned(),
            title: "Quant Head".to_owned(),
            mandate: "Own quant.".to_owned(),
            kind,
            department_id: "quant".to_owned(),
            employment_state: EmploymentState::Active,
            activation: "resident".to_owned(),
            tools: vec!["read".to_owned(), "bash".to_owned()],
            prompts: Vec::new(),
            created_at: "2026-07-22T00:00:00.000Z".to_owned(),
            staffing_history: None,
            extra: BTreeMap::new(),
        }
    }

    /// The operator's own Pi agent dir, WITH a provider configuration. This is
    /// the gate's subject now: every person inherits this one directory, so the
    /// question is asked once about it rather than once per home.
    fn operator_dir_signed_in(dir: &Path) -> PathBuf {
        let elsewhere = operator_dir_empty(dir);
        write(&elsewhere.join("auth.json"), "{}");
        elsewhere
    }

    /// The state a fresh box is in: the operator has neither signed Pi in nor
    /// written a provider registry.
    fn operator_dir_empty(dir: &Path) -> PathBuf {
        let elsewhere = dir.join("operator-pi-agent");
        fs::create_dir_all(&elsewhere).expect("the operator's own agent dir");
        elsewhere
    }

    /// The home the launch gate checks, built the way `ensure_agent_home`
    /// builds it: a real folder with a real `sessions/`, and a role skill in
    /// PROJECT scope. It holds nothing of the operator's — that is the point of
    /// the 2026-08-27 ruling, and it is why the gate stopped looking here.
    fn agent_home(dir: &Path, person_id: &str) -> PathBuf {
        let home = dir.join(".chief").join("agent").join(person_id);
        fs::create_dir_all(home.join("sessions")).expect("sessions");
        fs::create_dir_all(home.join(".pi").join("skills")).expect("project skills");
        std::os::unix::fs::symlink("../../../../skills", home.join(".pi/skills/worker"))
            .expect("the role skill install");
        home
    }

    /// A HOME THAT HOLDS NO CREDENTIAL IS LAUNCHABLE, because a home is not
    /// where a credential lives any more.
    ///
    /// The gate used to read `auth.json`/`models.json` symlinks inside each
    /// home. Those links are gone with the `PI_CODING_AGENT_DIR` redirect, so
    /// a gate that still looked here would refuse EVERY person in EVERY
    /// company — which is exactly the shape of failure this test now guards.
    /// The provider question moved to the one directory that answers it for
    /// everybody.
    #[test]
    fn a_home_holding_no_provider_files_is_launchable_when_the_operator_is_signed_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_signed_in(dir.path());
        let home = agent_home(dir.path(), "vera");
        for name in ["auth.json", "settings.json", "models.json"] {
            assert!(
                std::fs::symlink_metadata(home.join(name)).is_err(),
                "{name} must not be in the home at all — the fixture proves the real shape"
            );
        }

        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_some()
        );
        assert_eq!(
            explain_launch_refusal(&home, &operator),
            None,
            "and the explainer must not invent a reason for it either"
        );
    }

    /// THE COMPANY OF PEOPLE WHO COULD NOT THINK.
    ///
    /// A home exists and every provider link in it dangles, because the
    /// operator's own Pi agent directory holds neither file. The old gate asked
    /// only whether the folder was there, so chiefd published a full launch
    /// spec for every one of them, the client started them all, and the only
    /// signal anywhere was Pi's own banner INSIDE each pane — every surface
    /// chief owns reported a healthy company.
    ///
    /// This drives the condition rather than the shape: the fixture is the home
    /// `ensure_agent_home` writes, against an operator directory with nothing
    /// in it, and the subject is the gate the launch catalog actually calls.
    #[test]
    fn a_home_that_reaches_no_provider_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_empty(dir.path());
        let home = agent_home(dir.path(), "vera");
        assert!(home.is_dir(), "the home EXISTS: that is the whole trap");

        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_none(),
            "a person who cannot reach a provider must not be published as launchable"
        );
        let explanation =
            explain_launch_refusal(&home, &operator).expect("a refusal must be explainable");
        assert!(
            explanation.contains("auth.json") && explanation.contains("models.json"),
            "the refusal must NAME both files, or the operator cannot act on it: {explanation}"
        );
        assert!(
            explanation.contains(&operator.display().to_string()),
            "and the OPERATOR directory it looked in, which is the one they can fix: {explanation}"
        );
    }

    /// EITHER file is enough, and the person is launchable the moment one of
    /// them resolves. An operator who writes a `models.json` by hand against an
    /// environment key — the shape of the box this bug was found on — has a
    /// working company and must not be refused for a missing sign-in.
    #[test]
    fn a_home_reaching_only_models_json_is_launchable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_empty(dir.path());
        let home = agent_home(dir.path(), "vera");
        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_none()
        );

        write(&operator.join("models.json"), "{}");

        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_some(),
            "one resolving provider file is the gate; chief does not read what is in it"
        );
        assert_eq!(explain_launch_refusal(&home, &operator), None);
    }

    /// A HOME MID-MATERIALIZATION IS REFUSED FOR THIS PASS AND ADMITTED ON THE
    /// NEXT, which is the same shape as an absent home and not a new hazard.
    ///
    /// `ensure_agent_home` makes the folder before it writes its contents, so a
    /// converge pass can observe a home with nothing in it. That pass refuses
    /// by name and retries; what it must never do is latch.
    #[test]
    fn a_half_built_home_refuses_this_pass_and_admits_the_next() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_empty(dir.path());
        let home = dir.path().join(".chief").join("agent").join("vera");
        fs::create_dir_all(&home).expect("the folder, before its contents");
        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_none()
        );
        assert!(explain_launch_refusal(&home, &operator).is_some());

        write(&operator.join("auth.json"), "{}");
        let home = agent_home(dir.path(), "vera");
        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_some()
        );
        assert_eq!(explain_launch_refusal(&home, &operator), None);
    }

    /// A home that is itself a symlink to a real directory is launchable too.
    /// `ensure_agent_home` makes a real one, so this is not the product shape —
    /// it pins that the check FOLLOWS, which is the half of the old gate that
    /// was wrong and the half a `symlink_metadata` regression would restore.
    #[test]
    fn the_gate_follows_a_symlinked_home_rather_than_refusing_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_signed_in(dir.path());
        let real = dir.path().join("elsewhere");
        fs::create_dir_all(real.join("sessions")).expect("real home");
        let home = dir.path().join(".chief").join("agent").join("vera");
        fs::create_dir_all(home.parent().expect("parent")).expect("parent");
        std::os::unix::fs::symlink(&real, &home).expect("link the home itself");

        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_some()
        );
    }

    /// Deterministic local repro of a production failure: the daemon log read
    /// `planned=2 actuated=0 ... apply aborted: step 0 internal inconsistency:
    /// no launch spec for person 'chief'`. That message is `interpret.rs`'s
    /// `StepError::Internal` firing because `self.launch` (built from this
    /// function's `Some`/`None` results, filtered) has no entry for the CEO,
    /// which happens exactly when this function answers `None` for them. An
    /// absent home is now the ONLY condition that produces it.
    #[test]
    fn a_person_with_no_agent_home_has_no_launch_spec_and_a_named_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_signed_in(dir.path());
        let home = dir.path().join(".chief").join("agent").join("chief");
        let mut ceo = person(PersonKind::Executive);
        ceo.id = "chief".to_owned();

        assert!(
            read_materialized_resources(&ceo, &home, &operator).is_none(),
            "a person with no home must resolve to no launch spec: that is the exact condition \
             behind 'internal inconsistency: no launch spec for person' at apply time"
        );
        let explanation =
            explain_launch_refusal(&home, &operator).expect("a refusal must be explainable");
        assert!(
            explanation.contains("agent home") && explanation.contains("chief"),
            "the refusal must NAME the missing home, not say something is missing: {explanation}"
        );
    }

    /// A home the user deleted by hand refuses, and stops refusing the moment
    /// it is back — the state the launch catalog must survive between a
    /// deletion and the next hire-path pass.
    #[test]
    fn a_deleted_home_refuses_until_it_is_recreated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_signed_in(dir.path());
        let home = agent_home(dir.path(), "vera");
        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_some()
        );

        fs::remove_dir_all(&home).expect("the user deletes it");
        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_none()
        );
        assert!(explain_launch_refusal(&home, &operator).is_some());

        agent_home(dir.path(), "vera");
        assert!(
            read_materialized_resources(&person(PersonKind::Worker), &home, &operator).is_some()
        );
        assert_eq!(explain_launch_refusal(&home, &operator), None);
    }

    #[test]
    fn a_materialized_worker_derives_the_baseline_tool_grant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = operator_dir_signed_in(dir.path());
        let home = agent_home(dir.path(), "vera");
        let resources = read_materialized_resources(&person(PersonKind::Worker), &home, &operator)
            .expect("a home");
        // The unconditional builtin floor first, then the baseline sets — and
        // NO manager, root-executive, or loop tools for an unmonitored worker.
        assert_eq!(
            resources.tools,
            [
                "read",
                "bash",
                "edit",
                "write",
                "grep",
                "find",
                "ls",
                "org_send",
                "org_roster",
                "org_create_reminder",
                "org_list_reminders",
                "org_stop_reminder",
                // The SUBTREE surface. Every leaf may grow a unit beneath the
                // one it sits in and head what it made, so these are granted by
                // kind-independent derivation and gated per call by
                // `departmentIsInScope` — empty for a leaf, so each refuses
                // until that leaf heads a unit of its own.
                "org_launch_department",
                "org_stop_department",
                "org_remove_department",
                "org_launch_contract",
                "org_stop_contract",
                "org_remove_contract",
                "org_add_department",
                "org_pause_department",
                "org_resume_department",
                "org_resume_departments",
                "org_hire",
                "org_bench",
                "org_recall",
                "org_start_person",
                "org_stop_person",
                "org_transfer",
                "org_offboard",
                "org_reparent_department",
                "org_appoint_department_head",
                "org_move_department_members",
                "org_lifecycle_status",
            ]
            .map(str::to_owned)
        );
        // THIS ASSERTION INVERTED, and the inversion is the product change.
        // It was withheld from a worker because its handler refused a
        // non-manager outright, so holding it could never succeed. It is fenced
        // SERVER-SIDE now — the lifecycle board derives its scope from the
        // caller — so a worker holding it is refused by the daemon on the
        // subtree rule, exactly like every other verb in this list.
        // Present-and-scope-refused is the safety model working; absent was the
        // bug. (`org_set_thinking` was one of three, deleted with
        // provider/model management; `org_maintain_session` was another,
        // deleted whole on 2026-08-24.)
        // ONE TOOL, so this is an assertion rather than a loop — clippy is
        // right that a one-element `for` is noise. The list shape is not worth
        // preserving: if a second server-fenced tool ever joins it, the loop
        // comes back with a reason, rather than sitting here empty-handed
        // waiting for one.
        let tool = "org_lifecycle_status";
        assert!(
            resources.tools.iter().any(|granted| granted == tool),
            "a worker must now be granted the server-fenced tool {tool}"
        );
    }

    /// The Pi builtin floor is UNCONDITIONAL — operator decision, 2026-08-10:
    /// "every agent should have those tools. Do not block them."
    ///
    /// This inverts `an_empty_declared_tools_grants_no_builtin_tool_at_all`,
    /// which pinned the behaviour that produced the incident: an omitted
    /// `tools` meant a person who could not open a file. The three cases below
    /// are the three ways the old model could fail a person — declaring
    /// nothing, declaring one thing (which a DEFAULT would not have rescued,
    /// since a default only fires on absence), and being a manager.
    #[test]
    fn every_person_gets_the_whole_builtin_floor_whatever_their_seed_declared() {
        let cases: [(&str, PersonKind, Vec<String>); 4] = [
            ("declares nothing", PersonKind::Worker, Vec::new()),
            ("declares one builtin", PersonKind::Worker, vec!["read".to_owned()]),
            ("a manager", PersonKind::Head, Vec::new()),
            ("the executive", PersonKind::Executive, Vec::new()),
        ];
        for (label, kind, declared) in cases {
            let mut subject = person(kind);
            subject.tools = declared;
            let granted = person_tool_names(&subject);
            for builtin in BUILTIN_TOOLS {
                assert!(
                    granted.iter().any(|tool| tool == builtin),
                    "{label}: missing {builtin} from {granted:?}"
                );
            }
            // Composed once, never duplicated.
            for builtin in BUILTIN_TOOLS {
                assert_eq!(
                    granted.iter().filter(|tool| tool.as_str() == builtin).count(),
                    1,
                    "{label}: {builtin} appears more than once"
                );
            }
        }
    }

    /// The floor is composed, NOT stored. `person.tools` is untouched by the
    /// derivation, which is why people who already exist get their basics back
    /// without any migration writing to their rows.
    #[test]
    fn composing_the_floor_does_not_write_it_into_the_person_record() {
        let mut subject = person(PersonKind::Head);
        subject.tools = Vec::new();
        let granted = person_tool_names(&subject);
        assert!(granted.iter().any(|tool| tool == "bash"));
        assert!(subject.tools.is_empty(), "the record must stay as it was: {:?}", subject.tools);
    }

    /// The role sets stay COMPOSED and must never become declarable.
    ///
    /// The tool this used to name — `org_maintain_session` — was granted to a
    /// worker, so the assertion moved to the one set still gated by kind:
    /// `ROOT_EXECUTIVE_TOOLS`. That tool is now deleted outright, which changes
    /// nothing here: the RULE is unchanged and is the point of the test — a
    /// composed grant must never be earnable by naming it in the person record.
    #[test]
    fn the_role_sets_are_not_part_of_the_floor() {
        let granted = person_tool_names(&person(PersonKind::Worker));
        assert!(
            !granted.iter().any(|tool| tool == "org_escalate_to_operator"),
            "a worker must not receive a root-executive tool: {granted:?}"
        );
    }

    #[test]
    fn every_active_runtime_is_granted_the_self_service_reminder_tools() {
        // #699 closes the third-gate drift: the extension registers these for
        // every active person, then Pi filters that surface through ChiefD's
        // `--tools` list. They are self-service controls, not manager or root
        // privileges, so worker, head, and executive all receive them exactly
        // once while their existing restricted/root-only boundaries remain
        // governed by the separate grants below.
        for kind in [PersonKind::Worker, PersonKind::Head, PersonKind::Executive] {
            let tools = person_tool_names(&person(kind));
            for tool in ["org_create_reminder", "org_list_reminders", "org_stop_reminder"] {
                assert!(tools.iter().any(|granted| granted == tool), "{kind:?} missing {tool}");
                assert_eq!(
                    tools.iter().filter(|granted| granted.as_str() == tool).count(),
                    1,
                    "{kind:?} must receive {tool} exactly once"
                );
            }
        }
    }

    #[test]
    fn a_head_adds_the_manager_tools_but_not_the_root_executive_ones() {
        // The retired channel's executive-only tool is gone, so
        // ROOT_EXECUTIVE_TOOLS now carries the whole head/executive tool
        // distinction this pair polices.
        let tools = person_tool_names(&person(PersonKind::Head));
        assert!(tools.iter().any(|granted| granted == "org_hire"));
        assert!(tools.iter().any(|granted| granted == "org_offboard"));
        assert!(!tools.iter().any(|granted| granted == "org_escalate_to_operator"));
    }

    #[test]
    fn an_executive_adds_manager_and_root_executive_tools() {
        let tools = person_tool_names(&person(PersonKind::Executive));
        assert!(tools.iter().any(|granted| granted == "org_hire"));
        assert!(tools.iter().any(|granted| granted == "org_escalate_to_operator"));
    }

    #[test]
    fn a_manager_gets_the_restructure_tools_ts_grants() {
        // THIRD-gate parity (#30 class): the TS ORGANIZATION_MANAGER_TOOL_NAMES
        // grants these three to every manager, and Pi filters the model toolset
        // to this chiefd `--tools` allowlist — so if this re-derivation omits
        // them (it did: MANAGER_TOOLS had 27 entries against the TS list's 30),
        // a converged executive/head SILENTLY loses them on the live daemon.
        // The two restructure keystones (org_reparent_department,
        // org_appoint_department_head, Operator ruling 2026-07-24) are the most
        // damaging: a manager could not move a department or appoint/replace a
        // head at all.
        for kind in [PersonKind::Executive, PersonKind::Head] {
            let tools = person_tool_names(&person(kind));
            for tool in ["org_reparent_department", "org_appoint_department_head"] {
                assert!(
                    tools.iter().any(|granted| granted == tool),
                    "{kind:?} missing manager tool {tool}"
                );
            }
        }
        // THE ROLE-GATED LIST IS NOW EMPTY, and this is where that finishes.
        //
        // It shrank to one when `org_reparent_department` and
        // `org_appoint_department_head` became SCOPE-gated: every leaf may grow
        // a unit beneath itself and head what it made, so reparenting inside
        // its OWN subtree is legitimate, and their guarantee moved from "absent
        // from the catalog" to "refused by `departmentIsInScope`, which is
        // empty for a leaf" — proved in `org_ops`'s
        // `a_leaf_cannot_create_beneath_a_unit_it_neither_heads_nor_sits_in`.
        //
        // `org_set_thinking` was the last one left. It is DELETED now, with the
        // rest of provider/model management: an agent boots as plain Pi on the
        // operator's own defaults and changes its own reasoning effort through
        // Pi, not through chief. So the role-gated list is empty because there
        // is nothing left in it to gate.
        let worker = person_tool_names(&person(PersonKind::Worker));
        assert!(
            !worker.iter().any(|granted| granted == "org_set_thinking"),
            "org_set_thinking is deleted, not merely ungated"
        );
        // And the two keystones ARE granted to a worker now — stated positively
        // so this reads as the decision it is, not as an assertion someone
        // quietly deleted.
        for tool in ["org_reparent_department", "org_appoint_department_head"] {
            assert!(
                worker.iter().any(|granted| granted == tool),
                "the subtree surface is kind-independent: {tool}"
            );
        }
    }

    #[test]
    fn an_executive_adds_the_root_executive_tools_but_a_head_and_worker_do_not() {
        // The intercom extension registers `org_escalate_to_operator` only for
        // the structural root (the CEO/executive), and Pi filters the model
        // toolset to this `--tools` allowlist — so the chiefd-derived grant must
        // include it for an executive. A department head and a worker are
        // NOT the structural root and must NOT receive it (boundary correct,
        // same as the TS test at org-materialize).
        let executive = person_tool_names(&person(PersonKind::Executive));
        for tool in ROOT_EXECUTIVE_TOOLS {
            assert!(executive.iter().any(|granted| granted == tool), "executive missing {tool}");
        }
        for kind in [PersonKind::Head, PersonKind::Worker] {
            let tools = person_tool_names(&person(kind));
            for tool in ROOT_EXECUTIVE_TOOLS {
                assert!(
                    !tools.iter().any(|granted| granted == tool),
                    "{kind:?} must not be granted {tool}"
                );
            }
        }
    }

    #[test]
    fn a_declared_grant_duplicating_a_baseline_tool_is_granted_once() {
        let mut worker = person(PersonKind::Worker);
        worker.tools.push("org_send".to_owned());
        let tools = person_tool_names(&worker);
        assert_eq!(tools.iter().filter(|granted| *granted == "org_send").count(), 1);
    }

    /// Pi's own cwd-slug rule (`core/session-manager.js`), so a fixture layout
    /// is the layout Pi actually writes rather than one this test invented:
    /// leading `--`, trailing `--`, and `/`, `\` and `:` folded to `-`.
    fn slug(cwd: &str) -> String {
        let body: String = cwd
            .trim_start_matches('/')
            .chars()
            .map(|ch| if matches!(ch, '/' | '\\' | ':') { '-' } else { ch })
            .collect();
        format!("--{body}--")
    }

    /// A file Pi would recognise as one of its own: the first parseable JSON
    /// line is a `session` header. Two lines, because a real transcript has a
    /// header and then entries, and the second line must not be what decides.
    fn write_transcript(path: &Path) {
        write(
            path,
            "{\"type\":\"session\",\"version\":3,\"id\":\"01a0188b\",\"cwd\":\"/w\"}\n\
             {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        );
    }

    /// Push a file's modification time well into the past, so "newest" is a
    /// fact of the fixture rather than of how fast the test ran.
    fn age(path: &Path) {
        // Through `nix` rather than `std::fs::File`: `clippy.toml` keeps file
        // handles inside `chiefd_host::executor` (README §5.6), and that holds
        // in fixtures too — the same call `runtime_lifecycle`'s own fixtures
        // make.
        let stamp = nix::sys::time::TimeVal::new(1_700_000_000, 0);
        nix::sys::stat::utimes(path, &stamp, &stamp).expect("age the file");
    }

    /// Every candidate is a transcript. For the ORDER tests, which are about
    /// modification time and the tie-break and nothing else.
    fn any_file(_: &Path) -> bool {
        true
    }

    #[test]
    fn a_person_with_no_sessions_directory_resumes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(latest_session(&dir.path().join("nope"), None, false), None);
    }

    /// THE DEFECT THAT MADE SESSION RESUME DEAD CODE IN PRODUCTION.
    ///
    /// The read was non-recursive, so it looked for a `.jsonl` extension on the
    /// SLUG DIRECTORY's own name and found none — `latest_session` answered
    /// `None` for every person, on every pass, always. It was invisible because
    /// `None` is also the right answer for a person with no transcript.
    ///
    /// The layout here is the one measured on a live company, built through
    /// Pi's own slug rule. A flat fixture passes straight over this defect,
    /// which is how the original tests passed against code that had never once
    /// worked.
    #[test]
    fn the_transcript_pi_actually_writes_is_one_directory_down_and_is_found_there() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let transcript = sessions
            .join(slug("/root/companies/closegaps-labs/.chief/agent/dana"))
            .join("2026-08-19T05-43-08-380Z_01a0188b-0000-4000-8000-000000000000.jsonl");
        write_transcript(&transcript);
        assert_eq!(latest_session(&sessions, None, false), Some(transcript));
    }

    fn write_transcript_for(path: &Path, cwd: &str) {
        write(
            path,
            &format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"01a0188b\",\"cwd\":\"{cwd}\"}}\n\
                 {{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n"
            ),
        );
    }

    /// **THE PRIVACY BOUNDARY, AND THE DECOY THAT PROVES IT.**
    ///
    /// The Chief runs in the OPERATOR'S OWN Pi agent directory — that is what
    /// makes it their own Pi rather than an agent — so `sessions/` there holds
    /// their personal transcripts from every other directory they have ever run
    /// Pi in. On a live box a `--root-Developer-chief--` slug sits right
    /// next to the company's own.
    ///
    /// So "the newest transcript" is the WRONG rule here, and the decoy below is
    /// the case: a strictly NEWER, perfectly valid transcript whose header says
    /// it ran somewhere else must never be selected. Choosing it would resume
    /// the operator's private session into the company's front door.
    #[test]
    fn a_newer_transcript_from_another_directory_is_never_the_chiefs_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let company = Path::new("/root/workspace");

        let ours = sessions.join("root-workspace").join("2026-08-01T00-00-00-000Z_a.jsonl");
        write_transcript_for(&ours, "/root/workspace");
        age(&ours);
        // NEWER, valid, and the operator's own private work.
        let theirs = sessions.join("root-Developer-chief").join("2026-08-24T00-00-00-000Z_b.jsonl");
        write_transcript_for(&theirs, "/root/Developer/chief");

        assert_eq!(
            latest_session_for_cwd(&sessions, company, None, false),
            Some(ours),
            "the company's own transcript wins even though the operator's is newer"
        );
        // AND THE UNSCOPED RULE WOULD HAVE PICKED THE DECOY — which is what
        // makes this fixture prove the scoping rather than agree with it.
        assert_eq!(latest_session(&sessions, None, false), Some(theirs));
    }

    /// A FOUNDING BOOT HAS NOTHING TO RESUME, and must not reach for somebody
    /// else's transcript to fill the gap.
    #[test]
    fn a_company_with_no_transcript_of_its_own_resumes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let theirs = sessions.join("root-Developer-chief").join("2026-08-24T00-00-00-000Z_b.jsonl");
        write_transcript_for(&theirs, "/root/Developer/chief");

        assert_eq!(
            latest_session_for_cwd(&sessions, Path::new("/root/workspace"), None, false),
            None
        );
    }

    /// THE MACHINE ESCAPE, and it is why `force_fresh` is passed through rather
    /// than defaulted. A Chief wedged on a poisoned transcript would otherwise
    /// need a human to delete a file; every other person has this hatch.
    #[test]
    fn a_forced_fresh_chief_resumes_nothing_however_good_its_transcript_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let ours = sessions.join("root-workspace").join("2026-08-01T00-00-00-000Z_a.jsonl");
        write_transcript_for(&ours, "/root/workspace");

        assert_eq!(
            latest_session_for_cwd(&sessions, Path::new("/root/workspace"), None, true),
            None
        );
    }

    /// A header with no `cwd` is not evidence of THIS directory. Unstated is
    /// refused rather than guessed, because the cost of guessing wrong is the
    /// operator's private session.
    #[test]
    fn a_transcript_whose_header_states_no_directory_is_not_selected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let headless = sessions.join("root-workspace").join("2026-08-01T00-00-00-000Z_a.jsonl");
        write(&headless, "{\"type\":\"session\",\"version\":3,\"id\":\"x\"}\n");

        assert_eq!(
            latest_session_for_cwd(&sessions, Path::new("/root/workspace"), None, false),
            None
        );
        // NON-VACUITY: it IS a valid Pi transcript by the shared rule, so this
        // is the cwd filter refusing it and not the transcript check.
        assert!(is_pi_transcript(&headless));
    }

    /// A person whose workspace path ever differed has more than one slug
    /// directory, each with its own transcripts, and the conversation they were
    /// last having is the one to resume — wherever it happened. Pi's own
    /// `listAll` merges every subdirectory for the same reason.
    #[test]
    fn the_newest_transcript_wins_across_every_slug_directory_a_person_has() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let stale = sessions.join(slug("/old/home/dana")).join("2026-08-01T00-00-00-000Z_aa.jsonl");
        let current =
            sessions.join(slug("/root/companies/labs")).join("2026-08-19T05-43-08-380Z_bb.jsonl");
        write_transcript(&stale);
        write_transcript(&current);
        age(&stale);
        assert_eq!(latest_session(&sessions, None, false), Some(current));
    }

    /// SUPERSEDED AND INVERTED, openly. This asserted that a `.jsonl` sitting
    /// directly in `sessions/` is NOT a transcript, on the stated premise that
    /// "Pi has never written a transcript directly into `sessions/`". That
    /// premise was true while every session directory was chosen by Pi's own
    /// default, which appends a `--<cwd-slug>--` component. It is false now:
    /// chief hands managed people an explicit `PI_CODING_AGENT_SESSION_DIR`,
    /// and an explicit directory is used AS IT IS —
    /// `const dir = sessionDir ? normalizePath(sessionDir) : getDefaultSessionDir(cwd)`
    /// — so their transcripts sit at the top level. Keeping this assertion
    /// would have made `latest_session` answer `None` for every managed person
    /// for ever, which reads as every restart starting a fresh conversation and
    /// reports as nothing at all, because `None` is also the right answer on a
    /// first boot.
    ///
    /// What kept the old rule SAFE is not the layout and never was: a
    /// candidate still has to pass `is_pi_transcript`, so a stray `.jsonl` an
    /// agent wrote is rejected on its CONTENTS. That guard is untouched and is
    /// pinned by the wedge test below.
    #[test]
    fn a_transcript_sitting_directly_in_sessions_is_the_managed_layout_and_resumes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let transcript = sessions.join("2026-08-19T05-43-08-380Z_cc.jsonl");
        write_transcript(&transcript);
        assert_eq!(
            latest_session(&sessions, None, false),
            Some(transcript),
            "an explicit session dir has no slug component, so this IS where Pi writes"
        );
    }

    /// BOTH LAYOUTS AT ONCE, newest wins across them.
    ///
    /// A home that predates the explicit session directory has slug-nested
    /// transcripts; everything written since sits at the top level. A reader
    /// that understood only one of the two would silently drop half a person's
    /// history — and which half depends only on when they were hired.
    #[test]
    fn the_newest_transcript_wins_across_the_slug_and_top_level_layouts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let old_nested = sessions.join(slug("/root/companies/labs")).join("2026-08-01_aa.jsonl");
        let new_top_level = sessions.join("2026-08-19T05-43-08-380Z_bb.jsonl");
        write_transcript(&old_nested);
        write_transcript(&new_top_level);
        // The nested one is the OLD conversation; the top-level one is current.
        age(&old_nested);
        assert_eq!(
            latest_session(&sessions, None, false),
            Some(new_top_level),
            "the managed layout's transcript is seen at all, and wins on mtime"
        );

        // And the other way round, so the test cannot pass on ordering alone:
        // an older home's slug-nested history must stay findable.
        let sessions = dir.path().join("sessions-reversed");
        let nested = sessions.join(slug("/root/companies/labs")).join("2026-08-19_bb.jsonl");
        let top_level = sessions.join("2026-08-01_aa.jsonl");
        write_transcript(&nested);
        write_transcript(&top_level);
        age(&top_level);
        assert_eq!(latest_session(&sessions, None, false), Some(nested));
    }

    #[test]
    fn non_jsonl_entries_beside_a_transcript_are_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        write(&sessions.join(slug("/w")).join("notes.txt"), "irrelevant");
        assert_eq!(latest_session(&sessions, None, false), None);
    }

    /// THE THIRD DEFECT: AN AGENT COULD WEDGE ITSELF WITH A LOG FILE.
    ///
    /// Selection took the newest `.jsonl` by mtime and validated nothing about
    /// it, so an agent told through ordinary product messaging to "keep a small
    /// JSON-lines log of your own status" wrote a valid two-line JSONL file and
    /// never woke again. The log here is newer than the transcript, exactly as
    /// it was live — a freshly written note always is.
    ///
    /// The rule applied is Pi's own (`buildSessionInfo`): the first line that
    /// parses as JSON must be `type: "session"`.
    #[test]
    fn an_agents_own_jsonl_log_is_never_mistaken_for_the_transcript_beside_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slug_dir = dir.path().join("sessions").join(slug("/root/companies/labs"));
        let transcript = slug_dir.join("2026-08-19T05-43-08-380Z_bb.jsonl");
        write_transcript(&transcript);
        write(
            &slug_dir.join("status.jsonl"),
            "{\"at\":\"2026-08-19T06:00:00Z\",\"status\":\"reviewing the brief\"}\n\
             {\"at\":\"2026-08-19T06:05:00Z\",\"status\":\"waiting on dana\"}\n",
        );
        // The log is NEWER than the transcript, exactly as it was live.
        age(&transcript);
        assert_eq!(latest_session(&dir.path().join("sessions"), None, false), Some(transcript));
    }

    /// A correctly-named file that is not a transcript is refused too. This is
    /// why the check is Pi's header rule and not the `<timestamp>_<uuid>.jsonl`
    /// filename shape: the shape is a convention chief does not own.
    #[test]
    fn a_correctly_named_file_that_is_not_a_session_is_still_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        write(
            &sessions
                .join(slug("/w"))
                .join("2026-08-19T05-43-08-380Z_01a0188b-0000-4000-8000-000000000000.jsonl"),
            "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"not a header\"}}\n",
        );
        assert_eq!(latest_session(&sessions, None, false), None);
    }

    /// Ordered first, validated second — so a newer impostor costs one rejected
    /// file and the real transcript behind it is still found, rather than the
    /// person resuming nothing.
    #[test]
    fn selection_walks_newest_first_and_stops_at_the_first_real_transcript() {
        let base = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let real = (PathBuf::from("/sessions/w/real.jsonl"), base);
        let impostor =
            (PathBuf::from("/sessions/w/impostor.jsonl"), base + Duration::from_secs(60));
        assert_eq!(
            select_launch_session(vec![real.clone(), impostor].into_iter(), None, false, |path| {
                path.ends_with("real.jsonl")
            }),
            Some(real.0),
        );
    }

    #[test]
    fn the_newest_transcript_by_modification_time_is_selected() {
        let base = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let older = (PathBuf::from("/sessions/w/older.jsonl"), base);
        let newer = (PathBuf::from("/sessions/w/newer.jsonl"), base + Duration::from_secs(60));
        assert_eq!(
            select_launch_session(vec![older, newer.clone()].into_iter(), None, false, any_file),
            Some(newer.0),
        );
    }

    #[test]
    fn the_clean_session_epoch_excludes_equal_and_older_transcripts() {
        let epoch = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let older = (PathBuf::from("/sessions/w/older.jsonl"), epoch - Duration::from_secs(1));
        let equal = (PathBuf::from("/sessions/w/equal.jsonl"), epoch);
        let newer = (PathBuf::from("/sessions/w/newer.jsonl"), epoch + Duration::from_secs(1));

        assert_eq!(
            select_launch_session(
                vec![older, equal, newer.clone()].into_iter(),
                Some(epoch),
                false,
                any_file,
            ),
            Some(newer.0),
        );
    }

    #[test]
    fn a_forced_fresh_launch_never_resumes_a_transcript() {
        let modified = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let transcript = (PathBuf::from("/sessions/w/latest.jsonl"), modified);

        assert_eq!(select_launch_session(vec![transcript].into_iter(), None, true, any_file), None,);
    }

    #[test]
    fn launch_selection_without_an_epoch_keeps_the_default_open_resume_behavior() {
        let base = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let older = (PathBuf::from("/sessions/w/older.jsonl"), base);
        let newer = (PathBuf::from("/sessions/w/newer.jsonl"), base + Duration::from_secs(1));

        assert_eq!(
            select_launch_session(vec![older, newer.clone()].into_iter(), None, false, any_file),
            Some(newer.0),
        );
    }

    #[test]
    fn a_modification_time_tie_is_broken_by_the_lexicographically_smallest_path() {
        let same = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let z = (PathBuf::from("/sessions/w/z.jsonl"), same);
        let a = (PathBuf::from("/sessions/w/a.jsonl"), same);
        assert_eq!(
            select_launch_session(vec![z, a.clone()].into_iter(), None, false, any_file),
            Some(a.0),
        );
    }
}
