//! Placement: session, windows, panes — computed from the roster facts alone.
//!
//! # The line this module sits on
//!
//! **chiefd decides WHO runs. The client decides WHERE it is displayed.** Every
//! rule here is on the second side of that line, and none of them is a second
//! reading of the first: [`desired_topology`] never re-derives who should be
//! up, it consumes chiefd's published DESIRED SET.
//!
//! That set — `POST /v1/org/runtime/desired` — is the sole authority on
//! membership here. [`crate::roster::RosterPerson::desired_active`] is
//! deliberately NOT consulted: the roster is read for STRUCTURE (departments,
//! their order, the person display order) and the desired set is read for
//! MEMBERSHIP. Two answers
//! to "who should be up" from two routes read at two different instants is a
//! disagreement waiting to happen, and the one that would win by accident is
//! whichever call this module happened to make second.
//!
//! # The rules, and why each one is here rather than in chiefd
//!
//! * **session = company.** `org-<slug>_` is a tmux session name; a browser has
//!   no session at all. The trailing `_` is a terminator, not decoration: tmux
//!   resolves a target by PREFIX when nothing matches exactly, and `_` is a
//!   character the slug validator refuses, so no company's session name can be
//!   a prefix of another's. See [`session_name_for_slug`].
//! * **window = person, and a window holds exactly one pane.** This is the
//!   rule the whole module now turns on, and it replaced *window =
//!   department*. A department window tiled its people into a grid, so a
//!   person's pane was as wide as their department was crowded — and clicking
//!   them lifted that pane into a full-width body, which is a RESIZE. tmux
//!   truncates or pads the alternate screen at the new width and the Pi inside
//!   repaints its whole scrollback, which is what the operator reported as
//!   *"why is it going half screen and growing?"*. A pane has exactly one
//!   size, so the only fix is that it never has a second one: every desired
//!   person is born in a window of their own, at the size every other window
//!   has, and a click is navigation.
//! * **a department gets NO window at all.** Not "no window when empty" — no
//!   window, ever. Nothing places people by department any more, so the only
//!   department-shaped things on a tmux server are the rail's own card windows
//!   ([`overview_window_id`]), which placement has never heard of and converge
//!   is told to leave alone.
//! * **window name = the person's display name**, through
//!   [`safe_window_name`], so the operator reads "Lena Ortiz" in the window
//!   list. It used to be the department's name, which is why the root window
//!   read as the company.
//! * **window order = the company's canonical person order**, ascending
//!   `displayOrder`. That is the same ordering that used to sequence panes
//!   INSIDE a department window, lifted one level: the operator's chosen order
//!   is now the order of the window list itself.
//!
//! # Ported from
//!
//! `chiefd-core/src/runtime/reconcile_plan.rs`'s `desired_topology` and
//! `pane_department_id`, and `chiefd-host/src/tmux/mod.rs`'s
//! `safe_window_name`. Ported means REWRITTEN AGAINST THE WIRE: this crate
//! links none of those. `apps/chiefd/tests/fixtures/placement-golden.json` is
//! the shared golden both sides are asserted byte-identical against while both
//! still compute it.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Serialize;

use crate::roster::{Roster, RosterError, RosterPerson};

/// The longest tmux window name this client will emit.
///
/// A shared bounded canonical-label contract, not a claim about what today's
/// tmux rejects. The historical incident: a company named `Leo Capital Inc.`
/// aborted its own start with `invalid window name: Leo Capital Inc.` — the
/// slug (`org-leo-capital-inc`) was fine, because only the WINDOW name carries
/// the raw display text.
pub const MAX_WINDOW_NAME_CHARS: usize = 40;

/// One desired pane: which person, what they must be running, and their
/// ordinal in the company's canonical person order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pane {
    /// The person who owns this pane.
    pub person_id: String,
    /// The derived hash of what this person's process must be built from.
    ///
    /// From chiefd's desired set, never computed here — one of its inputs is
    /// the extension source digest, which only the daemon can see. This is the
    /// value the pane is tagged with and the value every later pass diffs on.
    pub launch_hash: String,
    /// Global person-order index, for stable diagnostics and ordering.
    pub order: usize,
}

