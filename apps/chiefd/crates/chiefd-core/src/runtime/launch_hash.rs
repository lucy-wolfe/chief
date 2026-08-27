//! The derived launch hash: what a person's process was BUILT FROM.
//!
//! # What the hash IS
//!
//! It is DERIVED from a person's launch inputs. Two people whose inputs are
//! byte-identical produce the same hash by construction, and any change to an
//! input changes it whether or not the author of that change knew this fence
//! existed. Nothing has to be remembered or advanced by hand.
//!
//! The actuator tags each pane with the hash it was started at. Its diff is
//! then "a pane exists for this person AND its tag equals the desired hash" —
//! so a stale process is REPLACED rather than adopted, and chiefd never has to
//! be told what is running.
//!
//! # What goes in, and the two failure modes it is aimed between
//!
//! The input is **what the process was built from**, which is deliberately
//! neither the `people` row nor everything in sight.
//!
//! **Too narrow** is the incident that is already on the record. A launcher
//! deploy rewrites extension code on disk with no row change at all; a hash
//! over the person's row misses it completely, and `runtime_lifecycle.rs`
//! records what that costs — "a whole fleet came up running old code and
//! reported success". So [`LaunchInputs::extension_digest`] is an input, fed
//! from the same materialization-checkpoint machinery `source_extension_drift`
//! already uses. That scan asked "who is running stale code?", which is a
//! question about the host; folding its digest in here answers the same
//! concern without anybody reporting anything upward.
//!
//! **Too wide** has TWO instances, not one.
//!
//! The first is PLACEMENT — see `the_launch_inputs_carry_no_placement_field`.
//! `department_id` and `is_head_of` were inputs here and should not have been:
//! this hash answers "must the PROCESS be replaced", and moving somebody
//! between departments does not change their process. With placement in the
//! hash every transfer became a move followed by a kill.
//!
//! The second is the live-apply trio. `model`,
//! `provider` and `thinking` are EXCLUDED, and this is not an oversight to be
//! tidied up later. Pi applies all three LIVE, in-session. The system has
//! already ruled on it — `organization-intercom.ts`: "`set_model` /
//! `set_thinking` are LIVE-APPLY actions. They never touch the transcript and
//! never replace the session."
//! Including them would mean a user switching their own model restarts their
//! own session, mid-turn, as a direct consequence of asking for a different
//! model. [`LaunchInputs`] has no field for any of them, so including one is a
//! compile error rather than a judgement call.

use crate::hexdigest::hex_digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Everything a person's process is built from.
///
/// Constructed field-by-field on purpose: there is no `From<RosterPerson>`, no
/// `..Default::default()`, and no catch-all map. Adding a launch input should
/// force a visit to this struct, and adding a NON-launch input should be
/// impossible without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInputs<'a> {
    /// The company slug.
    pub organization: &'a str,
    /// Stable person id. Identity, so a rename of the person is not a restart
    /// but a re-identification would be.
    pub person_id: &'a str,
    /// The MODEL-FREE fingerprint of how this person is launched.
    ///
    /// NOT the executed argv, and the distinction is the whole of ruling 5. The
    /// real argv carries `--provider`, the model and the thinking level, so
    /// hashing it would restart a person for changing their own model -- the
    /// exact "too wide" failure this struct is shaped to prevent, sneaking back
    /// in through a field that sounds innocent.
    ///
    /// Built only by [`launch_command_fingerprint`], so the exclusion lives
    /// where the string is produced rather than being re-argued at every call
    /// site.
    pub launch_command: &'a str,
    /// A digest of the extension source this person will load.
    ///
    /// THE FIX FOR THE SILENT-STALE-FLEET INCIDENT. Supplied by the caller
    /// rather than computed here: `chiefd-core` has no view of the launcher
    /// checkout, and the crate that does (`chiefd-host`, via
    /// `materialize::extensions`) already computes exactly this digest for the
    /// drift scan. One computation, two readers -- never a second opinion
    /// about which code is on disk.
    pub extension_digest: &'a str,
}

