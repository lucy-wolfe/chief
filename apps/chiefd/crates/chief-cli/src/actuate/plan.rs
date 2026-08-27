//! The pure converge planner: desired placement in, an ordered plan of tmux
//! steps out (#751/P8).
//!
//! # Why this file is in the client
//!
//! It moved here from `chiefd-core/src/runtime/reconcile_plan.rs` and the move
//! is the point of the packet, not bookkeeping. Every decision below is a
//! decision about a **pane**: which window it belongs in, whether it may be
//! moved rather than killed, what order the windows sit in, which pane id the
//! layout names. P8 lifted only the *types* out of the backend; the walk that
//! produces the steps stayed behind, and while it stayed behind chiefd still
//! knew what a pane was — which is the exact thing the #751 mandate exists to
//! end. This is the walk, and there is no copy left in `chiefd-core`.
//!
//! Nothing here was adapted on the way across. The rules, the ordering, the
//! fail-closed refusals and the tests are the planner's own; only the module
//! path changed, plus the layout arithmetic, which is not re-declared here
//! because [`crate::layout`] already owns it (a second copy of a layout formula
//! is a second answer, and two actuators that disagree about geometry fight
//! over the same window forever).
//!
//! # One pure function, and the one that is deliberately not here
//!
//! The DESIRED placement is [`crate::placement::desired_topology`], and it is
//! not in this file. It used to be: the backend's `desired_topology` moved
//! across with the walk, still taking a `chiefd-core`-shaped `Manifest` and
//! `ActivitySnapshot` — a manifest no client can obtain over HTTP, which made
//! the whole walk un-callable from client-side facts. `placement` already
//! computed the same answer from the roster the facts API actually publishes,
//! so the crate briefly held two `desired_topology` functions and two
//! structurally identical topology types.
//!
//! The backend-shaped pair is deleted and [`crate::placement::Topology`] is the
//! one input type. That is not merely de-duplication: the derived answer is the
//! CORRECT one. chiefd used to persist head-in-parent as
//! `last_pane_department_id`, rewritten only when the activity ledger was, so a
//! reparent left the stored column naming the old parent until the next
//! reconcile. Deriving from `isHeadOf` plus the department's current parent
//! tracks the tree as it is now, which is why the facts API deliberately never
//! published the column and why that column is now gone from the schema.
//!
//! One consequence worth stating: a window's tmux name is
//! [`crate::placement::Window::window_name`], a derivation, not a stored second
//! field — so a window can never carry a raw name and a sanitized name that
//! disagree.
//!
//! * [`compute_converge_plan`] — desired + observed topology → a deterministic
//!   [`ConvergePlan`] of ordered [`Step`]s, plus the predicted respawn/kill
//!   sets. It runs no tmux command; [`super::interpret`] applies the steps and
//!   maintains the symbolic-id → tmux-id binding map.
//!
//! # TOMBSTONE: the admission ramp
//!
//! `Admission`, `RampConfig`, `SpawnSpec::delay_ms`, `ConvergePlan::admission_ms`
//! and `compute_converge_plan_with_immediate` are DELETED, by operator ruling:
//! *"just boot them all at the same time."* Every missing pane in one pass is
//! now created in that pass, with no cap, throttle or batching anywhere.
//!
//! The ramp was not wrong about the machine — #431 really did watch 34 spawns
//! in one pass drive load to ~25 on 6 cores. It was wrong about WHERE. Half of
//! it lived in chiefd, which is not on the machine being protected, and the
//! half that lived here re-derived a delay chiefd had already computed. If a
//! boot storm ever needs pacing again it belongs in the one place that both
//! spawns the processes and can see the load: this crate, at the exec seam,
//! and never in a published desired set.
//!
//! # Determinism rules encoded here (all tested in `plan/tests.rs`)
//!
//! * **Desired-person filter** — dept-chain active, `employmentState = active`,
//!   a headed department must itself be active, and `activity.active`; with one
//!   override: a decision carrying the [`HANDOFF_REQUIRED_REASON`] reason beats
//!   roster state entirely.
//! * **Adopt by tag only** — a fully ownership-tagged observed pane matching a
//!   desired person is retained and re-tagged every pass; process/pid state is
//!   never consulted (there is none here to consult).
//! * **Respawn iff the launch hash drifts** — the *only* trigger for
//!   [`Step::Respawn`] is the observed `@organization_launch_hash` tag
//!   differing from the hash chiefd published for that person. The hash is
//!   DERIVED from what the process was built from, so nothing has to remember
//!   to bump it; equally, an input that is applied LIVE (`model`, `provider`,
//!   `thinking`) is deliberately not one of its inputs and therefore never
//!   restarts anybody.
//! * **Move, never kill+respawn** — a person tagged right but in the wrong
//!   window becomes [`Step::MovePane`]; a desired window that does not exist but
//!   whose person does becomes [`Step::CreateWindowByMove`] (tmux cannot create
//!   an empty window, so bootstrap+join+kill-the-bootstrap is one compound
//!   step).
//! * **A spent person window dies WHOLE** — one window holds one person
//!   ([`crate::placement::person_window_id`]), so when that person stops there
//!   is nothing left in it but its rail. Killing the pane and then the window
//!   is two commands and therefore two frames, and the frame in between is a
//!   window that is nothing but a sidebar — which is exactly the blank
//!   right-hand side the operator has ruled out, and which becomes PERMANENT
//!   when they happen to be watching, because `interpret::kill_window` defers
//!   on the active window and the pane kill does not. So the whole window goes
//!   in one [`Step::KillWindow`], and a deferred one leaves the person's last
//!   screen up until the operator navigates away.
//! * **Fail closed before mutation** — a foreign-tagged observed object, a
//!   duplicate logical window, or a duplicate person is a hard [`PlanErr`]
//!   produced *before* any step is emitted. A merely stray pane is quarantined
//!   into `warnings` instead (#410) and left untouched.
//! * **Step order** — windows in `peopleOrder` (a window IS a person now);
//!   kills happen after the full desired walk; ordering and layout are last.
//!
//! Deliberately **no argv** in [`SpawnSpec`]: the concrete pane command is
//! resolved by [`super::spawn_cmd::launch_command`] at apply time, never here.

use std::collections::{BTreeMap, BTreeSet};

use crate::placement::Topology;

use thiserror::Error;

/// The tmux-native layout string for one window's ordered panes.
///
/// Re-exported from [`crate::layout`] rather than re-declared: this planner
/// emits [`Step::ApplyLayout`] naming the panes, and the geometry that turns
/// those ids into `checksum,WxH,x,y{…}` is one formula with one home.
pub use crate::layout::{distributed_sizes, organization_tmux_layout};

/// The one activity reason that overrides roster state in the desired-person
/// filter: a person retained just long enough to write a required handoff from
/// their existing pane stays desired even after being benched or offboarded.
pub const HANDOFF_REQUIRED_REASON: &str = "handoff-required";

// ---------------------------------------------------------------------------
// Converge plan: the pure diffing walk.
// ---------------------------------------------------------------------------

