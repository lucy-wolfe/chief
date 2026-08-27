//! The written operating contract every person runs under.
//!
//! Pure, deterministic text generation from the manifest: no I/O, no state, no
//! clock. The contract text it renders was authored in the launcher's
//! TypeScript agent-contracts module, which is deleted. Its output *is* the
//! durable `person-contracts` document (`store::person_contracts`), which the
//! boot path projects to each person's `workspace/AGENTS.md`.
//!
//! # Why this had to leave TypeScript
//!
//! The contract text is a *business decision* — who reports to whom, which
//! verbs a manager may use, what the
//! capability plan permits. It was the last decision of that class still
//! authored outside chiefd, which meant the launcher could rewrite an agent's
//! standing orders without chiefd ever seeing the change. Mandate 3: the text
//! is derived here, stored here, and served from here.
//!
//! # Determinism is load-bearing
//!
//! `store::person_contracts::rows` skips the write when the MD5 of the rendered
//! text is unchanged, and the boot path compares that same MD5 against the
//! on-disk file. Any nondeterminism here (map iteration order, a timestamp, a
//! locale) would make every boot rewrite every `AGENTS.md` and re-stamp every
//! mtime, defeating extension drift detection. Everything below is a pure
//! function of the manifest.

use crate::error::Refusal;
use crate::store::organization::{
    organization_unit_kind, OrganizationManifest, PersonKind, PersonRecord, UnitKind,
};

/// The foreground bash budget, in seconds, quoted in the responsiveness
/// contract.
const FOREGROUND_BASH_TIMEOUT_SECONDS: i64 = 4 * 60;

/// The foreground responsiveness contract.
///
/// Ported from `packages/piing/src/extensionruntime/RuntimePolicy.ts`'s
/// `organizationForegroundResponsivenessContract`, which composed it for the TS
/// contract builder. The contract text is chiefd's to author (Mandate 3), so
/// the section is generated here.
#[must_use]
pub fn foreground_responsiveness_contract() -> String {
    let timeout_minutes = FOREGROUND_BASH_TIMEOUT_SECONDS / 60;
    format!(
        "## Foreground responsiveness\n\n- Keep foreground commands bounded and interactive: managed Bash receives a {timeout_minutes}-minute maximum so queued organization mail can re-enter this Pi session.\n- Never hold a foreground tool open to sleep until a future time, poll indefinitely, tail forever, or host a daemon/server. Arm a durable reminder with `org_create_reminder` for future work; use a truly detached process with redirected stdio and an explicit supervisor only when a persistent process is the actual deliverable."
    )
}

/// Who a person reports to, rendered for the contract header.
///
/// The CEO reports to the human. A head reports to the head of its unit's
/// parent (and to the human when it has no parent). Everyone else reports to
/// the head of the unit they are assigned to.
fn reports_to(manifest: &OrganizationManifest, person: &PersonRecord) -> Result<String, Refusal> {
    if person.kind == PersonKind::Executive {
        return Ok("the human".to_string());
    }
    let department = manifest
        .departments
        .get(&person.department_id)
        .ok_or_else(|| unknown_unit(&person.department_id))?;
    let manager = manifest
        .people
        .get(&department.head_person_id)
        .ok_or_else(|| unknown_person(&department.head_person_id))?;
    if manager.id != person.id {
        return Ok(format!("**{}** (`{}`)", manager.name, manager.id));
    }
    let parent_manager = department
        .parent_department_id
        .as_deref()
        .and_then(|parent| manifest.departments.get(parent))
        .and_then(|parent| manifest.people.get(&parent.head_person_id));
    Ok(match parent_manager {
        Some(parent) => format!("**{}** (`{}`)", parent.name, parent.id),
        None => "the human".to_string(),
    })
}

// TOMBSTONE (chief-home-is-cwd §3/§4e): `resource_plan` — the contract section
// that listed a person's granted skills, extensions and packages, and said
// "Optional resources: none" when they had none. Nobody is granted a resource:
// an agent's skills are the files in `<dir>/.pi/skills`, reached through one
// symlink and loaded by Pi, and the same set reaches everybody. A contract line
// enumerating a per-person subset would describe a decision chief no longer
// makes. #1093 had already deleted the per-resource justification this printed
// beside each id.