/// One desired window: exactly one desired person, alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// The logical window id — the `@organization_window_id` tag value.
    /// [`person_window_id`] of the one person this window shows.
    pub logical_id: String,
    /// The person's display name, RAW. This is the fact chiefd published;
    /// [`Window::window_name`] is what tmux is told.
    pub name: String,
    /// The one pane. A window holds exactly one person; the vector is kept
    /// because every consumer walks it and a one-element walk is the same walk.
    pub panes: Vec<Pane>,
}

impl Window {
    /// The tmux window name: [`Window::name`] through [`safe_window_name`].
    ///
    /// Kept as a derivation rather than a stored second field so a window can
    /// never carry a name and a sanitized name that disagree.
    #[must_use]
    pub fn window_name(&self) -> String {
        safe_window_name(&self.name)
    }
}

/// The complete desired placement for one company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    /// The company slug.
    pub organization: String,
    /// The tmux session name.
    pub session: String,
    /// One window per desired person, in canonical person order.
    pub windows: Vec<Window>,
    /// Every member of this company, desired or not.
    pub known_person_ids: BTreeSet<String>,
}

/// The department a person belongs to: THEIR OWN DEPARTMENT.
///
/// # NOT a placement answer any more
///
/// This used to name the WINDOW a person's pane was put in, and the whole of
/// the argument below is about which department that should be. It no longer
/// decides anything about tmux: a person's window is [`person_window_id`] and
/// has been since one window per person. What is left is the ROSTER question —
/// which unit is this person in — which the rail still asks to group its rows
/// and to answer "is everybody in Quant asleep", and which [`desired_topology`]
/// still asks so a person naming a department the roster does not declare fails
/// the pass closed instead of being placed anyway.
///
/// The history is kept because it is the reason the answer is DERIVED rather
/// than read, and that reason outlived the placement it was written for.
///
/// One rule, no head case. A head is not an exception here because a head is
/// not an exception in the model: `CLAUDE.md` states it outright — "there is no
/// 'heads a unit from outside it' — heading a department means living in it" —
/// and `HeadDecision::AppointExisting` re-points the appointee's
/// `department_id` INTO the department they now head. So `department_id` is
/// already the department a head heads, and asking `is_head_of` a second time
/// could only ever disagree with the durable record.
///
/// # What this replaced, and why the old rule lost
///
/// It used to be HEAD-IN-PARENT: a head's pane sat in their department's
/// parent window (a top-level head at the root). The justification was real —
/// a manager sits among their peers, under their own manager — and it is not
/// being traded away for a preference. It lost because it was the ONE place
/// the display disagreed with the model it was displaying: chiefd's record put
/// the head inside their unit and only this function sent their pane
/// elsewhere. Measured on the operator's own box: clicking Engineering showed
/// four engineers and NOT `ada`, who heads it, and the operator read that as
/// "everybody who is awake should be on the screen — I'm not seeing that".
///
/// **What the old rule bought is not bought back on the rail either.** The
/// rail briefly listed a child department's head under the PARENT as well as
/// under their own unit, and the operator ruled that out too: a department's
/// People list is its OWN members only (`sidebar.rs`'s `project`). A head
/// occupies one pane and one rail row — both in the unit they head, both
/// agreeing with the durable record.
///
/// # Why this is derived and not read
///
/// Unchanged by the above, and the reason the facts API publishes no
/// `paneDepartmentId`: chiefd used to persist the answer as
/// `last_pane_department_id` and rewrote it only when the activity ledger was,
/// so between a structural change and the next reconcile the stored column
/// named a window the tree no longer agreed with. Deriving the answer from the
/// roster tracks the CURRENT tree by construction.
///
/// # Errors
/// [`RosterError::UnknownDepartment`] when the person names a department the
/// roster does not declare.
pub fn pane_department_id(roster: &Roster, person: &RosterPerson) -> Result<String, RosterError> {
    if roster.department(&person.department_id).is_some() {
        Ok(person.department_id.clone())
    } else {
        Err(RosterError::UnknownDepartment {
            referrer: format!("person '{}'", person.id),
            department: person.department_id.clone(),
        })
    }
}