// RESTORED (#751/P10). The commit that deleted the backend-shaped INPUTS from
// this file — `Manifest`, `ActivitySnapshot`, `desired_topology` and friends —
// over-reached and took the OBSERVED and STEP vocabulary with them. Those are
// not backend types: `ObservedTopology` is what the client's own `observe`
// produces from live tmux, and `Step`/`ConvergePlan` are what it hands its own
// interpreter. Nothing outside this crate has ever named them.
//
// The lesson is worth leaving here: the deletion was done by matching section
// banners, and the section that had to go and the section that had to stay were
// adjacent under one banner. A range delete between two landmarks removes
// whatever happens to sit between them.

// ---------------------------------------------------------------------------
// Observed topology (input to `compute_converge_plan`).
// ---------------------------------------------------------------------------

/// An observed tmux pane id (e.g. `"%7"`). An opaque identity, never arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneId(pub String);

/// A window created by an earlier step in the *same* plan. Its string is the
/// logical department id, which is unique within a plan; the host executor
/// binds it to the real tmux window id when it applies the create step.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WindowSym(pub String);

/// A reference to a window that is either already observed or minted by an
/// earlier step in this plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowRef {
    /// An observed tmux window id (e.g. `"@3"`).
    Observed(String),
    /// A window created earlier in this plan.
    Created(WindowSym),
}

/// A reference to a pane that is either already observed or minted by an earlier
/// step in this plan. A created pane is identified by its person id (the
/// executor binds person → new tmux pane id at apply time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneRef {
    /// An observed tmux pane id.
    Observed(PaneId),
    /// A pane created earlier in this plan, identified by its person id.
    Created(String),
}

/// One observed tmux window, already reduced to its ownership tags. Empty string
/// means the tag is absent; a partially-tagged window fails the plan closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedWindow {
    /// The live tmux window id.
    pub tmux_id: String,
    /// `@organization_id` tag ("" if absent).
    pub organization_id: String,
    /// `@organization_window_id` tag ("" if absent).
    pub logical_id: String,
    /// Whether this window contains clean, unowned Chief UI furniture.
    ///
    /// This is a live observation only. It protects the rail's own sidebar,
    /// loading, and sleeping panes from an empty-window reap without turning
    /// them into person panes or durable placement state. A pane with any
    /// ownership option is not clean furniture and does not set this bit.
    pub protected_ui: bool,
    /// Whether this window contains an exact unowned sleeping notice.
    ///
    /// A speculative pane must not retire this notice. The notice stays until
    /// a later observation proves that a desired person pane is live and
    /// owned in the window.
    pub sleeping_notice: bool,
}

// TOMBSTONE: `ObservedWindow::waking_focus` and `Step::ClaimWakingFocus`.
//
// A cold person click painted "… is starting" into the permanent focus body and
// the actuator then CLAIMED that very pane with `respawn-pane`, so the person's
// process appeared in the cell the operator had clicked, with no second pane and
// no generic frame between them. It worked because the focus window was where
// that person's pane was going to live.
//
// It is not any more. One window per person means a woken person is placed in a
// window of their own, and a claim that re-pointed the desired placement back at
// the focus window would re-introduce exactly the move this model deletes. The
// waking body stays what it now is — a CARD the rail owns, in the rail's card
// window — and the person arrives in their own window, which the brain selects
// when their pane turns up (`brain::finish_pending_zoom`).

/// One observed tmux pane, already reduced to its ownership tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPane {
    /// The live tmux pane id.
    pub tmux_id: String,
    /// The tmux window id this pane currently lives in.
    pub tmux_window_id: String,
    /// `@organization_id` tag ("" if absent).
    pub organization_id: String,
    /// `@organization_window_id` tag ("" if absent).
    pub logical_window_id: String,
    /// `@organization_person_id` tag ("" if absent).
    pub person_id: String,
    /// `@organization_launch_hash` tag ("" if absent).
    ///
    /// Compared as a string against the hash chiefd published, and never
    /// parsed: an absent or unexpected value simply fails to match, which
    /// replaces the pane rather than adopting it. That is the safe direction,
    /// and it is why there is nothing here to validate.
    pub launch_hash: String,
    /// The pane's `#{pane_start_command}` ("" if unavailable). Its argv carries
    /// the crash-surviving `ORG_LAUNCHER_PERSON=<id>` identity env the spawner
    /// set (spawn_cmd.rs / cycle.rs), so a pane whose ownership TAGS never got
    /// written can still be attributed to its person — the evidence used to REAP
    /// a departed person's untagged orphan (#64, `reapable_orphan_pane`).
    pub start_command: String,
}

/// The observed tmux state the plan diffs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedTopology {
    /// Whether the owned session exists at all.
    pub session_exists: bool,
    /// The session-level `@organization_id` tag ("" if unset). Checked against
    /// the plan when the session already exists, so a foreign session is
    /// refused rather than adopted.
    pub session_organization: String,
    /// Observed windows (owned and auxiliary alike; filtered here).
    pub windows: Vec<ObservedWindow>,
    /// Observed panes (owned and auxiliary alike; filtered here).
    pub panes: Vec<ObservedPane>,
}

// ---------------------------------------------------------------------------
// Converge plan (output of `compute_converge_plan`).
// ---------------------------------------------------------------------------

/// A single admitted process launch. Deliberately carries no argv: the concrete
/// command is resolved host-side later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    /// The person to launch.
    pub person_id: String,
    /// The derived hash of what this process is built from.
    ///
    /// Written to the pane's `@organization_launch_hash` tag at launch, and the
    /// value every later pass diffs against. See
    /// `chiefd_core::runtime::launch_hash` for what is and is not an input.
    pub launch_hash: String,
}