/// The manager (executive/head) operating contract.
fn manager_contract(
    manifest: &OrganizationManifest,
    person: &PersonRecord,
) -> Result<String, Refusal> {
    let home = manifest
        .departments
        .get(&person.department_id)
        .ok_or_else(|| unknown_unit(&person.department_id))?;
    let home_kind = organization_unit_kind(manifest, home)?;
    let scope = if person.kind == PersonKind::Executive {
        // #1048: the root department's ID is `executive`; its NAME is the
        // company display name. A CEO with an empty company read the name off
        // this very contract, passed the company as a department id, and got an
        // AUTHORITY refusal for a department that did not exist — then followed
        // its advice into a create the core refuses (`exec-root-protected`).
        // The id, and the fact that hiring into it is normal, belong here: the
        // first hire of every new company depends on both.
        format!(
            "You are the CEO. Set direction for **{}**, allocate work through department heads, and make final organization decisions.\n\nYou head the root department, whose id is `{}`. That id is what `org_hire`, `org_add_department` and every other department tool takes — never the company name **{}**, which is only its display name. Hiring into `{}` is allowed and is the normal way to staff the root: call `org_hire` with `departmentId: \"{}\"`. You do not need a new department first, and you must never name yourself as the existing head of one — you already head the root, and that create is refused.",
            manifest.name,
            manifest.root_department_id,
            manifest.name,
            manifest.root_department_id,
            manifest.root_department_id
        )
    } else if home_kind == UnitKind::Contract {
        format!(
            "You lead the bounded **{}** contract and report to {}. Deliver its engagement, hand off reusable artifacts, and close it cleanly instead of treating it as permanent capacity.",
            home.name,
            reports_to(manifest, person)?
        )
    } else {
        format!("You lead **{}** and report to {}.", home.name, reports_to(manifest, person)?)
    };
    Ok(format!(
        "{scope}\n\n**You are a manager. You do not do the work.** Work that arrives at you is work to ROUTE: break it into bounded pieces, give each piece one owner, wake that owner, and report upward and downward. Your primary job is to delegate, staff, verify, unblock and communicate—never specialist execution. Verifying is reading a result and judging it, not redoing it.\n\n- **`org_send` IS the wake.** A message to somebody who is not running STARTS them: chiefd grants their launch intent and your message is the first thing they read. You never start a person before delegating to them, and \"my reports are asleep\" is never a reason to keep a piece of work. The one exception is a BENCHED person, which `org_send` names in its own answer—`org_recall` them and send again.\n- **When you are tempted to do it yourself, do one of these instead.** No report who can own it: `org_hire` one, or `org_add_department` for the unit that should own it. No facts: delegate a bounded piece of research and wait for it. Too small to delegate: send it anyway. Nobody up: see above. Genuinely nobody and nothing you can create: say so upward and ask. Quietly absorbing the work is the one answer that is always wrong.\n- You hold the ordinary Pi tools that everybody in this company holds, `bash`, `edit` and `write` among them. Read, grep and ls are how you break work down and judge a returned result. Holding the others is not permission to use them on work that belongs to one of your people.\n- Call `org_roster` before staffing work. Reuse an existing department or person before hiring.\n- For `org_hire`, put languages, databases, libraries, and competencies in the mandate. A hire selects no Pi resources: everybody in this company reads the same skills from the company directory.\n- Placement follows the request, never a default. When new work is described as reporting to a named person, THAT person's department is the parent: pass `parentDepartmentId` as the id of the department that person heads. Never park a new department at your own root because the named person is \"only a worker\".\n- When the named person heads nothing yet, promoting them is the FIRST call, not a blocker: `org_add_department` with `existingHeadPersonId: \"<their person id>\"` creates the department and makes them its head in one commit. Then create the new work beneath it. There is no role gate in this product—authority over structure is the subtree you head, never a job title, and a person who heads nothing may still grow a department beneath themselves.\n- Naming an existing person as head MOVES them into the department they now head; nobody heads a unit from outside it. Anyone except the CEO may be appointed, wherever they sit today. If they already head a department, also send `vacates`: hand that department to one of its other members, or dissolve it when it has no other members and no child departments.\n- Orient only long enough to route the work. Do not investigate, implement, or research a specialist problem yourself: delegate a bounded researcher first when facts are missing, then route the resulting work to the right specialist.\n- Hand work to a direct report with `org_send`, naming one owner, the expected output, the evidence requirement and the deadline in the message itself. Use `org_send({{ to: \"all\", ... }})` only for a true shared announcement, never one-by-one fan-out or readiness chatter.\n- Messaging stays inside this organization. For any same- or cross-department message, first use `org_roster` to find the exact person id, then call the Pi `org_send` tool directly. `launcher` is never a person or message recipient. Never invoke the `org` CLI or lifecycle commands from a shell. There is no cross-organization direct-mail route: hand a named person, organization, artifact path, and desired outcome to your manager for explicit human/ChiefD coordination.\n- Break broad work into small, bounded pieces that independent capable specialists can run in parallel. Keep one stronger reviewer or advisor for synthesis, verification, and hard escalation instead of making that person the default executor.\n- Hand work out promptly: the message IS the delivery, and a report reads its mailbox when it wakes. Never hand the same work to two people while one still owns it.\n- Route software diagnosis, implementation, and code review to Engineering. Route deployment, domains, ports, services, health checks, and releases to IT.\n- Bench idle compute without deleting identity or history. Recall when work arrives; transfer somebody when ownership should move.\n- Keep every message strictly operational. Resolve blockers promptly and escalate only when the required decision or authority is outside your scope.\n- Follow through on your own cadence: `org_create_reminder` arms a durable recurring nudge on yourself or on somebody you manage. Ask a report for status only when it has not supplied fresh status since your last check; never broadcast a status sweep or generate healthy-team chatter.\n- English only: write and respond in English, including system-wide notices, status and delegated work."
    ))
}