/// The tmux session name for a company slug: `org-<slug>` followed by
/// [`SESSION_TERMINATOR`].
///
/// The ONE definition of the convention in this crate. The binary's listing
/// surface calls it too, so a company whose manifest cannot be read still
/// prints the same session name placement would have used.
///
/// # Why the name ends in a character the slug validator refuses
///
/// It used to be a bare `org-<slug>`, and that convention could move an
/// operator into a DIFFERENT company. **`tmux -t <name>` matches exactly first
/// and falls back to PREFIX**, measured against a live server:
///
/// ```text
/// $ tmux -L t3 new-session -d -s org-acme-corp
/// $ tmux -L t3 has-session -t org-acme ; echo exit=$?
/// exit=0
/// $ tmux -L t3 display-message -p -t org-acme '#{session_name}'
/// org-acme-corp
/// ```
///
/// Two companies with prefix-related slugs — `acme` and `acme-corp` — minted
/// `org-acme` and `org-acme-corp`. While `acme` was STOPPED, every probe for
/// `org-acme` resolved to `acme-corp`'s live session: `session_exists` answered
/// yes for a company with nothing running, `chief attach acme` walked the
/// operator into `acme-corp`'s panes, and `chief stop acme` would have killed
/// `acme-corp`'s session.
///
/// # Why a terminator, and not a "safer" spelling
///
/// Avoiding today's collisions is not a fix — a convention that holds until
/// somebody creates `acme-corp-holdings` is the same defect with a longer fuse.
/// This makes the collision STRUCTURALLY impossible, and the whole argument is
/// two facts:
///
/// 1. A slug is `[a-z0-9-]` only, never empty, no leading/trailing hyphen.
///    `crate::paths::is_canonical_slug` is the validator, and it is EXACT: it
///    accepts that set and nothing else, so a slug it accepts can never contain
///    [`SESSION_TERMINATOR`]. Every slug that reaches this function must
///    satisfy it — that is the fact this rests on, and it is now checked, one
///    producer at a time, by
///    `no_input_makes_this_producer_emit_a_non_canonical_slug` in each
///    producer's own crate, over one shared adversarial corpus that contains
///    the terminator.
///
///    THIS COMMENT DOES NOT ENUMERATE THE PRODUCERS, and a version of it that
///    said "`genesis::slugify` is the only producer" stood here and was false:
///    `chiefd_core::store::organization_spec::slugify` is a second one, in a
///    crate this one is forbidden to link. The producer set is not
///    re-derivable at this line — read
///    `scripts/test/slug-producers-agree.test.mjs`, which enumerates it by
///    shape across every source language and fails when a new one appears.
/// 2. Every company session is `org-` + slug + `-` + key + [`SESSION_TERMINATOR`].
///
/// Take two company sessions `A = "org-" + a + T` and `B = "org-" + b + T`
/// where `a` and `b` are each a slug, a hyphen and a key, and `A` is a prefix
/// of `B`. If `a` is shorter than `b`, then `B`'s character at index
/// `4 + a.len()` is both `T` (it is `A`'s last character) and a character of
/// `b` (fact 2 — the position is inside `b`, since `b` is longer than `a`),
/// which fact 1 forbids. So `a` and `b` have the same length, and therefore
/// `a == b`. No pair of DIFFERENT companies can ever collide, with no length
/// limit and no list of reserved names.
///
/// # Why the key is in the name at all
///
/// A tmux server is per-socket and therefore BOX-WIDE, while a company is now
/// per-directory: two directories may hold companies with the same name, and
/// under the old `org-<slug>_` they would have been the same session — the
/// second attach would have landed the operator inside the first company's
/// panes. The key is the directory hash (`paths::company_key`), so the name is
/// unique exactly when the company is.
///
/// The slug stays in front of it because the name is something an operator
/// READS — in the status line, in `tmux ls`, in a `attach-session` they type
/// by hand — and twelve hex characters name nothing to a person. Six are
/// enough to separate the directories one box holds; the full key is on the
/// wire, where uniqueness is load-bearing.
///
/// The actuator name (`attach::actuator_session_name`) inherits the property
/// for free: it prefixes this name with discriminating text, so two actuator
/// names collide only if their company names do.
#[must_use]
pub fn session_name_for(slug: &str, key: &str) -> String {
    let short: String = key.chars().take(SESSION_KEY_CHARS).collect();
    format!("org-{slug}-{short}{SESSION_TERMINATOR}")
}

/// What every company session name begins with.
pub const SESSION_PREFIX: &str = "org-";