/// The model-free launch fingerprint for one person.
///
/// The ONE producer of [`LaunchInputs::launch_command`]. It names what the
/// process is structurally launched as -- binary, home, workspace, granted
/// tools -- and deliberately omits `model`, `provider` and `thinking`, which Pi
/// applies live in-session and which must never move the hash.
///
/// A person's granted TOOLS are included: changing what an agent is allowed to
/// do is a change to the process, not a live-apply setting, and a pane running
/// with a stale capability set is exactly the kind of stale this fence exists
/// to replace. Tools are passed to Pi once, as `--tools`, and are not re-read.
///
/// It takes the MANIFEST's person record rather than the roster's, and that is
/// the correction that made this function match its own doc. The roster shape
/// carries no tools, no binary and no paths, so an earlier version hashed
/// `person={id};title={title}` and nothing else -- while this comment claimed
/// otherwise. Changing a person's granted tools did not move the hash, so a
/// pane kept running with the capability set it launched with, for ever. The
/// doc was right and the code was narrow; the type is now the one that can
/// deliver it.
///
/// `pi_binary` is daemon-wide config rather than a person field, so it is
/// passed in: repinning the binary is a change to every person's process and
/// must replace every pane, which is the one case where a fleet-wide restart is
/// the correct outcome.
///
/// What is deliberately NOT here, and why it needs no field: `pi_home` and
/// `workspace` are derived from the data root and the person id, and both of
/// those already reach the hash (`organization`, `person_id`). Adding a path
/// built from values already hashed would add no information and one more way
/// for two daemons to disagree about a string.
#[must_use]
pub fn launch_command_fingerprint(
    person: &crate::store::organization::PersonRecord,
    pi_binary: &str,
) -> String {
    // Every field here is a reason somebody's session gets restarted, so the
    // bar is "would I want this person's process replaced when this changes?".
    // Tools are sorted so a reordered grant is not a restart -- the SET is the
    // capability, the order is not.
    let mut tools: Vec<&str> = person.tools.iter().map(String::as_str).collect();
    tools.sort_unstable();
    format!("person={};title={};tools={};pi={pi_binary}", person.id, person.title, tools.join(","))
}