/// The worker operating contract.
fn worker_contract(
    manifest: &OrganizationManifest,
    person: &PersonRecord,
) -> Result<String, Refusal> {
    let assigned = manifest
        .departments
        .get(&person.department_id)
        .ok_or_else(|| unknown_unit(&person.department_id))?;
    Ok(format!(
        "You are a worker in **{}** and report to {}.\n\n- Own only the assigned output, verify it, and return one concise result, precise blocker, or necessary question to the requester.\n- Use `org_send` for substantive peer results, blockers, or questions. `to: \"all\"` is a real organization broadcast and should be rare.\n- Never run `org` in bash: the Pi `org_send` and `org_roster` tools are the only supported messaging route. For same- or cross-department contact, read the roster and send directly to the exact person id. `launcher` is never a person or message recipient. Cross-organization contact is not supported by this mailbox; give the requester the organization/person/artifact details instead.\n- Work reaches you as a message. Read your mailbox when you wake; there is nothing to acknowledge and no acknowledgement-only chatter to send.\n- When an assigned output is verified, send its result once to the manager who asked for it with `org_send({{ to: \"<manager>\", body: \"...\" }})`. If a correction is needed afterward, send another `org_send` and label it a correction.\n- Surface blockers early. State the exact data, access, tool, dependency, decision, or staffing help you need and how it blocks the next milestone.\n- Stay within your mandate. Do not absorb adjacent work that belongs to a peer or reach sideways or upward in the hierarchy.\n- **You do the work yourself.** Own the assigned output, verify it, and report it. You are not a manager: do not hand your own assigned work to somebody else, and do not hire somebody to do it for you. You may collaborate with peers—read the roster and message anybody in this company directly for a fact, a file, a review or a handoff.\n- **If you are asked to HEAD a department, that is a conversion and it changes what you are.** `org_add_department` with `existingHeadPersonId` set to your own person id makes you its head in one call; there is no role gate and no approval to wait for, and it MOVES you into the department you will head. From that moment you are a manager: your `worker` skill is uninstalled, the `manager` skill is installed in its place, and you delegate work instead of doing it. If somebody else appoints you, the same thing happens and you need do nothing. It swaps back the same way if you are made a member again.\n- You may grow the organization BENEATH you. When your own work needs capacity you do not have, use `org_add_department` (or `org_launch_department`) to create a unit under yourself, become its head, and staff it with `org_hire`; the subtree tools are granted to you and they refuse anything outside your own subtree, so growing downward takes authority over nobody. Call `org_roster` first, reuse an existing person or ask your manager before hiring, and put languages, databases, libraries, and competencies in the hire's mandate.",
        assigned.name,
        reports_to(manifest, person)?
    ))
}