/// The ending every session of the company keyed `key` carries: `-<key6>_`.
///
/// # Why an ENDING is a company's identity on a tmux server
///
/// [`session_name_for`] is the whole name and needs both halves — the slug and
/// the key — but the slug lives in a company's store, so a process that has not
/// read the store cannot compose one. Two do exactly that on purpose: the rail
/// ([`crate::sidebar::for_session`]) reads nothing at all, and the click bench
/// must work against a company whose daemon is wedged or dead.
///
/// Matching the ending asks the IDENTITY question and only that one — the key
/// is the company, the slug is a display word — so it is exactly as strong as
/// matching the full name and strictly stronger than matching a slug, which two
/// directories may share.
///
/// It cannot match by accident. [`SESSION_TERMINATOR`] is a character no slug
/// may contain, so `-<key6>_` can only ever align with the END of a real
/// company session name and never with hex that happens to sit inside a slug.
///
/// Derived from [`session_name_for`] rather than spelled, so a change to the
/// naming convention cannot leave a fence refusing the very sessions the
/// actuator mints.
#[must_use]
pub fn session_key_suffix(key: &str) -> String {
    // `session_name_for("", key)` is `org--<key6>_`; everything after the
    // prefix is exactly the `-<key6>_` a real name ends with.
    let full = session_name_for("", key);
    full.strip_prefix(SESSION_PREFIX).map_or(full.clone(), str::to_owned)
}

/// Does `session` belong to the company keyed `key`?
#[must_use]
pub fn session_belongs_to(session: &str, key: &str) -> bool {
    session.starts_with(SESSION_PREFIX) && session.ends_with(&session_key_suffix(key))
}

/// How much of the directory key the tmux session name carries.
///
/// Six hex characters — 24 bits — against the handful of company directories
/// one box holds at once. The FULL key is what identifies a company on the
/// wire; this is a display name that must merely not collide on one machine,
/// and a longer one costs the operator screen width in every status line.
pub const SESSION_KEY_CHARS: usize = 6;

/// The character every company tmux session name ends with, chosen because
/// [`crate::paths::is_canonical_slug`] refuses it — so no slug that validator
/// accepts can contain it.
///
/// See [`session_name_for_slug`] for the proof this makes a prefix collision
/// between two companies impossible, and for why that proof rests on the
/// validator rather than on a count of the producers. `_` and not `.` or `:` because tmux
/// refuses both of those in a session name — they are its own target
/// separators — and not `!`, `+`, `-`, `^` or `$`, which carry meaning in tmux
/// target syntax.
pub const SESSION_TERMINATOR: char = '_';

/// The tmux session name a roster projects onto, for a company in `dir`.
///
/// The KEY comes from the directory and the SLUG from the roster, because they
/// are facts of different kinds: the key is where the company is (this client
/// is standing in it) and the slug is what it is called (chiefd published it).
/// Deriving both from one side would mean inventing the other.
///
/// [`desired_topology`] takes the composed name rather than calling this,
/// because its two hottest callers — the actuator's converge pass and the
/// brain's click path — already hold the session they are drawing into.
/// Re-deriving it there would hash a path on every click to reproduce a string
/// the caller was looking at.
#[must_use]
pub fn session_name(roster: &Roster, dir: &std::path::Path) -> String {
    session_name_for(&roster.company.slug, &host_primitives::rendezvous::company_key(dir))
}

/// Canonicalize a department name into a tmux window name.
///
/// `.` and `:` become `-` (both are tmux target separators), the result is
/// bounded to [`MAX_WINDOW_NAME_CHARS`], and the truncation happens BEFORE the
/// final trim so a cut cannot leave a dangling separator. A name that was
/// entirely forbidden characters still produces a legal window name: refusing
/// here would trade a broken name for a dead company.
///
/// Keep this in agreement with every other actuator that names the same
/// windows — this is a shared canonical-label contract, not a local tidy-up.
#[must_use]
pub fn safe_window_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|character| if character == '.' || character == ':' { '-' } else { character })
        .collect();
    let truncated: String = replaced.chars().take(MAX_WINDOW_NAME_CHARS).collect();
    let trimmed = truncated.trim().trim_matches('-').trim().to_owned();
    if trimmed.is_empty() {
        "window".to_owned()
    } else {
        trimmed
    }
}