/// One ordered step of a converge plan. The host executor interprets these and
/// tracks the symbolic-id → tmux-id binding map; the planner only declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The desired plan is empty: kill the owned session outright. When emitted
    /// it is the sole step.
    StopSession,
    /// Create the session with its first window and first pane.
    CreateSession {
        /// The first admitted pane.
        first: SpawnSpec,
    },
    /// Create a new window whose first pane is a freshly admitted spawn.
    CreateWindowWithSpawn {
        /// The window minted by this step.
        w: WindowSym,
        /// The window name.
        name: String,
        /// The first admitted pane.
        first: SpawnSpec,
    },
    /// Create a new window by moving an existing pane into it (bootstrap shell,
    /// join the pane, kill the bootstrap — one compound step). Consumes no
    /// stagger slot: nothing new is launched.
    CreateWindowByMove {
        /// The window minted by this step.
        w: WindowSym,
        /// The window name.
        name: String,
        /// The existing pane joined into the new window.
        move_pane: PaneId,
    },
    /// Split an existing window to add a freshly admitted pane.
    SplitPane {
        /// The window to split.
        w: WindowRef,
        /// The admitted pane.
        spec: SpawnSpec,
    },
    /// Move a correctly-tagged pane into its desired window (never kill+respawn).
    MovePane {
        /// The pane to move.
        pane: PaneId,
        /// Its destination window.
        to: WindowRef,
    },
    /// Replace the process in a pane because its launch hash drifted.
    Respawn {
        /// The pane to respawn in place (id preserved).
        pane: PaneId,
        /// The replacement launch.
        spec: SpawnSpec,
    },
    /// Re-apply ownership tags/title to a retained observed pane every pass.
    Retag {
        /// The retained pane.
        pane: PaneId,
        /// The person it belongs to.
        person_id: String,
        /// The launch hash it runs at.
        launch_hash: String,
    },
    /// Kill a tagged-ours pane whose person is no longer desired.
    KillPane {
        /// The pane to kill.
        pane: PaneId,
    },
    // TOMBSTONE: `ParkLastPane`. The last person in a RAILED DEPARTMENT window
    // used to become that department's sleeping body in place, so the window
    // never stood rail-only for one command boundary. There is no department
    // window left to keep: the window that person was alone in is THEIRS, and
    // when they stop it is killed whole (see [`Step::KillWindow`]), which
    // answers the same rail-only frame with one command instead of two.
    /// Kill an owned window nothing is desired in any more and which contains
    /// no clean Chief UI furniture.
    ///
    /// Emitted for every owned window whose logical id has no window in the
    /// desired topology.
    ///
    /// **A SPENT PERSON WINDOW IS REAPED EVEN THOUGH IT HAS A RAIL.** Every
    /// window has a rail, so the protected-UI exemption below would spare every
    /// person window for ever and leave one rail-only leftover per person who
    /// has ever stopped. The exemption is about a window whose CONTENT is
    /// furniture the rail owns; a person window's content is one person, and
    /// when that person is gone the window is not furniture, it is litter. So
    /// `crate::placement::person_window_person_id` is asked first, and a window
    /// it answers for is reaped on the ordinary rule.
    ///
    /// **NEVER for [`crate::placement::FOCUS_WINDOW_ID`].** That window is the
    /// session's one permanent view artifact: minted once by the brain and kept
    /// for the life of the session, holding the person the operator is looking
    /// at or, when they are looking at a department, a standing notice saying so.
    /// It used to be minted and reaped per gesture, which is what made a person
    /// click boot a rail and a department click destroy one — the topology churn
    /// Stage 4 of that work deletes. Converge deferred that reap
    /// anyway whenever the window held rail furniture, so the window's survival
    /// was already a SIDE EFFECT of what happened to be standing in it; this
    /// makes it a guarantee, and saves a `KillWindow` step per round for the
    /// whole life of every session.
    ///
    /// **The interpreter now refuses to let this happen to a WATCHED window.**
    /// `interpret::kill_window` reads `#{window_active}` and DEFERS — `Ok`,
    /// with a warning, no kill — when the target is the session's active
    /// window. The paragraph that used to stand here said tmux would move the
    /// operator to the last-used window, then the previous, then the next, as
    /// though that were a consolation; measured live, it was how every person
    /// click came to land on the CEO. The fallback still exists and is still
    /// tmux's behaviour; this step simply no longer causes it.
    ///
    /// Chief UI furniture is different from an empty managed window. The
    /// observer identifies only exact, unowned rail markers and records their
    /// containing window as protected UI. The planner leaves that window to the
    /// rail instead of aiming a kill which the interpreter must refuse on every
    /// pass. A managed window with no clean furniture remains reapable.
    ///
    /// The one qualification to #410 this carries — already accepted for the
    /// focus window — is that a quarantined pane survives only as long as its
    /// window is desired: a stray sheltering in a window whose department is
    /// gone dies with it.
    ///
    /// When the killed window is the one the operator is looking at, tmux
    /// falls back to the last-used window, then the previous, then the next
    /// (tmux session.c, `session_detach`) — the glass moves somewhere real
    /// rather than going blank.
    KillWindow {
        /// The window to kill.
        w: WindowRef,
    },
    /// Keep the managed windows contiguous in desired order.
    OrderWindows {
        /// The windows in desired order.
        order: Vec<WindowRef>,
    },
    /// Apply the deterministic layout to a window's ordered panes.
    ApplyLayout {
        /// The window.
        w: WindowRef,
        /// Its panes in desired order.
        panes: Vec<PaneRef>,
        /// Retire the existing sleeping notice in the same tmux transaction.
        /// This is true only after a desired person pane was observed live.
        retire_sleeping_notice: bool,
    },
}