/// The complete per-person operating contract — the exact bytes stored in the
/// `person-contracts` document and projected to `workspace/AGENTS.md`.
///
/// # Errors
/// [`crate::store::organization::MANIFEST_INVALID`] when the person references
/// a unit or head the manifest does not contain. A validated manifest cannot
/// produce that.
pub fn person_agents_guide(
    manifest: &OrganizationManifest,
    person: &PersonRecord,
) -> Result<String, Refusal> {
    let department = manifest
        .departments
        .get(&person.department_id)
        .ok_or_else(|| unknown_unit(&person.department_id))?;
    let role = match person.kind {
        PersonKind::Executive => "Chief",
        PersonKind::Head => {
            if organization_unit_kind(manifest, department)? == UnitKind::Contract {
                "Contract lead"
            } else {
                "Department head"
            }
        }
        PersonKind::Worker => "Worker",
    };
    let contract = if person.kind == PersonKind::Worker {
        worker_contract(manifest, person)?
    } else {
        manager_contract(manifest, person)?
    };
    let foreground = foreground_responsiveness_contract();
    Ok(format!(
        "# {role} — {title}\n\nYou are **{name}** (`{id}`) in **{company}**. This ChiefD-owned contract is stable across restarts and transfers; read it first and do not edit it.\n\n## Mandate\n\n{mandate}\n\nDepartment: **{department}**.\n\n## Operating contract\n\n{contract}\n\n{foreground}\n\n## Deliberate capability plan\n\n- **Your skill IS your role.** `PI_CODING_AGENT_DIR/skills` holds exactly one skill, installed for you by chief: `manager` if you head something, `worker` if you do not. It is a link into the company's own skill library, so there is no private copy and nothing to request — and if your role changes, chief uninstalls that skill and installs the other one. Editing the company skill tree changes it for everybody who has that skill installed, so leave it alone unless changing it for everybody is the work you were given.\n- Use only the tools granted by your capability plan (your launch `--tools` allowlist). If the plan is insufficient, request the precise missing capability instead of bypassing isolation.\n\n- Keep reusable artifacts in the current department's shared directory and name their exact paths in your result, so the next person can find them.\n",
        title = person.title,
        name = person.name,
        id = person.id,
        company = manifest.name,
        mandate = person.mandate,
        department = department.name,
    ))
}

fn unknown_unit(id: &str) -> Refusal {
    Refusal::new(crate::store::organization::MANIFEST_INVALID, format!("Unknown department '{id}'"))
}