/// The logical window id — the `@organization_window_id` tag value — of the
/// window one focused person is displayed alone in.
///
/// Not a department id, and it cannot collide with one: `Roster::validate`
/// REFUSES a department that claims this value ([`RosterError::
/// ReservedDepartmentId`]), so the check is a check and not a paragraph. Its
/// ancestor argued the same conclusion in prose and shipped no test; the reap
/// is aimed by logical window id, which makes the difference load-bearing.
///
/// [`RosterError::ReservedDepartmentId`]: crate::roster::RosterError::ReservedDepartmentId
pub const FOCUS_WINDOW_ID: &str = "__focus__";

/// The logical window id of a DEPARTMENT'S OVERVIEW — the card that reports the
/// unit's own state (`sidebar::department_card`).
///
/// # Why it is not the department's own id
///
/// A department already has a window: the one placement puts its people in, and
/// `Roster::validate` plus `plan` both refuse two windows claiming one logical
/// id. The overview is a THIRD thing — it holds no person, converge does not
/// want it, and it is minted by the click rather than by placement — so it
/// carries an id of its own, exactly like [`FOCUS_WINDOW_ID`] does for the
/// window one person is shown alone in.
///
/// Prefixed so it can never collide with a department id, which is a slug and
/// cannot contain a colon.
#[must_use]
pub fn overview_window_id(department_id: &str) -> String {
    format!("__overview__:{department_id}")
}

/// The logical window id — the `@organization_window_id` tag value — of the
/// window one person's pane lives in, alone.
///
/// # Why a person id is a window id now
///
/// A window used to be a DEPARTMENT and a pane used to be a person, so a
/// person's width was decided by how many colleagues shared their window and
/// changed whenever the operator asked to see them alone. One window per person
/// is the only shape in which a pane has one size for its whole life, which is
/// the only shape in which clicking somebody cannot reflow them.
///
/// Prefixed so it can never collide with a department id, which is a slug and
/// cannot contain a colon — the same argument [`overview_window_id`] rests on,
/// and stronger than the one [`FOCUS_WINDOW_ID`] needs, because that one is a
/// bare word and has to be REFUSED by `Roster::validate` instead.
#[must_use]
pub fn person_window_id(person_id: &str) -> String {
    format!("{PERSON_WINDOW_PREFIX}{person_id}")
}

/// The person a person-window id names, or `None` for anything else.
///
/// The inverse of [`person_window_id`], and load-bearing rather than
/// convenient: converge asks it to tell a spent person's window — which must be
/// killed WHOLE, rail and all, the moment its person stops — from a department
/// or card window, which must not.
#[must_use]
pub fn person_window_person_id(window_id: &str) -> Option<&str> {
    window_id.strip_prefix(PERSON_WINDOW_PREFIX)
}

/// What every person-window id begins with.
const PERSON_WINDOW_PREFIX: &str = "__person__:";

/// The department an overview window id names, or `None` for anything else.
///
/// The inverse of [`overview_window_id`], and load-bearing rather than
/// convenient: every sweeper that asks "is this window's department still in
/// the roster" reads a TAG, and an overview's tag is not a department id. The
/// first version shipped without this and the reaper killed each card the
/// instant it was drawn — `sidebar.department.removed … "that department is no
/// longer in the roster"` for `__overview__:research`, once per pass, so the
/// card was minted and destroyed in a loop and never reached the glass.
#[must_use]
pub fn overview_department_id(window_id: &str) -> Option<&str> {
    window_id.strip_prefix("__overview__:")
}