impl Step {
    /// The variant name, for a log line and for a step error's `step` field.
    ///
    /// One definition, so the name in a success line, the name in a failure
    /// line and the name in an error are the same word.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::StopSession => "StopSession",
            Self::CreateSession { .. } => "CreateSession",
            Self::CreateWindowWithSpawn { .. } => "CreateWindowWithSpawn",
            Self::CreateWindowByMove { .. } => "CreateWindowByMove",
            Self::SplitPane { .. } => "SplitPane",
            Self::MovePane { .. } => "MovePane",
            Self::Respawn { .. } => "Respawn",
            Self::Retag { .. } => "Retag",
            Self::KillPane { .. } => "KillPane",
            Self::KillWindow { .. } => "KillWindow",
            Self::OrderWindows { .. } => "OrderWindows",
            Self::ApplyLayout { .. } => "ApplyLayout",
        }
    }

    /// WHO and WHERE this step acts on, in one short sentence.
    ///
    /// The instrument the actuator did not have. A round that failed used to
    /// name only the INDEX of the step it died on, so nobody reading the log
    /// could tell which person did not come up, or which window tmux refused.
    /// Every step names its person, its window or its pane here, and the
    /// sentence is never empty.
    #[must_use]
    pub fn subject(&self) -> String {
        match self {
            Self::StopSession => "the whole session".to_owned(),
            Self::CreateSession { first } => {
                format!("person '{}' in the first window", first.person_id)
            }
            Self::CreateWindowWithSpawn { w, name, first } => {
                format!("person '{}' in new window '{name}' ({})", first.person_id, w.0)
            }
            Self::CreateWindowByMove { w, name, move_pane } => {
                format!("pane {} into new window '{name}' ({})", move_pane.0, w.0)
            }
            Self::SplitPane { w, spec } => {
                format!("person '{}' in window {}", spec.person_id, window_ref_label(w))
            }
            Self::MovePane { pane, to } => {
                format!("pane {} into window {}", pane.0, window_ref_label(to))
            }
            Self::Respawn { pane, spec } => {
                format!("person '{}' in pane {}", spec.person_id, pane.0)
            }
            Self::Retag { pane, person_id, .. } => {
                format!("person '{person_id}' in pane {}", pane.0)
            }
            Self::KillPane { pane } => format!("pane {}", pane.0),
            Self::KillWindow { w } => format!("window {}", window_ref_label(w)),
            Self::OrderWindows { order } => format!(
                "{} window(s): {}",
                order.len(),
                order.iter().map(window_ref_label).collect::<Vec<_>>().join(", ")
            ),
            Self::ApplyLayout { w, panes, .. } => format!(
                "window {} with {} pane(s): {}",
                window_ref_label(w),
                panes.len(),
                panes.iter().map(pane_ref_label).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// A window reference an operator can read: the tmux id when it exists, the
/// symbolic id of the step that mints it when it does not.
fn window_ref_label(w: &WindowRef) -> String {
    match w {
        WindowRef::Observed(id) => id.clone(),
        WindowRef::Created(sym) => format!("(to be created: {})", sym.0),
    }
}

/// A pane reference an operator can read.
fn pane_ref_label(pane: &PaneRef) -> String {
    match pane {
        PaneRef::Observed(id) => id.0.clone(),
        PaneRef::Created(person_id) => format!("(to be created for '{person_id}')"),
    }
}
// ---------------------------------------------------------------------------
// Converge plan (output of `compute_converge_plan`).
/// The deterministic result of a converge pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConvergePlan {
    /// The ordered steps.
    pub steps: Vec<Step>,
    /// People whose pane will be respawned (launch-hash drift), in walk order.
    pub predicted_respawn_persons: Vec<String>,
    /// Panes that will be killed as no longer desired, in observed order.
    pub predicted_kill_panes: Vec<PaneId>,
    /// Non-fatal quarantine notices, one per stray/un-adoptable pane skipped by
    /// `assert_unambiguous`. Each is a legible, operator-facing string that
    /// names the offending pane and retains the `not fully ownership-tagged`
    /// phrase so the health classifier still recognizes the signal. A stray
    /// pane no longer aborts the whole plan (#410); it is quarantined here and
    /// left untouched while the rest of the company converges.
    pub warnings: Vec<String>,
    /// person → tmux pane id, for every fully-tagged owned pane this pass
    /// observed (E10/E8-S1: the same reduction `assert_unambiguous` already
    /// performed to plan against — not a second observation, not a second
    /// filter). A stray/quarantined or foreign pane never appears here.
    pub owned_panes: BTreeMap<String, String>,
    /// department (logical window id) → tmux window id, for every
    /// fully-tagged owned window this pass observed.
    pub owned_windows: BTreeMap<String, String>,
}

/// Errors that fail a plan closed before any step is produced.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanErr {
    /// The manifest's order/lookup tables are inconsistent.
    #[error("Organization manifest is invalid: {0}")]
    ManifestInvalid(String),
    /// The activity snapshot is for a different organization.
    #[error("Activity snapshot does not match organization '{organization}'")]
    ActivityMismatch {
        /// Expected organization slug.
        organization: String,
    },
    /// The activity snapshot does not cover every person exactly once.
    #[error("Activity snapshot must include every organization person")]
    ActivityIncomplete,
    /// A person's activity decision is malformed.
    #[error("Activity snapshot has invalid placement for '{0}'")]
    ActivityPlacement(String),
    /// The existing session is tagged for a different organization.
    #[error("Refusing to reconcile tmux session '{session}': ownership tag is '{found}', expected '{expected}'")]
    SessionOwnership {
        /// The session name.
        session: String,
        /// The tag found ("missing" when absent).
        found: String,
        /// The expected slug.
        expected: String,
    },
    /// An observed window carries some but not all ownership tags.
    #[error("Refusing to reconcile organization '{org}': tmux window {tmux_id} is not fully ownership-tagged")]
    WindowNotFullyTagged {
        /// The organization slug.
        org: String,
        /// The offending window id.
        tmux_id: String,
    },
    /// Two observed windows claim the same logical organization window.
    #[error("Ambiguous duplicate organization window '{logical}': {ids}")]
    DuplicateWindow {
        /// The contested logical window id.
        logical: String,
        /// The tmux ids that collided.
        ids: String,
    },
    /// An observed pane carries some but not all ownership tags.
    #[error("Refusing to reconcile organization '{org}': tmux pane {tmux_id} is not fully ownership-tagged")]
    PaneNotFullyTagged {
        /// The organization slug.
        org: String,
        /// The offending pane id.
        tmux_id: String,
    },
    /// A pane and its window disagree on organization window identity.
    #[error("Tmux pane {pane} and window {window} disagree on organization window identity")]
    WindowPaneDisagree {
        /// The pane id.
        pane: String,
        /// The window id.
        window: String,
    },
    /// Two observed panes claim the same person.
    #[error("Ambiguous duplicate organization person '{person}': {ids}")]
    DuplicatePerson {
        /// The contested person id.
        person: String,
        /// The tmux ids that collided.
        ids: String,
    },
    /// An internal invariant was violated. Should be unreachable.
    #[error("internal reconcile-plan invariant violated: {0}")]
    Internal(String),
    /// The layout could not be computed for the given dimensions.
    #[error("Tmux layout error: {0}")]
    Layout(String),
}

/// The owned subset of an observed topology, after the fail-closed filter.
struct Owned {
    windows: Vec<ObservedWindow>,
    panes: Vec<ObservedPane>,
    /// Stray/un-adoptable panes that were quarantined (skipped, never
    /// actuated) rather than aborting the plan (#410). Each is a legible
    /// warning string naming the pane.
    warnings: Vec<String>,
    /// Untagged orphans of DEPARTED people to REAP (#64) — a leaked pane whose
    /// ownership tags never got written, attributed to a no-longer-desired
    /// member by its crash-surviving `ORG_LAUNCHER_PERSON=` start-command env.
    /// Not owned (never retagged/respawned); killed in the removal pass.
    reap: Vec<ObservedPane>,
}

/// The identity env key the spawner writes into a pane's argv/start command
/// (chiefd-host `converge_apply::cycle`/`spawn_cmd`, key `ORG_LAUNCHER_PERSON`).
/// chiefd-core cannot depend on chiefd-host, so the literal is mirrored here;
/// the reap test asserts against this exact spelling so a rename is caught.
const ORG_LAUNCHER_PERSON_ENV: &str = "ORG_LAUNCHER_PERSON";

/// #64: the untagged orphan of a person who has LEFT the running fleet, or
/// `None`. The reap mirror of the launcher's env attribution — same
/// crash-surviving evidence, opposite roster verdict. Deliberately as narrow as
/// possible so the stray/foreign-pane quarantine (#410/#438) is untouched:
///
/// - fully UNTAGGED only — a partially tagged pane is a real ownership defect
///   and stays quarantined (never silently killed);
/// - its start command names EXACTLY ONE person via `ORG_LAUNCHER_PERSON=`, and
///   that person is a KNOWN member of this org — a pane naming nobody or a
///   stranger stays a foreign pane we never touch;
/// - that person must NOT be in the desired roster (departed/benched/stopped).
fn reapable_orphan_pane(
    pane: &ObservedPane,
    known_person_ids: &BTreeSet<String>,
    desired_person_ids: &BTreeSet<&str>,
) -> Option<String> {
    if !pane.organization_id.is_empty()
        || !pane.logical_window_id.is_empty()
        || !pane.person_id.is_empty()
        || !pane.launch_hash.is_empty()
    {
        return None;
    }
    let mut named = known_person_ids
        .iter()
        .filter(|id| pane.start_command.contains(&format!("{ORG_LAUNCHER_PERSON_ENV}={id}")));
    let person = named.next()?;
    if named.next().is_some() {
        return None; // ambiguous — names more than one known member
    }
    if desired_person_ids.contains(person.as_str()) {
        return None; // still desired — not an orphan
    }
    Some(person.clone())
}

/// Port of `assertUnambiguousObservation` (org-tmux.ts:588-621). Returns the
/// fully-tagged owned windows/panes.
///
/// A stray, un-adoptable pane — one that carries tags but is not fully/correctly
/// ownership-tagged for this org, or an untagged pane sitting inside a tagged
/// company window — is NOT fatal (#410). Aborting the whole plan on one such
/// pane produced zero actuation for the entire company every pass until the pane
/// vanished, so the fleet could not self-heal for minutes. Instead the offending
/// pane is quarantined: excluded from the owned set (never adopted, never
/// actuated — the fail-closed intent is preserved) and recorded as a legible
/// warning so the rest of the company still converges. Genuine ambiguity about a
/// *managed* identity — a partially tagged window, a duplicate logical window, a
/// pane/window identity disagreement, or two panes claiming one person — remains
/// fatal, because those cannot be resolved by simply skipping one object.
fn assert_unambiguous(desired: &Topology, observed: &ObservedTopology) -> Result<Owned, PlanErr> {
    let org = &desired.organization;
    let mut warnings: Vec<String> = Vec::new();
    let mut reap: Vec<ObservedPane> = Vec::new();
    let desired_person_ids: BTreeSet<&str> =
        desired.windows.iter().flat_map(|w| w.panes.iter().map(|p| p.person_id.as_str())).collect();

    let mut windows: Vec<ObservedWindow> = Vec::new();
    let mut by_logical: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for window in &observed.windows {
        let has_tag = !window.organization_id.is_empty() || !window.logical_id.is_empty();
        if !has_tag {
            continue;
        }
        if &window.organization_id != org || window.logical_id.is_empty() {
            return Err(PlanErr::WindowNotFullyTagged {
                org: org.clone(),
                tmux_id: window.tmux_id.clone(),
            });
        }
        by_logical.entry(window.logical_id.clone()).or_default().push(window.tmux_id.clone());
        windows.push(window.clone());
    }
    for (logical, ids) in &by_logical {
        if ids.len() > 1 {
            return Err(PlanErr::DuplicateWindow { logical: logical.clone(), ids: ids.join(", ") });
        }
    }
    let window_by_tmux: BTreeMap<&str, &ObservedWindow> =
        windows.iter().map(|w| (w.tmux_id.as_str(), w)).collect();

    let mut panes: Vec<ObservedPane> = Vec::new();
    let mut by_person: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pane in &observed.panes {
        let has_tag = !pane.organization_id.is_empty()
            || !pane.logical_window_id.is_empty()
            || !pane.person_id.is_empty()
            || !pane.launch_hash.is_empty();
        let window = window_by_tmux.get(pane.tmux_window_id.as_str());
        if !has_tag && window.is_none() {
            continue;
        }
        let window = match window {
            Some(window)
                if &pane.organization_id == org
                    && !pane.logical_window_id.is_empty()
                    && !pane.person_id.is_empty() =>
            {
                *window
            }
            _ => {
                // #64: before quarantining, ask whether this is our OWN leaked
                // orphan. A fully-untagged pane inside a managed window whose
                // start command names exactly one KNOWN member no longer desired
                // is a departed person's pane whose tags never got written —
                // reap it (mirror of the launcher's env attribution). A pane
                // naming nobody, a stranger, or a still-desired person, or one
                // carrying partial tags, falls through to the quarantine below,
                // so the stray/foreign-pane safety invariant (#410/#438) holds.
                if window.is_some()
                    && reapable_orphan_pane(pane, &desired.known_person_ids, &desired_person_ids)
                        .is_some()
                {
                    reap.push(pane.clone());
                    continue;
                }
                // A stray/un-adoptable pane: quarantine it (skip, never touch)
                // and keep planning the rest of the company (#410). The message
                // retains the `not fully ownership-tagged` phrase so the health
                // classifier still recognizes the signal.
                warnings.push(format!(
                    "Quarantined stray tmux pane {} in organization '{org}': not fully ownership-tagged; \
                     skipped and left untouched so the rest of the company can converge",
                    pane.tmux_id,
                ));
                continue;
            }
        };
        if pane.logical_window_id != window.logical_id {
            // THE LABEL LAGS THE MOVE, AND THE MOVE IS THE FACT.
            //
            // A pane's `@organization_window_id` is a CACHED ANSWER to "which
            // window am I in"; the window that physically contains it is tmux's
            // own. Moving a person between windows is two tmux commands — the
            // join and the retag — and no ordering makes them one: tag first and
            // the pane disagrees with the window it is still in; join first and
            // it disagrees with the window it has just entered. Another process
            // listing panes in between sees a disagreement either way.
            //
            // This used to fail the WHOLE plan closed for it. Measured on a live
            // company: six converge passes in thirty seconds applied NOTHING
            // because one pane was mid-move, and the operator's window sat
            // wrongly laid out the entire time — a person left sitting in the
            // sidebar's column until they clicked the department to force a
            // re-lay. A transient label is not a reason to stop converging a
            // company.
            //
            // NOTHING IS REPAIRED OPTIMISTICALLY, and nothing needs to be. The
            // pane is passed through with its ORIGINAL tags, which is what makes
            // the rest of the pass correct on its own:
            //
            // * placement reads `tmux_window_id` — physical containment — so
            //   `MovePane` already computes from the truth, not the label;
            // * the retag walk diffs those original tags against desired, sees
            //   the drifted `logical_window_id`, and emits `Step::Retag`, which
            //   is the label being corrected by the ordinary machinery;
            // * the person stays accounted for, so nothing spawns a second pane
            //   for somebody who already has one.
            //
            // THE STRANGER FENCE IS UNCHANGED. This leniency is only for a pane
            // this company KNOWS: same organization (checked above), inside one
            // of our own fully-tagged windows (that is where `window` came
            // from), naming somebody on our roster. Anything else still fails
            // closed below, because for a pane we cannot account for, a
            // conflicting identity is exactly the ambiguity worth stopping on.
            if desired.known_person_ids.contains(&pane.person_id) {
                warnings.push(format!(
                    "Tmux pane {} is tagged for window '{}' but sits in '{}'; it is one of                      this company's own and its tag is being corrected, so the pass                      continues",
                    pane.tmux_id, pane.logical_window_id, window.logical_id,
                ));
            } else {
                return Err(PlanErr::WindowPaneDisagree {
                    pane: pane.tmux_id.clone(),
                    window: window.tmux_id.clone(),
                });
            }
        }
        by_person.entry(pane.person_id.clone()).or_default().push(pane.tmux_id.clone());
        panes.push(pane.clone());
    }
    for (person, ids) in &by_person {
        if ids.len() > 1 {
            return Err(PlanErr::DuplicatePerson { person: person.clone(), ids: ids.join(", ") });
        }
    }

    Ok(Owned { windows, panes, warnings, reap })
}

/// A window in the mutable working set as the walk proceeds.
struct WorkWindow {
    window_ref: WindowRef,
    logical_id: String,
}

/// A pane in the mutable working set as the walk proceeds. `launch_hash` is a
/// string because the tmux tag is one, and the comparison is equality.
struct WorkPane {
    pane_ref: PaneRef,
    window: WindowRef,
    person_id: String,
    launch_hash: String,
}

fn as_pane_id(pane_ref: &PaneRef) -> Option<PaneId> {
    match pane_ref {
        PaneRef::Observed(id) => Some(id.clone()),
        PaneRef::Created(_) => None,
    }
}

/// The launch spec for one desired pane.
///
/// `launch_hash` is what the actuator DIFFS on.
fn spawn_spec(pane: &crate::placement::Pane) -> SpawnSpec {
    SpawnSpec { person_id: pane.person_id.clone(), launch_hash: pane.launch_hash.clone() }
}

/// Compute the deterministic converge plan from a desired placement and the
/// observed tmux topology. Port of the diffing walk inside
/// `reconcileOrganizationTmuxOnce` (org-tmux.ts:705-848), minus every I/O
/// effect.
///
/// **Every missing pane is created in this one plan.** There is no cap, no
/// batch and no delay; see the ramp tombstone in the module doc.
///
/// # Errors
/// Fails closed (no steps produced) on a foreign session, a partially/foreign
/// tagged observed object, a duplicate logical window, or a duplicate person.
pub fn compute_converge_plan(
    desired: &Topology,
    observed: &ObservedTopology,
) -> Result<ConvergePlan, PlanErr> {
    // A session that already exists must prove it is ours before we touch it.
    if observed.session_exists && observed.session_organization != desired.organization {
        return Err(PlanErr::SessionOwnership {
            session: desired.session.clone(),
            found: if observed.session_organization.is_empty() {
                "missing".to_string()
            } else {
                observed.session_organization.clone()
            },
            expected: desired.organization.clone(),
        });
    }

    // Fail closed before any step. Un-adoptable stray panes are quarantined
    // here (non-fatal, #410) and surfaced as `warnings` on the returned plan.
    let owned = assert_unambiguous(desired, observed)?;
    let warnings = owned.warnings.clone();
    // The observation this pass has in hand — E10/E8-S1 publishes this same
    // reduction into the durable `runtime` row; it is not recomputed, only
    // carried onto every return point below (including the two early
    // returns), so a desired-empty pass still reports what tmux truly held.
    let owned_panes: BTreeMap<String, String> =
        owned.panes.iter().map(|p| (p.person_id.clone(), p.tmux_id.clone())).collect();
    let owned_windows: BTreeMap<String, String> =
        owned.windows.iter().map(|w| (w.logical_id.clone(), w.tmux_id.clone())).collect();

    let desired_people: BTreeSet<&str> =
        desired.windows.iter().flat_map(|w| w.panes.iter().map(|p| p.person_id.as_str())).collect();

    // Desired-empty: stop the owned session as the sole step (nothing else).
    if desired.windows.is_empty() {
        if observed.session_exists {
            let kills: Vec<PaneId> =
                owned.panes.iter().map(|p| PaneId(p.tmux_id.clone())).collect();
            return Ok(ConvergePlan {
                steps: vec![Step::StopSession],
                predicted_respawn_persons: Vec::new(),
                predicted_kill_panes: kills,
                warnings,
                owned_panes,
                owned_windows,
            });
        }
        return Ok(ConvergePlan {
            steps: Vec::new(),
            predicted_respawn_persons: Vec::new(),
            predicted_kill_panes: Vec::new(),
            warnings,
            owned_panes,
            owned_windows,
        });
    }

    let mut steps: Vec<Step> = Vec::new();
    let mut predicted_respawn_persons: Vec<String> = Vec::new();
    let mut predicted_kill_panes: Vec<PaneId> = Vec::new();

    let mut work_windows: Vec<WorkWindow> = owned
        .windows
        .iter()
        .map(|w| WorkWindow {
            window_ref: WindowRef::Observed(w.tmux_id.clone()),
            logical_id: w.logical_id.clone(),
        })
        .collect();
    let mut work_panes: Vec<WorkPane> = owned
        .panes
        .iter()
        .map(|p| WorkPane {
            pane_ref: PaneRef::Observed(PaneId(p.tmux_id.clone())),
            window: WindowRef::Observed(p.tmux_window_id.clone()),
            person_id: p.person_id.clone(),
            launch_hash: p.launch_hash.clone(),
        })
        .collect();

    // The live ownership tags of each retained observed pane, keyed by tmux id,
    // so the retag walk below can diff them against desired and emit a `Retag`
    // ONLY when a tag actually drifted. A fully converged pane emits nothing.
    let observed_pane_tags: BTreeMap<&str, &ObservedPane> =
        owned.panes.iter().map(|p| (p.tmux_id.as_str(), p)).collect();
    // Windows whose pane membership changed this pass — a pane created, moved in,
    // moved out, or killed. ONLY these need their layout re-applied; a converged
    // window (same panes, same order) is left untouched. `Respawn` preserves the
    // pane's identity and window and so never dirties a layout.
    let mut layout_dirty: Vec<WindowRef> = Vec::new();

    // Create the session first when it does not exist. The created window and
    // pane join the working set as if observed, so the walk below treats the
    // first desired window/pane as already existing (mirrors the TS re-read).
    if !observed.session_exists {
        let first_window = &desired.windows[0];
        let first_pane = &first_window.panes[0];
        steps.push(Step::CreateSession { first: spawn_spec(first_pane) });
        let sym = WindowSym(first_window.logical_id.clone());
        let win_ref = WindowRef::Created(sym);
        // A freshly created window is born with a pane: its layout is dirty.
        layout_dirty.push(win_ref.clone());
        work_windows.push(WorkWindow {
            window_ref: win_ref.clone(),
            logical_id: first_window.logical_id.clone(),
        });
        work_panes.push(WorkPane {
            pane_ref: PaneRef::Created(first_pane.person_id.clone()),
            window: win_ref,
            person_id: first_pane.person_id.clone(),
            launch_hash: first_pane.launch_hash.clone(),
        });
    }

    let mut window_binding: BTreeMap<String, WindowRef> = BTreeMap::new();
    let mut pane_binding: BTreeMap<String, PaneRef> = BTreeMap::new();

    for desired_window in &desired.windows {
        // Resolve to an owned ref first so the immutable `find` borrow is
        // released before the `None` arm mutates the working set (the classic
        // NLL get-then-insert limitation otherwise rejects this).
        let existing_window = work_windows
            .iter()
            .find(|w| w.logical_id == desired_window.logical_id)
            .map(|w| w.window_ref.clone());
        // A focused person's home is kept in the desired topology as their
        // return destination. On the first wake that department can still have
        // no live window and, because the person is in `__focus__`, it has no
        // pane from which a window can be made. Absence is already the correct
        // empty state. When focus ends, the returning person creates it.
        if existing_window.is_none() && desired_window.panes.is_empty() {
            continue;
        }
        let win_ref = match existing_window {
            Some(window_ref) => window_ref,
            None => {
                // The window does not exist. If one of its people already has a
                // pane, create the window by moving that pane in; otherwise
                // create it around a freshly admitted first pane.
                let existing = desired_window
                    .panes
                    .iter()
                    .find(|p| work_panes.iter().any(|op| op.person_id == p.person_id));
                if let Some(existing) = existing {
                    let index = work_panes
                        .iter()
                        .position(|op| op.person_id == existing.person_id)
                        .ok_or_else(|| {
                            PlanErr::Internal(format!("lost pane for '{}'", existing.person_id))
                        })?;
                    let move_pane = as_pane_id(&work_panes[index].pane_ref).ok_or_else(|| {
                        PlanErr::Internal(format!(
                            "cannot move in-plan pane for '{}'",
                            existing.person_id
                        ))
                    })?;
                    let sym = WindowSym(desired_window.logical_id.clone());
                    steps.push(Step::CreateWindowByMove {
                        w: sym.clone(),
                        name: desired_window.window_name(),
                        move_pane,
                    });
                    let win_ref = WindowRef::Created(sym);
                    // The pane leaves its old window (layout there shrinks) and
                    // joins the new one: both windows' layouts are dirty.
                    layout_dirty.push(work_panes[index].window.clone());
                    layout_dirty.push(win_ref.clone());
                    work_panes[index].window = win_ref.clone();
                    work_windows.push(WorkWindow {
                        window_ref: win_ref.clone(),
                        logical_id: desired_window.logical_id.clone(),
                    });
                    win_ref
                } else {
                    let first_missing = desired_window
                        .panes
                        .iter()
                        .find(|p| !work_panes.iter().any(|op| op.person_id == p.person_id))
                        .ok_or_else(|| {
                            PlanErr::Internal(format!(
                                "desired window '{}' has no missing pane to create around",
                                desired_window.logical_id
                            ))
                        })?;
                    let sym = WindowSym(desired_window.logical_id.clone());
                    steps.push(Step::CreateWindowWithSpawn {
                        w: sym.clone(),
                        name: desired_window.window_name(),
                        first: spawn_spec(first_missing),
                    });
                    let win_ref = WindowRef::Created(sym);
                    // A freshly created window is born with a pane: layout dirty.
                    layout_dirty.push(win_ref.clone());
                    work_panes.push(WorkPane {
                        pane_ref: PaneRef::Created(first_missing.person_id.clone()),
                        window: win_ref.clone(),
                        person_id: first_missing.person_id.clone(),
                        launch_hash: first_missing.launch_hash.clone(),
                    });
                    work_windows.push(WorkWindow {
                        window_ref: win_ref.clone(),
                        logical_id: desired_window.logical_id.clone(),
                    });
                    win_ref
                }
            }
        };
        window_binding.insert(desired_window.logical_id.clone(), win_ref.clone());

        for desired_pane in &desired_window.panes {
            match work_panes.iter().position(|op| op.person_id == desired_pane.person_id) {
                None => {
                    steps.push(Step::SplitPane {
                        w: win_ref.clone(),
                        spec: spawn_spec(desired_pane),
                    });
                    // A new pane is added to this window: its layout is dirty.
                    layout_dirty.push(win_ref.clone());
                    let pane_ref = PaneRef::Created(desired_pane.person_id.clone());
                    work_panes.push(WorkPane {
                        pane_ref: pane_ref.clone(),
                        window: win_ref.clone(),
                        person_id: desired_pane.person_id.clone(),
                        launch_hash: desired_pane.launch_hash.clone(),
                    });
                    pane_binding.insert(desired_pane.person_id.clone(), pane_ref);
                }
                Some(index) => {
                    let mut respawned = false;
                    if work_panes[index].window != win_ref {
                        let pane = as_pane_id(&work_panes[index].pane_ref).ok_or_else(|| {
                            PlanErr::Internal(format!(
                                "cannot move in-plan pane for '{}'",
                                desired_pane.person_id
                            ))
                        })?;
                        steps.push(Step::MovePane { pane, to: win_ref.clone() });
                        // The pane leaves its old window and joins this one:
                        // both windows' layouts are dirty.
                        layout_dirty.push(work_panes[index].window.clone());
                        layout_dirty.push(win_ref.clone());
                        work_panes[index].window = win_ref.clone();
                    }
                    // THE ONLY RESPAWN TRIGGER: the launch hash drifted. A
                    // pane whose tag equals the desired hash is running exactly
                    // what chiefd wants and is adopted untouched, however long
                    // it has been up; a pane whose tag differs is stale and is
                    // replaced, whether it drifted because a person moved
                    // department, because their launch command changed, or
                    // because a launcher deploy rewrote the extension source
                    // under them.
                    if work_panes[index].launch_hash != desired_pane.launch_hash {
                        let pane = as_pane_id(&work_panes[index].pane_ref).ok_or_else(|| {
                            PlanErr::Internal(format!(
                                "cannot respawn in-plan pane for '{}'",
                                desired_pane.person_id
                            ))
                        })?;
                        steps.push(Step::Respawn { pane, spec: spawn_spec(desired_pane) });
                        work_panes[index].launch_hash = desired_pane.launch_hash.clone();
                        predicted_respawn_persons.push(desired_pane.person_id.clone());
                        respawned = true;
                    }
                    // Re-tag a retained (observed) pane ONLY when its live tags have
                    // actually drifted from desired — a converged pane emits no
                    // step. A pane created earlier in this plan was already tagged
                    // by its create step; a respawn re-tags in full as its last act
                    // (interpret.rs `respawn`), so neither needs a separate Retag.
                    if let PaneRef::Observed(pane) = work_panes[index].pane_ref.clone() {
                        let drifted = observed_pane_tags.get(pane.0.as_str()).is_none_or(|obs| {
                            obs.organization_id != desired.organization
                                || obs.logical_window_id != desired_window.logical_id
                                || obs.person_id != desired_pane.person_id
                                || obs.launch_hash != desired_pane.launch_hash
                        });
                        if drifted && !respawned {
                            steps.push(Step::Retag {
                                pane,
                                person_id: desired_pane.person_id.clone(),
                                launch_hash: desired_pane.launch_hash.clone(),
                            });
                        }
                    }
                    pane_binding
                        .insert(desired_pane.person_id.clone(), work_panes[index].pane_ref.clone());
                }
            }
        }
    }

    // Kills happen after the full desired walk, in observed order.
    //
    // Windows this walk takes with the pane inside them, so the reap below does
    // not aim a second `KillWindow` at a window that is already going.
    let mut killed_windows: BTreeSet<&str> = BTreeSet::new();
    for (work_index, work_pane) in work_panes.iter().enumerate() {
        if desired_people.contains(work_pane.person_id.as_str()) {
            continue;
        }
        if let PaneRef::Observed(pane) = &work_pane.pane_ref {
            // THE WHOLE WINDOW, WHEN THE WINDOW IS THIS PERSON'S OWN. One
            // window holds one person, so their pane leaving is the window
            // emptying, and killing the pane first publishes a rail-only window
            // to whoever is looking — permanently, if that is the operator,
            // because `interpret::kill_window` defers on the active window and
            // `kill_pane` does not. One command instead of two, and a deferred
            // one leaves their last screen up rather than a sidebar and a void.
            let alone = !work_panes.iter().enumerate().any(|(other_index, other)| {
                other_index != work_index && other.window == work_pane.window
            });
            let own_window = match &work_pane.window {
                WindowRef::Observed(window_id) if alone => owned
                    .windows
                    .iter()
                    .find(|window| {
                        window.tmux_id == *window_id
                            && crate::placement::person_window_person_id(&window.logical_id)
                                == Some(work_pane.person_id.as_str())
                    })
                    .map(|window| window.logical_id.as_str()),
                _ => None,
            };
            if let Some(logical_id) = own_window {
                steps.push(Step::KillWindow { w: work_pane.window.clone() });
                killed_windows.insert(logical_id);
            } else {
                steps.push(Step::KillPane { pane: pane.clone() });
                // A pane leaves a window it shared: the survivors' layout is
                // dirty. A window that is going needs no layout.
                layout_dirty.push(work_pane.window.clone());
            }
            predicted_kill_panes.push(pane.clone());
        }
    }

    // #64: reap the untagged orphans of departed people. These carry no
    // ownership tags, so no owned-kill above sees them; without this they
    // survive as phantom `%N`s no one owns. They were never in `work_panes`
    // (excluded from the owned set), so a plain KillPane + a dirty layout for
    // their window is the whole actuation.
    for orphan in &owned.reap {
        let pane = PaneId(orphan.tmux_id.clone());
        steps.push(Step::KillPane { pane: pane.clone() });
        predicted_kill_panes.push(pane);
        layout_dirty.push(WindowRef::Observed(orphan.tmux_window_id.clone()));
    }

    // WINDOWS NOBODY IS DESIRED IN ANY MORE. A truly empty managed window is
    // stale and is reaped. A window holding clean Chief UI furniture is NOT
    // empty: the rail owns that live view, and observation marked it explicitly
    // so planning reaches steady state instead of asking the interpreter to
    // defer the same kill on every pass.
    let desired_window_ids: BTreeSet<&str> =
        desired.windows.iter().map(|w| w.logical_id.as_str()).collect();
    for window in &owned.windows {
        if desired_window_ids.contains(window.logical_id.as_str()) {
            continue;
        }
        if killed_windows.contains(window.logical_id.as_str()) {
            continue;
        }
        // A SPENT PERSON WINDOW IS NOT FURNITURE. Every window carries a rail,
        // so `protected_ui` is true of all of them and would spare every person
        // window whose person has stopped — one rail-only leftover per person
        // who has ever been up. See `Step::KillWindow`'s own doc.
        if window.protected_ui
            && crate::placement::person_window_person_id(&window.logical_id).is_none()
        {
            continue;
        }
        // THE FOCUS WINDOW IS PERMANENT. See `Step::KillWindow`'s own doc: the
        // brain mints it once per session and holds its content, so converge
        // neither creates nor destroys it. Skipping it here is also what stops a
        // deferred-forever `KillWindow` being planned, executed and refused on
        // every converge round for the life of the session.
        if window.logical_id == crate::placement::FOCUS_WINDOW_ID {
            continue;
        }
        // A DEPARTMENT'S OVERVIEW IS THE RAIL'S, NOT CONVERGE'S — the same
        // exemption and for the same reason. The brain mints it on a click and
        // holds its content; placement has never heard of it, so converge would
        // read it as a stray window and kill it. Measured on a live box: the card
        // was minted on every click and reaped before it reached the glass, so
        // the operator clicked a department and nothing happened.
        //
        // Its lifetime is owned by `effects::close_sleeping_notices`, which
        // retires it when the department it reports on leaves the roster.
        if crate::placement::overview_department_id(&window.logical_id).is_some() {
            continue;
        }
        steps.push(Step::KillWindow { w: WindowRef::Observed(window.tmux_id.clone()) });
        killed_windows.insert(window.logical_id.as_str());
    }

    // Ordering and layout are last, and BOTH are emitted only on real change so a
    // converged company yields an empty plan (no tmux execs, no writes).
    //
    // OrderWindows: only when the observed managed-window order differs from the
    // desired order. At steady state the owned windows already sit in
    // `department_order`, so nothing is emitted. A window killed just above is
    // read out of the observed order — it will not be there when the ordering
    // runs, and counting it would emit a `move-window` sequence for a company
    // that is otherwise converged.
    //
    // THE FOCUS WINDOW IS ORDERED BY NOBODY, and is left out of both sides of
    // this comparison. It is minted last (`-a -t '<session>:$'`) and it is
    // permanent, so it is always the last managed window and there is nothing to
    // shuffle. Counting it would be worse than pointless: it exists on every
    // pass and is DESIRED only while somebody is focused, so a session at rest
    // would see `[executive, quant] != [executive, quant, __focus__]` and emit a
    // `move-window` sequence for every window, every round, for ever.
    let ordered = |logical: &str| {
        logical != crate::placement::FOCUS_WINDOW_ID
            && crate::placement::overview_department_id(logical).is_none()
    };
    let desired_order: Vec<&str> = desired
        .windows
        .iter()
        .map(|w| w.logical_id.as_str())
        .filter(|logical| ordered(logical) && window_binding.contains_key(*logical))
        .collect();
    if desired_order.len() > 1 {
        let observed_order: Vec<&str> = owned
            .windows
            .iter()
            .filter(|window| {
                let logical = window.logical_id.as_str();
                ordered(logical)
                    && desired_window_ids.contains(logical)
                    && !killed_windows.contains(logical)
            })
            .map(|window| window.logical_id.as_str())
            .collect();
        if desired_order != observed_order {
            let order = desired
                .windows
                .iter()
                .filter(|w| ordered(&w.logical_id) && window_binding.contains_key(&w.logical_id))
                .map(|w| window_binding[&w.logical_id].clone())
                .collect();
            steps.push(Step::OrderWindows { order });
        }
    }
    // ApplyLayout: only for a window whose pane membership changed this pass. A
    // converged window keeps its geometry and emits nothing. A deliberately
    // retained empty home has no person cell to lay out: its rail keeps the
    // window alive while the person is focused, and an empty layout is invalid.
    for desired_window in &desired.windows {
        let Some(win_ref) = window_binding.get(&desired_window.logical_id).cloned() else {
            continue;
        };
        if desired_window.panes.is_empty() {
            continue;
        }
        let panes: Vec<PaneRef> =
            desired_window.panes.iter().map(|p| pane_binding[&p.person_id].clone()).collect();
        let retire_sleeping_notice =
            owned.windows.iter().any(|window| {
                window.logical_id == desired_window.logical_id && window.sleeping_notice
            }) && panes.iter().any(|pane| matches!(pane, PaneRef::Observed(_)));
        if !layout_dirty.contains(&win_ref) && !retire_sleeping_notice {
            continue;
        }
        steps.push(Step::ApplyLayout { w: win_ref, panes, retire_sleeping_notice });
    }

    Ok(ConvergePlan {
        steps,
        predicted_respawn_persons,
        predicted_kill_panes,
        warnings,
        owned_panes,
        owned_windows,
    })
}

#[cfg(test)]
mod tests;