/// The hash a pane is tagged with, and the value the actuator diffs against.
///
/// Rendered as lowercase hex so it survives a tmux option round-trip
/// unmodified: the tag is written with `set-option` and read back with
/// `show-options`, and anything needing quoting or escaping there is a bug
/// waiting for a person id with a space in it.
#[must_use]
pub fn desired_launch_hash(inputs: &LaunchInputs<'_>) -> String {
    // Length-prefixed field framing, NOT a separator character. A separator is
    // only unambiguous until some field is allowed to contain it, and person
    // ids and department ids are both operator-influenced. With lengths, two
    // different input tuples cannot collide into one byte string -- which is
    // the entire property a fence like this rests on.
    let mut hasher = Sha256::new();
    let mut field = |value: &str| {
        hasher.update(value.len().to_le_bytes());
        hasher.update(value.as_bytes());
    };
    field(inputs.organization);
    field(inputs.person_id);
    field(inputs.launch_command);
    field(inputs.extension_digest);
    hex_digest(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal person record for these tests.
    ///
    /// Spelled out rather than reached for from `test_support`: the fingerprint
    /// must be exercised against the fields it actually reads, and a template
    /// that changes for an unrelated reason would move these hashes.
    fn person_record(
        id: &str,
        title: &str,
        tools: &[&str],
    ) -> crate::store::organization::PersonRecord {
        use crate::store::organization::{EmploymentState, PersonKind, PersonRecord};
        PersonRecord {
            id: id.to_owned(),
            name: id.to_owned(),
            title: title.to_owned(),
            mandate: String::new(),
            kind: PersonKind::Worker,
            department_id: "quant".to_owned(),
            employment_state: EmploymentState::Active,
            activation: "on-demand".to_owned(),
            tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            prompts: Vec::new(),
            created_at: "2026-08-07T00:00:00.000Z".to_owned(),
            staffing_history: None,
            extra: std::collections::BTreeMap::new(),
        }
    }

    fn inputs() -> LaunchInputs<'static> {
        LaunchInputs {
            organization: "northstar",
            person_id: "signal-researcher",
            launch_command: "person=signal-researcher;title=Engineer",
            extension_digest: "abc123",
        }
    }

    #[test]
    fn the_same_inputs_hash_the_same_so_a_steady_company_never_churns() {
        assert_eq!(desired_launch_hash(&inputs()), desired_launch_hash(&inputs()));
    }

    /// THE "TOO NARROW" FAILURE: a launcher deploy rewrites extension code on
    /// disk and changes no person row at all. A row-only hash misses it and the
    /// fleet runs stale code while reporting success.
    #[test]
    fn a_new_extension_digest_changes_the_hash_so_a_deploy_replaces_stale_panes() {
        let before = desired_launch_hash(&inputs());
        let after = desired_launch_hash(&LaunchInputs { extension_digest: "def456", ..inputs() });
        assert_ne!(before, after, "an extension deploy must move the hash");
    }

    /// THE OTHER "TOO WIDE" FAILURE, also barred by construction.
    ///
    /// This REPLACES `placement_is_a_launch_input_because_the_pane_lives_in_a_department_window`,
    /// which asserted the opposite, and the reversal is deliberate. Placement's
    /// old argument was true -- a pane IS created inside the window its
    /// department owns -- and led to the wrong conclusion, because this hash
    /// answers one question only: must the PROCESS be replaced? Moving somebody
    /// between departments does not change their process, and `break-pane`
    /// relocates a live one without touching it.
    ///
    /// With placement in the hash every transfer became a move followed by a
    /// kill: the seamless relocation the design record requires
    /// ("a seamless move when possible, kill-and-relaunch when not") turned into
    /// a lost turn, for a change about which tab somebody appears in.
    ///
    /// Placement is still DIFFED, just not here -- the actuator compares each
    /// pane's window against the desired topology every pass and emits
    /// `MovePane`/`CreateWindowByMove`. That needs no hash, because the actuator
    /// can see both sides.
    #[test]
    fn the_launch_inputs_carry_no_placement_field() {
        let serialized = serde_json::to_string(&inputs()).expect("serializes");
        for placement in ["department", "isHeadOf", "headOf"] {
            assert!(
                !serialized.contains(placement),
                "`{placement}` is PROJECTION: a pane moves between windows without the process \
                 inside it being replaced, and a transfer must not cost a turn: {serialized}"
            );
        }
    }

    /// THE NARROWNESS THIS FUNCTION'S OWN DOC PROMISED AGAINST.
    ///
    /// An earlier version hashed `person={id};title={title}` and nothing else,
    /// while its comment claimed it named the binary and the granted tools and
    /// argued tools MUST be in because a stale capability set is what this
    /// fence replaces. It did not, so changing a person's tools left their pane
    /// running the capability set it launched with, for ever. Both halves are
    /// asserted here so the doc and the code cannot drift apart again.
    #[test]
    fn changing_granted_tools_or_the_pinned_binary_moves_the_fingerprint() {
        let base = person_record("val", "Engineer", &["read"]);
        let granted = person_record("val", "Engineer", &["read", "bash"]);

        assert_ne!(
            launch_command_fingerprint(&base, "/opt/pi/bin/pi"),
            launch_command_fingerprint(&granted, "/opt/pi/bin/pi"),
            "a new tool grant is a new process: --tools is passed once and never re-read"
        );
        assert_ne!(
            launch_command_fingerprint(&base, "/opt/pi/bin/pi"),
            launch_command_fingerprint(&base, "/opt/pi-next/bin/pi"),
            "repinning the binary must replace every pane -- the one case where a fleet-wide \
             restart is the correct outcome"
        );
    }

    /// The SET is the capability; the order is not. A reordered grant list must
    /// not restart anybody, or an unrelated edit to a JSON array becomes a
    /// company-wide churn.
    #[test]
    fn reordering_the_same_tools_is_not_a_restart() {
        let one = person_record("val", "Engineer", &["read", "bash"]);
        let other = person_record("val", "Engineer", &["bash", "read"]);
        assert_eq!(
            launch_command_fingerprint(&one, "/opt/pi/bin/pi"),
            launch_command_fingerprint(&other, "/opt/pi/bin/pi")
        );
    }

    #[test]
    fn a_different_launch_command_is_a_different_process() {
        assert_ne!(
            desired_launch_hash(&inputs()),
            desired_launch_hash(&LaunchInputs {
                launch_command: "person=signal-researcher;title=Staff Engineer",
                ..inputs()
            })
        );
    }

    /// Field framing, not a separator. Two people whose ids and departments
    /// split differently across the same concatenated text must not collide.
    #[test]
    fn field_boundaries_cannot_be_forged_by_moving_a_character_between_fields() {
        let left = desired_launch_hash(&LaunchInputs {
            organization: "north",
            person_id: "star",
            ..inputs()
        });
        let right = desired_launch_hash(&LaunchInputs {
            organization: "northstar",
            person_id: "",
            ..inputs()
        });
        assert_ne!(left, right, "length-prefixed framing must keep these apart");
    }
}