/// Build the complete desired placement from the roster facts.
///
/// Never from tmux observation: what is on screen is an outcome, not an input.
///
/// # ONE WINDOW PER PERSON, and why the focus argument is gone
///
/// This function used to take a `focus: Option<&str>` — the person the operator
/// had clicked — and lift that one person's pane out of their department's
/// window into [`FOCUS_WINDOW_ID`]. The reason was mechanical and, given
/// department windows, correct: a tmux layout string enumerates EVERY pane in a
/// window with explicit geometry, so a bystander can be made narrow but never
/// hidden, and the only verbs that remove a pane from a window are `break-pane`
/// and `join-pane`. Showing one person alone therefore MEANT moving their pane,
/// and a move that placement did not agree with would be undone by the next
/// converge pass.
///
/// The move was the defect. A pane that changes window changes width, and a Pi
/// whose pane changes width repaints its entire scrollback — measured on the
/// operator's own recording as text wrapped to half the body and then growing
/// out to fill it. Every attempt to make that transition prettier was an
/// attempt to make a resize invisible, which is not a thing that can be done.
///
/// So there is nothing left to move. A person is placed alone from the start;
/// the operator's selection reaches tmux as a `select-window`
/// (`sidebar::effects::show_person`) and reaches placement not at all. Focus is
/// a SELECTION, and a selection has no geometry.
///
/// # What the focus window is still for
///
/// [`FOCUS_WINDOW_ID`] still exists and is still exempt from converge's reap,
/// but nothing here ever names it: it is the rail's card holder — the standing
/// "click a person" notice, a sleeping person's card, a waking person's "…is
/// starting" body. Those are furniture, never a live person, and the brain owns
/// their whole lifetime.
///
/// # Errors
/// [`RosterError`] when the roster does not hold together. Fails CLOSED —
/// a topology computed from a half-read roster silently omits people, and an
/// actuator reads an omission as "stop them".
pub fn desired_topology(
    roster: &Roster,
    desired: &BTreeMap<String, String>,
    session: &str,
) -> Result<Topology, RosterError> {
    roster.validate()?;

    let mut people: Vec<&RosterPerson> = roster.people.iter().collect();
    // The canonical order is the `displayOrder` FIELD, never the array
    // position: it is an operator-chosen ordering and must survive a client
    // that re-sorted or re-chunked the list. It orders WINDOWS now rather than
    // panes within one, which is the same fact one level up.
    people.sort_by_key(|person| person.display_order);

    let mut windows: Vec<Window> = Vec::new();
    for person in people {
        // MEMBERSHIP COMES FROM THE DESIRED SET, and only from it. A person
        // absent from `desired` gets no pane no matter what the roster's
        // `desiredActive` says — see the module doc for why there is exactly
        // one authority here.
        let Some(launch_hash) = desired.get(&person.id) else {
            continue;
        };
        // Still asked, and still fails closed: a person naming a department the
        // roster does not declare is a roster that does not hold together, and
        // placing them anyway would hide it. The ANSWER is no longer where
        // their pane goes — see [`pane_department_id`].
        pane_department_id(roster, person)?;
        windows.push(Window {
            logical_id: person_window_id(&person.id),
            name: person.display_name.clone(),
            panes: vec![Pane {
                person_id: person.id.clone(),
                launch_hash: launch_hash.clone(),
                order: person.display_order,
            }],
        });
    }

    Ok(Topology {
        organization: roster.company.slug.clone(),
        session: session.to_owned(),
        windows,
        known_person_ids: roster.known_person_ids(),
    })
}

/// Render a topology for an operator, one line per window.
///
/// Deliberately flat and quotable rather than pretty: during the transition
/// this output is held up against what chiefd actually built, and the
/// comparison is done by eye and by `diff`. The name printed is the SANITIZED
/// one — the string tmux is told — so a line can be compared directly against
/// `tmux list-windows` output rather than against the raw fact.
#[must_use]
pub fn render(topology: &Topology) -> Vec<String> {
    let mut lines = vec![format!("session {}", topology.session)];
    lines.extend(topology.windows.iter().map(|window| {
        let panes = window
            .panes
            .iter()
            .map(|pane| format!("{}@{}", pane.person_id, short_hash(&pane.launch_hash)))
            .collect::<Vec<_>>()
            .join(",");
        format!("window {} {:?} panes={panes}", window.logical_id, window.window_name())
    }));
    lines
}

/// The first twelve hex characters of a launch hash, for an operator's eye.
///
/// Rendering the whole sha256 would push the real content of a `render` line
/// off the side of a terminal, and the line exists to be read and diffed by
/// hand. Twelve characters is enough to tell two hashes apart at a glance and
/// is never compared programmatically — every real comparison is against the
/// full value.
fn short_hash(launch_hash: &str) -> &str {
    let end = launch_hash.char_indices().nth(12).map_or(launch_hash.len(), |(index, _)| index);
    &launch_hash[..end]
}

#[cfg(test)]
mod tests;