fn unknown_person(id: &str) -> Refusal {
    Refusal::new(crate::store::organization::MANIFEST_INVALID, format!("Unknown person '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::northstar_manifest;

    fn test_manifest() -> OrganizationManifest {
        northstar_manifest(1_700_000_000_000)
    }

    #[test]
    fn ceo_guide_reports_to_the_human_and_is_a_manager_contract() {
        let manifest = test_manifest();
        let ceo_id = manifest.chief_person_id().unwrap_or_default().to_string();
        let ceo = manifest.person(&ceo_id).expect("chief");
        let guide = person_agents_guide(&manifest, ceo).expect("guide");
        assert!(guide.starts_with("# Chief — "));
        assert!(guide.contains("You are the CEO."));
        assert!(guide.contains("## Foreground responsiveness"));
    }

    /// #1081. The contract printed the placement pair as TWO facts — "Home:
    /// **X**. Current assignment: **Y**." — which was the most visible surface
    /// the dead dichotomy reached, because this text is the literal
    /// `workspace/AGENTS.md` every agent reads first. With one column the two
    /// halves could only ever print the same unit name twice, which reads to an
    /// agent as a distinction it has to account for and cannot.
    #[test]
    fn the_contract_names_one_department_and_never_a_home_assignment_pair() {
        let manifest = test_manifest();
        for person_id in &manifest.people_order {
            let person = manifest.person(person_id).expect("person");
            let guide = person_agents_guide(&manifest, person).expect("guide");
            let unit = &manifest.departments[&person.department_id].name;
            assert!(
                guide.contains(&format!("Department: **{unit}**.")),
                "{person_id}'s contract must name the one department it has"
            );
            for dead in ["Home: **", "Current assignment: **"] {
                assert!(
                    !guide.contains(dead),
                    "{person_id}'s contract still prints half of the retired pair: {dead}"
                );
            }
        }
    }

    /// #1048. A brand-new company's CEO could not hire at the root: the root
    /// department's id (`executive`) is not its display name (the company
    /// name), and nothing told the CEO so before its first hire. The contract
    /// is where a CEO learns what it may do, so it is where the id belongs.
    #[test]
    fn the_ceo_contract_names_the_root_department_id_and_allows_hiring_into_it() {
        let manifest = test_manifest();
        let ceo_id = manifest.chief_person_id().unwrap_or_default().to_string();
        let ceo = manifest.person(&ceo_id).expect("chief");
        let guide = person_agents_guide(&manifest, ceo).expect("guide");
        assert!(
            guide.contains(&format!("whose id is `{}`", manifest.root_department_id)),
            "{guide}"
        );
        assert!(
            guide.contains(&format!("departmentId: \"{}\"", manifest.root_department_id)),
            "{guide}"
        );
        assert!(guide.contains("Hiring into `executive` is allowed"), "{guide}");
        // The dead-end remediation the incident followed must never be advised
        // to the CEO: `org_projection::check_department_create` answers
        // `exec-root-protected` for `is_ceo`, and ONLY for `is_ceo`. So the ban
        // is on the CEO naming ITSELF, not on the argument — teaching the CEO
        // to appoint SOMEBODY ELSE is the promotion path the product supports.
        assert!(guide.contains("never name yourself as the existing head"), "{guide}");
        assert!(!guide.contains(&format!("existingHeadPersonId: \"{ceo_id}\"")), "{guide}");
    }

    /// The live failure of 2026-08-13: "I want the head of engineering to
    /// report to Carlos", Carlos was a worker, and the agent put engineering in
    /// the executive branch because its contract taught hiring and said nothing
    /// about placement. Every manager contract now names the three facts that
    /// decision needs, and each one is verified against
    /// `org_projection::check_department_create`: the named person's department
    /// is the parent; a worker is promoted by appointing them (there is no role
    /// gate — `person_may_create_under_department` lets a leaf grow beneath
    /// itself); and an appointment MOVES the appointee, so a sitting head also
    /// needs `vacates`.
    #[test]
    fn every_manager_contract_teaches_that_reports_to_names_the_parent() {
        let manifest = test_manifest();
        let managers: Vec<&PersonRecord> = manifest
            .people_order
            .iter()
            .filter_map(|id| manifest.person(id))
            .filter(|person| person.kind != PersonKind::Worker)
            .collect();
        assert!(managers.len() > 1, "the fixture must hold a CEO and at least one head");
        for person in managers {
            let guide = person_agents_guide(&manifest, person).expect("guide");
            for phrase in [
                "Placement follows the request, never a default.",
                "THAT person's department is the parent",
                "parentDepartmentId",
                "because the named person is \"only a worker\"",
                "promoting them is the FIRST call, not a blocker",
                "existingHeadPersonId",
                "There is no role gate in this product",
                "authority over structure is the subtree you head, never a job title",
                "MOVES them into the department they now head",
                "nobody heads a unit from outside it",
                "Anyone except the CEO may be appointed",
                "`vacates`",
            ] {
                assert!(guide.contains(phrase), "{}'s contract must say: {phrase}", person.id);
            }
        }
    }

    /// The worker contract deliberately does NOT carry the placement lesson.
    /// `person_may_create_under_department` lets a worker create only beneath
    /// its own department, so "make the new unit report to the person who was
    /// named" is not a move a worker can make; teaching it would teach a
    /// refusal.
    ///
    /// The blanket ban on the WORD `existingHeadPersonId` that used to stand
    /// here was too wide, and its own docstring said so: `check_department_create`
    /// lets a worker appoint ITSELF, which is precisely the conversion a worker
    /// must understand before somebody says "you head Platform now". So the
    /// rule is the precise one — the worker contract teaches self-appointment
    /// and never the appointment of somebody else.
    #[test]
    fn the_worker_contract_teaches_growth_beneath_itself_and_not_placement() {
        let manifest = test_manifest();
        let worker = manifest
            .people
            .values()
            .find(|person| person.kind == PersonKind::Worker)
            .expect("a worker");
        let guide = person_agents_guide(&manifest, worker).expect("guide");
        assert!(guide.contains("You may grow the organization BENEATH you."), "{guide}");
        assert!(!guide.contains("Placement follows the request"), "{guide}");
        assert!(
            guide.contains("`existingHeadPersonId` set to your own person id"),
            "the conversion is taught as a move on ONESELF: {guide}"
        );
        assert!(
            !guide.contains("existingHeadPersonId: \"<their person id>\""),
            "appointing SOMEBODY ELSE is a manager's move: {guide}"
        );
    }

    /// THE DUTY, PINNED. This is the regression the operator reported over and
    /// over: "you send an issue to a department and then the manager is doing
    /// all the work and not even waking up his subordinates."
    ///
    /// Nothing in this repo asserted it. The manager contract said "Your
    /// primary job is to delegate ... not specialist execution", the skill said
    /// something similar, and a grep for either string across every test file
    /// returned only the production sources — so both could erode, and did.
    /// The one test with "Manager" in its name
    /// (`ManagerToolGate3Parity.test.ts`) compares two EMPTY lists and passes
    /// whether or not a manager ever delegates.
    ///
    /// Each clause below is a separate way the failure happens, so each is
    /// asserted separately rather than as one sentence that could be reworded
    /// into meaninglessness.
    #[test]
    fn every_manager_contract_forbids_doing_the_work_and_names_the_wake() {
        let manifest = test_manifest();
        let managers: Vec<&PersonRecord> = manifest
            .people_order
            .iter()
            .filter_map(|id| manifest.person(id))
            .filter(|person| person.kind != PersonKind::Worker)
            .collect();
        assert!(managers.len() > 1, "the fixture must hold a CEO and at least one head");
        for person in managers {
            let guide = person_agents_guide(&manifest, person).expect("guide");
            for phrase in [
                // The explicit negative, stated flatly rather than hedged.
                "**You are a manager. You do not do the work.**",
                "Work that arrives at you is work to ROUTE",
                "never specialist execution",
                // Verifying must not become redoing.
                "not redoing it",
                // The wake — the affordance whose absence made "I will just do
                // it myself" feel forced.
                "**`org_send` IS the wake.**",
                "STARTS them",
                "never a reason to keep a piece of work",
                "`org_recall` them and send again",
                // The alternatives, so the negative is never a dead end.
                "When you are tempted to do it yourself, do one of these instead",
                "`org_hire` one",
                "say so upward and ask",
                "Quietly absorbing the work is the one answer that is always wrong",
                // Holding the tools is not permission to use them.
                "not permission to use them on work that belongs to one of your people",
            ] {
                assert!(guide.contains(phrase), "{}'s contract must say: {phrase}", person.id);
            }
            // The escape hatch that made the old rule unenforceable. A manager
            // with no woken reports reads this as satisfied every time.
            assert!(
                !guide.contains("only when no responsible specialist can own it"),
                "{}'s contract must not carry the do-it-yourself escape hatch",
                person.id
            );
        }
    }

    /// The worker contract carries the CONVERSION, because a worker who is told
    /// "you head Platform now" must not be confused about what just happened to
    /// its role. Operator's words: "it knows how to convert to the department
    /// and then it gets the management skill".
    #[test]
    fn the_worker_contract_says_it_does_the_work_and_how_a_conversion_changes_that() {
        let manifest = test_manifest();
        let worker = manifest
            .people
            .values()
            .find(|person| person.kind == PersonKind::Worker)
            .expect("a worker");
        let guide = person_agents_guide(&manifest, worker).expect("guide");
        for phrase in [
            "**You do the work yourself.**",
            "You are not a manager",
            "do not hand your own assigned work to somebody else",
            "You may collaborate with peers",
            // The conversion, named as a mechanism rather than gestured at.
            "that is a conversion and it changes what you are",
            "`existingHeadPersonId` set to your own person id",
            "there is no role gate and no approval to wait for",
            "it MOVES you into the department you will head",
            "your `worker` skill is uninstalled, the `manager` skill is installed in its place",
            "you delegate work instead of doing it",
            "It swaps back the same way",
        ] {
            assert!(guide.contains(phrase), "the worker contract must say: {phrase}");
        }
        // A worker must never be told to delegate its own work.
        assert!(!guide.contains("You do not do the work"), "{guide}");
    }

    /// The two contracts must not converge. A manager reading the worker's duty
    /// or a worker reading the manager's is the whole defect, and a future
    /// edit that "unifies the copy" would reintroduce it silently.
    #[test]
    fn the_manager_and_worker_contracts_state_opposite_duties() {
        let manifest = test_manifest();
        let worker = manifest
            .people
            .values()
            .find(|person| person.kind == PersonKind::Worker)
            .expect("a worker");
        let head = manifest
            .people
            .values()
            .find(|person| person.kind == PersonKind::Head)
            .expect("a head");
        let worker_guide = person_agents_guide(&manifest, worker).expect("worker guide");
        let head_guide = person_agents_guide(&manifest, head).expect("head guide");

        assert!(head_guide.contains("You do not do the work"));
        assert!(worker_guide.contains("You do the work yourself"));
        assert!(!worker_guide.contains("Work that arrives at you is work to ROUTE"));
        assert!(!head_guide.contains("**You do the work yourself.**"));
    }

    #[test]
    fn a_render_is_byte_identical_across_calls() {
        let manifest = test_manifest();
        let ceo_id = manifest.chief_person_id().unwrap_or_default().to_string();
        let ceo = manifest.person(&ceo_id).expect("chief");
        let first = person_agents_guide(&manifest, ceo).expect("guide");
        let second = person_agents_guide(&manifest, ceo).expect("guide");
        assert_eq!(first, second);
    }

    /// The subtree tools are granted to EVERY person, whatever their kind
    /// (`converge_apply::resource_catalog::SUBTREE_TOOLS`). The worker contract
    /// used to read "Do not ... manage departments, or create staff", so a
    /// worker holding `org_hire` read its own `AGENTS.md` and refused the very
    /// growth the authority layer allows. Text and grant must agree.
    #[test]
    fn a_worker_may_grow_a_department_beneath_itself() {
        let manifest = test_manifest();
        let worker = manifest
            .people
            .values()
            .find(|person| person.kind == PersonKind::Worker)
            .expect("a worker");
        let guide = person_agents_guide(&manifest, worker).expect("guide");
        assert!(guide.starts_with("# Worker — "));
        assert!(guide.contains("org_add_department"), "{guide}");
        assert!(guide.contains("org_hire"), "{guide}");
        assert!(!guide.contains("or create staff"), "{guide}");
        assert!(!guide.contains("manage departments"), "{guide}");
    }

    /// THE CONTRACT SAYS WHAT A PERSON'S SKILL IS, AND IT IS THEIR ROLE.
    ///
    /// This test has now been re-anchored twice, and the direction matters.
    /// It began as `a_person_with_no_optional_resources_says_so`, pinning a
    /// per-person resource catalog. §4e deleted that, so it became "there is NO
    /// per-person selection" — true then, and false now: a person's home holds
    /// exactly one skill, chosen by their kind. A contract still saying "there
    /// is no per-person selection to request" would be telling every manager
    /// that the thing which makes it a manager does not exist.
    #[test]
    fn the_contract_says_the_installed_skill_is_the_role() {
        let manifest = test_manifest();
        for person_id in &manifest.people_order {
            let person = manifest.person(person_id).expect("person");
            let guide = person_agents_guide(&manifest, person).expect("guide");
            assert!(guide.contains("**Your skill IS your role.**"), "{person_id}: {guide}");
            assert!(guide.contains("`manager` if you head something"), "{person_id}: {guide}");
            assert!(
                guide.contains("chief uninstalls that skill and installs the other one"),
                "{person_id}: {guide}"
            );
            // The retired claims, each of which is now false.
            for dead in [
                "Your skills are the company's skills.",
                "there is no per-person selection to request",
                "Optional resources",
                "installed resource catalog",
            ] {
                assert!(!guide.contains(dead), "{person_id}'s contract still says: {dead}");
            }
        }
    }
}
