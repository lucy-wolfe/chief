//! The operator's sidebar rail: one expandable company tree, in one pane.
//!
//! # Why one pane and not two
//!
//! Two tmux panes cannot share a collapse control, cannot be held to a 50/50
//! split without a third party arbitrating them, and would each need their own
//! scroll protocol. One program owns both sections, so the split is arithmetic
//! and the control belongs to somebody.
//!
//! More decisively: `select-layout` is fed an ABSOLUTE layout string that
//! enumerates every pane in the window with explicit geometry
//! ([`crate::layout`]). A rail is therefore a CELL of the window's layout, not
//! a pane laid beside it — and one cell is one pane.
//!
//! # Where it exists, and the disclosure that rests on it
//!
//! Only in the operator's own company session, `org-<slug>_`. [`for_session`]
//! is the single point that decides this and it REFUSES everything else — the
//! headless `chiefd-actuator-*` session, a bare shell, another company's
//! session, and a prefix-lookalike.
//!
//! That refusal is load-bearing rather than tidy. The rail reads the roster
//! with the OPERATOR bearer, a non-person identity whose scope is
//! unconditional: no disclosure fence narrows it to a subtree. That is sound
//! here because the viewer is the operator, on the operator's own box, holding
//! that key already — and because the CEO heads the root department, so the
//! whole tree is that viewer's subtree anyway. Nothing is disclosed that the
//! viewer could not already see. It would STOP being sound the moment a rail
//! were rendered for a person-scoped viewer, who would then be shown the entire
//! tree regardless of their own subtree. The safety does not rest on nobody
//! having moved it yet: it rests on [`for_session`], and on the tests that pin
//! what [`for_session`] refuses.
//!
//! # What is a fact here, and what is a decision
//!
//! * The department tree and the person order are chiefd's facts, read from
//!   `POST /v1/org/roster/desired`.
//! * Who SHOULD be up is chiefd's fact, read from
//!   `POST /v1/org/runtime/desired` — never re-derived from `desiredActive`,
//!   which is the duplicated-predicate defect [`crate::roster`] documents.
//! * Who IS up is TMUX's fact, and chiefd does not have it: `resident.rs`'s
//!   "Nothing goes up" means no report ever travels back. A person is live when
//!   a live pane carries their `@organization_person_id`.
//! * Everything else in this file — the 50/50 split, the scroll offsets, which
//!   row a click landed on — is a display decision and belongs to the client.

use std::collections::{BTreeMap, BTreeSet};

use host_primitives::rendezvous::company_key;

/// The rail's whole tmux surface: one command in, its stdout out.
///
/// A seam, and a narrow one on purpose. The rail issues a short list of verbs —
/// `list-panes`, `display-message`, `select-window`, `select-pane`,
/// `select-layout`, `resize-pane`, `set-option` — and every one of them acts on
/// the operator's own terminal. Nothing here can write to a company, so the
/// seam does not need to carry a result: a `select-pane` at a pane that has
/// just closed is a no-op the rail should survive, not an error it should
/// report.
///
/// It exists as a trait because tmux placement is a product invariant and
/// `chief/CLAUDE.md` requires simulated tmux coverage for it: a recording
/// implementation is what lets the click-to-zoom sequence be asserted as a
/// sequence, and the live implementation in the binary is what proves the same
/// verbs work against a real tmux server.
pub trait Tmux: Send + Sync {
    /// Run one tmux command and return its stdout, empty when it failed.
    fn run(&self, args: &[&str]) -> String;
}

/// Apply the guarded tmux half of one card wake action.
///
/// The session brain calls this only after its in-memory card authority agrees.
/// Keeping the live compare-and-set at this boundary also lets the real tmux
/// regression drive the exact product guard.
#[must_use]
pub fn authorize_sleeping_card(
    tmux: &dyn Tmux,
    session: &str,
    organization: &str,
    pane: &str,
    person: &str,
) -> bool {
    effects::activate_sleeping_focus(tmux, session, organization, pane, person)
}

/// Why a rail could not be built for a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RailRefusal {
    /// The session is not this company's operator session.
    #[error(
        "the sidebar exists only in a company session: '{session}' does not end in '{expected}', \
         so it is not the session of the company in this directory. It reads the company with \
         the operator credential, whose scope is unconditional, so it must never draw where the \
         viewer is not the operator."
    )]
    NotACompanySession {
        /// The session the rail was asked to draw in.
        session: String,
        /// The ending it would have accepted: `-<key6>_`.
        expected: String,
    },
}

/// Accept this session for the company in `dir`, or refuse and say why.
///
/// The whole condition, in one place.
///
/// # It matches the KEY, and no longer the whole name
///
/// A company session is `org-<slug>-<key6>_`
/// ([`crate::placement::session_name_for`]). The KEY half is this directory's
/// hash — a fact this process can compute from where it is standing — while the
/// SLUG half lives in the store, which the rail deliberately does not read: a
/// thin client that opened a company round trip to check a session name would
/// give back the boot this whole stage exists to win.
///
/// So the fence asks the identity question and only that one: *is this session
/// the one belonging to the company in my directory?* The key is the company's
/// identity, so matching on it is exactly as strong as matching the full name
/// was, and it is strictly stronger than matching the slug — two directories
/// may hold companies with the same slug, and the old comparison would have
/// accepted either one's session for the other.
///
/// The terminator still does the work it was chosen for: `_` cannot appear in a
/// slug ([`crate::placement::SESSION_TERMINATOR`]), so `-<key6>_` can only ever
/// match at the END of a company session name and never inside a slug that
/// happens to contain hex.
///
/// # Errors
/// [`RailRefusal::NotACompanySession`] for any other session.
pub fn for_session(session: &str, dir: &std::path::Path) -> Result<(), RailRefusal> {
    let key = company_key(dir);
    if crate::placement::session_belongs_to(session, &key) {
        Ok(())
    } else {
        Err(RailRefusal::NotACompanySession {
            session: session.to_owned(),
            expected: crate::placement::session_key_suffix(&key),
        })
    }
}

/// What the ROOT department is called in the rail.
///
/// The root department's stored `name` is the COMPANY's name — genesis writes
/// the company name into it (`organization_spec.rs`, the `DepartmentRecord` for
/// `ROOT_DEPARTMENT_ID`). Drawn raw, the Departments list therefore opened with
/// the company name pretending to be a department, which is what the operator
/// reported: a first row reading "Tribes Capital" where "Executive" belongs.
///
/// The company name is not lost — it is the rail pane's own border title
/// ([`rail_border_format`]), where it names the whole surface once instead of
/// impersonating one of its rows. The root department's id is `executive` and
/// that is what this row says.
pub const ROOT_DEPARTMENT_DISPLAY_NAME: &str = "Executive";

/// The one role label the sidebar derives from organization identity.
///
/// The CEO is the head of the root department. Existing companies can carry
/// an old or abbreviated durable title for that person, but the product role
/// is invariant. Every other person keeps the exact title from the roster.
pub const CEO_DISPLAY_ROLE: &str = "Chief Executive Officer";
/// The explicit product role when the roster has no usable job title.
pub const TEAM_MEMBER_DISPLAY_ROLE: &str = "Team member";

/// The first human name Chief shows for a roster person.
#[must_use]
pub fn person_first_name(display_name: &str) -> String {
    display_name
        .split_whitespace()
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("Person")
        .to_owned()
}

/// The short user-facing identity for a canonical first name.
#[must_use]
pub fn person_short_identity(display_name: &str) -> String {
    let first = person_first_name(display_name);
    let slug: String = first
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric() || *character == '_' || *character == '-')
        .collect();
    format!("@{}", if slug.is_empty() { "person" } else { slug.as_str() })
}

/// The role Chief shows for one roster person.
#[must_use]
pub fn person_display_role(display_name: &str, title: &str, is_ceo: bool) -> String {
    if is_ceo {
        return CEO_DISPLAY_ROLE.to_owned();
    }
    let title = title.trim();
    let first = person_first_name(display_name);
    if title.is_empty()
        || title.eq_ignore_ascii_case(display_name.trim())
        || title.eq_ignore_ascii_case(&first)
    {
        TEAM_MEMBER_DISPLAY_ROLE.to_owned()
    } else {
        title.to_owned()
    }
}

/// The rail's own border colours: black ground, white text.
///
/// Fixed rather than computed, because the rail is not a person and has no
/// identity accent to take a colour from. The operator asked for exactly this.
pub const RAIL_BORDER_BACKGROUND: &str = "black";
/// The rail border's text colour. See [`RAIL_BORDER_BACKGROUND`].
pub const RAIL_BORDER_FOREGROUND: &str = "white";

/// The text colour used on a light ground.
///
/// A truecolor value is required. Tmux's named `black` resolves through the
/// terminal palette; on the browser terminal it is `#2e3436`, not black.
pub const CONTRAST_ON_LIGHT: &str = "#000000";
/// The text colour used on a dark ground. See [`CONTRAST_ON_LIGHT`].
pub const CONTRAST_ON_DARK: &str = "#ffffff";

/// Identity chips keep the allocated hue but use a dark enough ground for
/// white text. The curated palette sits near L=0.202: named terminal black is
/// only 3.04:1 on the screenshot's `#6977c5`, while white is only 4.16:1.
/// L=0.16 gives white at least 5:1 and leaves rounding room above AA.
const CHIP_BACKGROUND_LUMINANCE: f64 = 0.16;
const RAW_IDENTITY_LUMINANCE_MIN: f64 = 0.19;
const RAW_IDENTITY_LUMINANCE_MAX: f64 = 0.21;

/// The chip ground for a person who HAS no identity accent.
///
/// Reachable now only when chiefd's palette and its hue rotations are
/// exhausted. It used to be the CEO's and the operator's ordinary state — a
/// standard identity carried no generated theme and therefore no accent — and
/// that split is deleted: chief generates no theme for anybody, so there is
/// nothing left for those two to be an exception to, and they are allocated a
/// colour like everyone else.
///
/// The CHIP is the one place "absent is not a colour" produced a worse answer
/// than a choice.
/// Left unfilled, the title inherited the terminal's own border style and the
/// operator got white-on-grey — unreadable, and reported as such. A chip is a
/// FILLED shape or it is not a chip. So a person with no accent gets an
/// explicit no-accent ground rather than no ground, spelled as a hex precisely
/// so [`contrast_foreground`] measures it like any other and picks the ink by
/// the same rule — black ground, white ink, which is also what the rail's own
/// title uses.
pub const NO_ACCENT_BACKGROUND: &str = "#000000";

/// Is this a `#rrggbb` colour — a ground [`contrast_foreground`] can measure?
///
/// A person's identity accent is one. An unset `@accent`, a tmux colour NAME
/// and a malformed hex are not, and each of those is a pane with no accent
/// rather than a pane whose accent is black.
#[must_use]
pub fn is_hex_colour(candidate: &str) -> bool {
    candidate
        .strip_prefix('#')
        .is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn color_channels(color: &str) -> Option<[u8; 3]> {
    if !is_hex_colour(color) {
        return None;
    }
    let hex = &color[1..];
    Some([
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ])
}

fn relative_luminance(background: &str) -> Option<f64> {
    let channels = color_channels(background)?;
    let linear = |raw: u8| {
        let scaled = f64::from(raw) / 255.0;
        if scaled <= 0.040_45 {
            scaled / 12.92
        } else {
            ((scaled + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * linear(channels[0]) + 0.7152 * linear(channels[1]) + 0.0722 * linear(channels[2]))
}

/// Keep one identity accent's channel proportions while making its chip a
/// safe ground for white text. This is computed from the final RGB value, so
/// curated and hue-wrapped accents follow one rule.
fn person_chip_background(accent: &str) -> String {
    let Some(channels) = color_channels(accent) else {
        return NO_ACCENT_BACKGROUND.to_owned();
    };
    let luminance = relative_luminance(accent).unwrap_or_default();
    if !(RAW_IDENTITY_LUMINANCE_MIN..=RAW_IDENTITY_LUMINANCE_MAX).contains(&luminance)
        || luminance <= CHIP_BACKGROUND_LUMINANCE
    {
        return accent.to_ascii_lowercase();
    }
    let mut low = 0.0;
    let mut high = 1.0;
    let scaled = |factor: f64| {
        channels.map(|channel| (f64::from(channel) * factor).round().clamp(0.0, 255.0) as u8)
    };
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        let value = scaled(middle);
        let candidate = format!("#{:02x}{:02x}{:02x}", value[0], value[1], value[2]);
        if relative_luminance(&candidate).is_some_and(|value| value <= CHIP_BACKGROUND_LUMINANCE) {
            low = middle;
        } else {
            high = middle;
        }
    }
    let value = scaled(low);
    format!("#{:02x}{:02x}{:02x}", value[0], value[1], value[2])
}

/// Pick a foreground that READS on `background`.
///
/// COMPUTED, never a table. The operator's rule was "not a yellow background
/// with white text — it should be black text", and the way to hold that for a
/// colour nobody has allocated yet is to measure the ground rather than to
/// enumerate the grounds. The two WCAG contrast ratios are compared directly:
/// `(L + 0.05) / 0.05` for black and `1.05 / (L + 0.05)` for white. Their
/// equal point is near `L = 0.179`; the higher-ratio foreground wins.
///
/// Anything [`is_hex_colour`] refuses is not a ground this can measure, so it
/// answers [`CONTRAST_ON_DARK`]: white on the terminal's default ground, which
/// is the state the rail was in before any of this existed.
#[must_use]
pub fn contrast_foreground(background: &str) -> &'static str {
    let Some(luminance) = relative_luminance(background) else {
        return CONTRAST_ON_DARK;
    };
    let black_contrast = (luminance + 0.05) / 0.05;
    let white_contrast = 1.05 / (luminance + 0.05);
    if black_contrast >= white_contrast {
        CONTRAST_ON_LIGHT
    } else {
        CONTRAST_ON_DARK
    }
}

/// The pane-border title for a PERSON's pane: short identity and real role.
///
/// The operator's words: "just show the agent role… It should not show the pi
/// or company or anything like that." What they were shown instead —
/// `pi · π - Tribes Capital · CEO - workspace` — is the pane TITLE, which the
/// program inside the pane sets and which nobody here controls. So the rail
/// stops rendering `#{pane_title}` at all and renders a string it composes
/// from the person's canonical `@first-name` identity and roster role.
///
/// The title is a FILLED CHIP: the ground is that person's identity accent, as
/// chiefd's own allocator published it on the launch catalog, and the ink is
/// whatever READS on it ([`contrast_foreground`]). The style is
/// inlined on the title span, which is the whole point: `pane-border-style`
/// colours the LINE and leaves the text on the default, which is the plain-grey
/// title the operator reported after the first cut of this shipped.
#[must_use]
pub fn person_border_format(display_name: &str, role: &str, accent: &str) -> String {
    let background = person_chip_background(accent);
    let identity = tmux_static(&person_short_identity(display_name));
    let role = tmux_static(role);
    // From the RESOLVED ground, never from the raw accent. Reading the ink off
    // an accent that was then discarded is how a chip ends up with a colour
    // picked for a background it is not drawn on.
    format!(
        "#[fg={},bg={}] {identity} · {role} #[default]",
        contrast_foreground(&background),
        background
    )
}

/// Escape literal text before tmux parses it as a format string.
#[must_use]
pub fn tmux_static(value: &str) -> String {
    value.replace('#', "##")
}

// TOMBSTONE: `accent_from_theme`, deleted 2026-08-16 with the generated theme
// files it parsed. It opened `pi-home/themes/organization-<id>*.json` and read
// `vars.accent` back out, because the launch catalog carried the theme PATHS
// and the colour existed only inside them. Chief writes no theme file now, so
// chiefd publishes the allocator's answer directly on the catalog
// (`LaunchEntry::accent`) and the rail reads a hex instead of a file. The
// earlier note about `#{@accent}` stays true and stays the reason not to reach
// for the pane: `@accent` was a pane option of the retired TypeScript launcher
// and this tree never sets one.

/// The pane-border title for the RAIL's own pane: the company name, white on
/// black.
#[must_use]
pub fn rail_border_format(company: &str) -> String {
    format!("#[fg={RAIL_BORDER_FOREGROUND},bg={RAIL_BORDER_BACKGROUND}] {company} #[default]")
}

/// The GLOBAL `pane-border-format`, paired with every site that turns
/// `pane-border-status` on.
///
/// # Why a global default exists at all
///
/// tmux's own default is `#{pane_index} "#{pane_title}"`, and `pane_title` for
/// a pane nothing has titled is **the machine's hostname**. So enabling the
/// border globally makes every not-yet-titled pane show the host until
/// something titles it — which was a persistent leak for department cards
/// (fixed by titling them) and remains a TRANSIENT one for any pane in the
/// window between its mint and its first title pass. Measured on a live
/// company: after the cards were titled, a department window's rail still
/// flashed `0 "<hostname>"` for the instant before the refresh reached it.
///
/// Titling each pane kind fixes the pane kinds we know about. This fixes the
/// ones we do not: a pane that is new, stray, or simply early shows the
/// company's own word instead of the operator's machine. **The enabling site
/// owns the fallback** — turning a chrome on globally without saying what it
/// says by default is what made a hostname the default.
///
/// It deliberately renders nothing rather than a name: an untitled pane has
/// no honest title to give, and a blank border is the truthful answer where
/// tmux's default is an accidental one.
pub const SAFE_BORDER_DEFAULT: &str = "";

/// The pane-border title for a DEPARTMENT CARD's pane: the department name,
/// styled like the rail's.
///
/// # Why this exists, and what was on screen before it
///
/// `pane-border-status` is turned on GLOBALLY — that is deliberate, and the
/// rail and every person pane are given a format of their own. The department
/// card was not, and tmux's default `pane-border-format` is
/// `#{pane_index} "#{pane_title}"`, whose `pane_title` for a pane nothing has
/// titled is **the machine's hostname**.
///
/// So clicking a department — the product's central gesture — drew the
/// operator's hostname above the card, on every box, in every department
/// window. Measured on a live company while capturing the README asset: five
/// untitled panes, every one of them reading `1 "<hostname>"`. It is
/// unrequested chrome that leaks the name of the machine, and a README
/// recording would have published one.
///
/// **`#{window_name}` rather than a threaded string**, deliberately: chief
/// already names a department's window after the department, so reading it
/// back cannot drift from it, and no caller has to carry a display name it
/// does not otherwise hold. The rail's format interpolates because a rail pane
/// has no window whose name is the company.
#[must_use]
pub fn department_border_format() -> String {
    format!(
        "#[fg={RAIL_BORDER_FOREGROUND},bg={RAIL_BORDER_BACKGROUND}] #{{window_name}} #[default]"
    )
}

/// One department row, as the rail draws it.
///
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DepartmentRow {
    /// The department's logical id — also its tmux window tag.
    pub id: String,
    /// Display name, raw.
    pub name: String,
    /// Depth in the tree; the row is indented by it.
    pub depth: usize,
    /// How many of its people have a live pane right now.
    pub live: usize,
    /// How many operational roster people belong to this department.
    pub total: usize,
}

/// What a person is doing, as far as anything can actually be known.
///
/// # The three facts, and which authority owns each
///
/// * `live` — TMUX's fact. A pane carries their `@organization_person_id` and
///   is not dead. chiefd never learns this ("Nothing goes up").
/// * `desired` — CHIEFD's fact, from `/v1/org/runtime/desired`: should this
///   person be running at all.
/// * `idle` — CHIEFD's fact, from `idleSince` on the lifecycle board: is the
///   settle clock RUNNING.
///
/// # Why a running clock is positive evidence, not an absence
///
/// `agent_quiet_since` has three states — never beaten, BEATING, went quiet —
/// and the clock is STOPPED BY the beat, which fires on `message_update`,
/// `message_end` and `tool_execution_start`/`update`/`end`. So a clock that is
/// running is a report that the person went quiet, and no clock on a live pane
/// means the model is emitting or a tool is in flight. Neither is inferred from
/// silence.
/// SERIALIZED because the department card is a separate process that draws
/// these states, and the alternative was a second state vocabulary living in
/// that card and drifting from this one. There is exactly one definition of
/// what a person is doing in this product and this is it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PersonState {
    /// A live pane and no settle clock: emitting, or a tool is in flight.
    Working,
    /// A live pane and the settle clock running: quiet, spending the lease down.
    Idle,
    /// chiefd wants them up and tmux has no pane for them yet.
    Starting,
    /// chiefd wants them up and chiefd's OWN LAUNCH GATE has declined them.
    ///
    /// # Why this is not `Starting`, and not `Sleeping` either
    ///
    /// `Starting` is a promise that the actuator is on its way. A person the
    /// gate has refused is one nobody is going to try: the refusal is
    /// re-derived by the daemon on every pass, from the disk it alone can see,
    /// and it does not clear because time passed. The rail said `starting`
    /// about them anyway, on every pass, for ever.
    ///
    /// `Sleeping` would be the opposite lie. The person is WANTED — they are in
    /// chiefd's desired set and will start the moment their refusal is fixed —
    /// and `sleeping` says the operator parked them, which invites exactly the
    /// wrong repair.
    ///
    /// So refused is its own cell: wanted, blocked, and carrying the gate's own
    /// sentence about what is wrong ([`PersonRow::refused`]).
    Refused,
    /// chiefd wants them up, the actuator keeps starting them, and they keep
    /// dying.
    ///
    /// # Why this is not `Starting`
    ///
    /// `Starting` is a promise that is about to be kept: chiefd wants this
    /// person, the actuator is on its way, and the pane is seconds out. A
    /// person whose boot has died eleven times in the last four minutes is not
    /// that, and the row went on saying so for ever. A live company sat with
    /// thirteen people marked `starting` for twenty minutes while the actuator
    /// was respawning them once a second: the rail was reporting a state that
    /// never advanced, which is the worst of both worlds — no diagnosis and no
    /// visible progress.
    ///
    /// # Why this is not a dead end either
    ///
    /// The cell this replaces was `Held`, and it meant the actuator had GIVEN
    /// UP after five failures. Nothing gives up any more (`crash_loop`): the
    /// person is retried on a backoff capped at ten seconds, for as long as
    /// chiefd wants them. So the row does not ask the operator to do anything.
    /// It tells them what is happening — the retry number, how long it has been
    /// going on, and what went wrong — so they can decide whether to.
    Crashing,
    /// chiefd does not want them up, and no pane carries them. Parked.
    Sleeping,
}

impl PersonState {
    /// The state tag used in notices and diagnostics.
    ///
    /// Every state this product can actually establish has one. There is no
    /// "unknown" tag because there is no unknown state left: the three inputs
    /// are total, and a person is in exactly one cell of them.
    /// Lowercase, because the operator asked for it and because the fallback
    /// they offered — a smaller font — does not exist: a terminal cell is one
    /// size, which is a property of the terminal and not something a rail can
    /// change. Lowercase is the whole of the available answer.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Starting => "starting",
            Self::Refused => "refused",
            Self::Crashing => "crashing",
            Self::Sleeping => "sleeping",
        }
    }

    /// Whether this person can be focused — i.e. whether they have a pane.
    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Working | Self::Idle)
    }
}

/// What the rail says about a person whose boot keeps dying.
///
/// Pre-FORMATTED, deliberately. The rail is pure and serializable; a `Duration`
/// here would make every renderer re-decide how to write "1m 30s", and the two
/// answers would drift. `crash_loop` owns the numbers and `crash_loop`'s own
/// `human_duration` owns how they read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CrashNotice {
    /// How many consecutive boots have failed. The operator's "retry number".
    pub failures: u32,
    /// How long this run of failures has been going on, already written out.
    pub elapsed: String,
    /// How long until the next attempt, already written out.
    pub retry_in: String,
    /// One or two sentences about what went wrong, when the actuator learned
    /// any; `None` when the only fact is that the pane did not survive.
    pub last_error: Option<String>,
}

impl CrashNotice {
    /// The line the operator reads: the retry number, how long, and why.
    #[must_use]
    pub fn sentence(&self) -> String {
        let error = self
            .last_error
            .as_deref()
            .map_or_else(|| "their pane did not survive the pass".to_owned(), ToOwned::to_owned);
        format!(
            "crashing · retry {} · failing for {} · next attempt in {} · {error}",
            self.failures, self.elapsed, self.retry_in
        )
    }
}

/// One person row, as the rail draws it.
///
/// It carries the three INPUTS rather than a computed state, so
/// [`View::set_live`] can re-read tmux on a click and the state follows without
/// a second copy of the precedence rule existing anywhere.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersonRow {
    /// The person's id — also their `@organization_person_id` pane tag.
    pub id: String,
    /// Display name, raw.
    pub name: String,
    /// Human-readable job title from the durable roster.
    pub title: String,
    /// Whether a live pane carries this person right now. TMUX's fact.
    pub live: bool,
    /// Whether chiefd wants this person running. CHIEFD's fact.
    pub desired: bool,
    /// Whether this person's settle clock is running. CHIEFD's fact.
    pub idle: bool,
    /// This person's live crash report, when their boot keeps dying; `None`
    /// when it does not.
    ///
    /// THE ACTUATOR's fact, and the only one of the four chiefd cannot supply —
    /// it holds the desired state and never learns what happened on this box.
    pub crash: Option<CrashNotice>,
    /// CHIEFD's own reason for declining to launch this person, when its launch
    /// gate has declined them; `None` when it has not.
    ///
    /// The gate's SENTENCE, not a flag and not a rewrite of one. chiefd is the
    /// only process that can see the disk a refusal is about, so it is the only
    /// process that can say which two files are missing from whose home — and
    /// summarizing that into "blocked" would throw away the only part the
    /// operator can act on. `wake_refused_notice` carries a daemon refusal the
    /// same way, for the same reason.
    pub refused: Option<String>,
    /// Whether this person HEADS the department this row is listed under.
    ///
    /// Per-ROW and not per-person, because it is a fact about the pairing: the
    /// same person is a manager on their own department's list and would be a
    /// plain member anywhere else they appeared. A department has at most one.
    pub manager: bool,
}

impl PersonRow {
    /// This person's state, and the ONE place the precedence is decided.
    ///
    /// **Liveness wins.** A pane that exists settles WORKING vs IDLE and
    /// nothing else is consulted; only a person with no pane can be STARTING or
    /// SLEEPING. Two consequences, both deliberate and neither incidental:
    ///
    /// 1. **A pane that died mid-turn never reads WORKING.** It holds no settle
    ///    clock for up to the 300s activity-liveness window, so a state derived
    ///    from the clock alone would call a corpse busy for five minutes. A dead
    ///    pane is not a live pane (`pane_dead` is filtered at the source), so it
    ///    falls to STARTING while chiefd still wants them — which is true, and
    ///    is what the actuator is about to act on.
    /// 2. **A person who has just launched and emitted nothing reads WORKING,
    ///    not STARTING.** They have never beaten, so they hold no clock either.
    ///    STARTING means "no pane yet"; once the pane exists the person is up,
    ///    and booting IS working. Stating this here is the point — the two
    ///    no-clock cases are told apart by the pane, not by the clock.
    #[must_use]
    pub const fn state(&self) -> PersonState {
        if self.live {
            if self.idle {
                PersonState::Idle
            } else {
                PersonState::Working
            }
        } else if self.desired {
            // NEITHER HELD NOR REFUSED IS STARTING, and this is the only place
            // that is decided. All three are "chiefd wants them and there is no
            // pane"; they differ in whether anything is still going to happen
            // about it, and that difference is the whole reason the operator is
            // looking at the row.
            //
            // REFUSED BEATS CRASHING. A crash report is this actuator's record
            // of boots it attempted; a refusal is chiefd declining to publish a
            // launch spec at all, re-derived this very pass against the disk.
            // When both are true the refusal is the live cause — no boot is
            // being attempted for a refused person, so the crash report is a
            // record of an older question — and it is the one that names a
            // repair.
            if self.refused.is_some() {
                PersonState::Refused
            } else if self.crash.is_some() {
                PersonState::Crashing
            } else {
                PersonState::Starting
            }
        } else {
            PersonState::Sleeping
        }
    }
}

/// Everything the rail knows, and where the operator has scrolled it.
///
/// Pure. It holds no pane ids and no tmux handle: a pane id is resolved from
/// the live `@organization_person_id` tag at CLICK time, never cached, because
/// a cached pane id is a second source of truth for placement and is stale the
/// moment a person is moved between windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The company's display name. Drawn on the rail pane's own border, never
    /// as a department row — see [`ROOT_DEPARTMENT_DISPLAY_NAME`].
    company: String,
    departments: Vec<DepartmentRow>,
    /// People by department id, in the company's canonical person order.
    people: BTreeMap<String, Vec<PersonRow>>,
    selected: Option<String>,
    /// The person the operator last clicked, if any.
    ///
    /// Cleared whenever a DEPARTMENT is selected: the operator ruled that
    /// picking a department puts the selection on the department row and
    /// leaves no person selected, so that two rows are never marked
    /// at once and the marker always answers "what did I last choose".
    selected_person: Option<String>,
    /// Department nodes whose people are visible as child rows.
    expanded: BTreeSet<String>,
    department_scroll: usize,
    collapsed: bool,
    /// Whether the company has EVER been read into this view.
    ///
    /// False only between [`View::unread`] and the first [`View::refresh`],
    /// which is the rail's boot. It exists so the renderer can tell "nobody
    /// works here" from "I do not know yet who works here" — see
    /// [`View::unread`] for the operator report that made the difference
    /// visible.
    read: bool,
    /// Whether the last attempt to read the company FAILED.
    ///
    /// "I have not read it yet" and "I tried and could not" are different
    /// facts, and a rail that cannot tell them apart draws the boot placeholder
    /// for ever — which is the defect this exists to end. See
    /// [`View::note_unreadable`].
    unreadable: bool,
}

/// What a click did, for the caller to carry out against tmux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing there. A click on a title or a row past the end of the tree.
    ///
    /// **Not a person who is down.** Every person is drawn now, and a click on
    /// one who is not up is [`Action::FocusPerson`] — the rail turns it into a
    /// wake. Refusing here was the silence the operator read as broken.
    Ignored,
    /// Show this department: expand its people and show its window.
    ///
    /// **It moves the glass, and it moves no pane.** Selecting a department
    /// expands the department node AND switches to that department's window,
    /// where converge has already placed everybody alive in it as an equal grid
    /// beside a rail.
    ///
    /// This branch spent a while issuing nothing at all. The first version
    /// answered the click with `select-window -t <session>:<department_id>`,
    /// which failed loudly (`can't find window: executive`) for every
    /// department whose window is named anything but its raw id, and the
    /// retreat was to rule the click a pure filter. The BY-NAME lookup was the
    /// defect; the window switch is what the operator asked for, and
    /// `effects::show_department` resolves the window by its
    /// `@organization_window_id` tag.
    SelectDepartment(String),
    /// Toggle only this department's explicit `+`/`−` disclosure control.
    ToggleDepartmentDisclosure(String),
    /// Put this person's pane on the glass ALONE — a tmux zoom, rail included.
    ///
    /// It was "full screen BESIDE the rail", laid as a focused layout with
    /// every bystander held at 24 columns, because a layout string cannot hide
    /// a pane. The operator retired that compromise: 24 columns of somebody
    /// else beside the person you clicked is what they reported as "the person
    /// I selected gets merged with the CEO". Its successor, `resize-pane -Z`,
    /// was retired in turn for hiding the RAIL. A clicked person is now MOVED
    /// into a window of their own beside a rail of their own, and clicking a
    /// department moves them back ([`effects::show_person`]).
    FocusPerson {
        /// The owning department disclosed above this person row.
        department_id: String,
        /// The person whose existing focus route must open.
        person_id: String,
    },
    /// Collapse or expand the rail.
    ToggleCollapsed,
}

impl View {
    /// Build a view over a company's departments and their people.
    #[must_use]
    pub fn new(departments: Vec<DepartmentRow>, people: BTreeMap<String, Vec<PersonRow>>) -> Self {
        let selected = departments.first().map(|row| row.id.clone());
        let expanded = selected.iter().cloned().collect();
        Self {
            company: String::new(),
            departments,
            people,
            selected,
            selected_person: None,
            expanded,
            department_scroll: 0,
            collapsed: false,
            read: true,
            unreadable: false,
        }
    }

    /// The view a rail has before it has read the company even once.
    ///
    /// # Why this is not just `new(vec![], BTreeMap::new())`
    ///
    /// Because that view LIES, and the operator caught it. An empty roster
    /// drawn through the ordinary path renders "Nobody works here" under an
    /// empty Departments heading — which is a statement about the company, and
    /// during boot it is a false one. They reported it as "why does the
    /// department list disappear?", which is exactly right: the rail was
    /// asserting an emptiness it had no evidence for.
    ///
    /// "I have not read the company" and "I read it and it is empty" are
    /// different facts and must not render the same. This one draws the rail's
    /// chrome and says it is still coming.
    #[must_use]
    pub fn unread() -> Self {
        Self { read: false, ..Self::new(Vec::new(), BTreeMap::new()) }
    }

    /// Whether this view has ever been filled from the company.
    #[must_use]
    pub const fn is_read(&self) -> bool {
        self.read
    }

    /// Record that an attempt to read the company FAILED.
    ///
    /// # Why a rail must say this rather than keep waiting
    ///
    /// [`View::unread`] draws `…` — "still coming" — and that is honest for the
    /// moment between a rail's birth and its first answer. It stops being
    /// honest the instant a read has been tried and refused, because nothing is
    /// coming: the rail will try again, but what is on the glass is no longer a
    /// wait, it is a failure. A rail whose every read fails drew `…` for ever,
    /// so the operator's screen said "loading" about a company nobody could
    /// read, indefinitely and with no way to tell the two apart.
    ///
    /// This does NOT make the view read. An unread company stays unread, so the
    /// honesty rule holds unchanged: a rail that has never read the company
    /// still never claims "Nobody works here". It changes only WHICH true thing
    /// is drawn while the company is unknown.
    pub const fn note_unreadable(&mut self) {
        self.unreadable = true;
    }

    /// Whether the last read attempt failed. Only ever consulted for a view
    /// that is not [`View::is_read`]; once the company has been read, a later
    /// failure leaves the last good frame on the glass rather than replacing it
    /// with a notice.
    #[must_use]
    pub const fn is_unreadable(&self) -> bool {
        self.unreadable
    }

    /// The company's display name, for the rail's own pane border.
    #[must_use]
    pub fn company(&self) -> &str {
        &self.company
    }

    /// The department whose people are shown.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The person the operator last clicked, if the selection is on a person.
    #[must_use]
    pub fn selected_person(&self) -> Option<&str> {
        self.selected_person.as_deref()
    }

    /// Put the selection on a person. Clicking a person is the only thing that
    /// does this.
    pub fn select_person(&mut self, person_id: &str) {
        self.selected_person = Some(person_id.to_owned());
    }

    /// Say STARTING for somebody whose wake chiefd has just granted.
    ///
    /// # The second and third click this stops
    ///
    /// `desired` is chiefd's fact and the rail only re-reads it on a changefeed
    /// wake, so a granted wake took a round trip — measured at about three
    /// seconds on the operator's own box — before the row stopped saying
    /// `sleeping`. The operator clicked a sleeper, watched the row not change,
    /// and clicked again. Every one of those extra clicks is another POST and
    /// another spawn: nine clicks in three and a half seconds appear in the log,
    /// and five consecutive failed boots is exactly what makes the actuator give
    /// up on somebody for good.
    ///
    /// So the grant is applied to the row that asked for it, at once. This is
    /// not the rail inventing a fact: chiefd answered `Ok` to
    /// `wake_person`, which IS the desired set changing, and the very next
    /// [`View::refresh`] overwrites this from chiefd's own answer. It can be
    /// wrong for at most one pass, in the direction chiefd has already agreed
    /// to.
    ///
    /// It does NOT touch `live` — that is tmux's fact, the pane does not exist
    /// yet, and claiming it would make the row read `working` for somebody who
    /// has not started.
    pub fn mark_starting(&mut self, person_id: &str) {
        for rows in self.people.values_mut() {
            for row in rows.iter_mut().filter(|row| row.id == person_id) {
                row.desired = true;
            }
        }
    }

    /// Is the rail collapsed to its stub?
    #[must_use]
    pub const fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Every department row, in canonical order.
    #[must_use]
    pub fn departments(&self) -> &[DepartmentRow] {
        &self.departments
    }

    /// Everybody, by department id — not just the selected department's people.
    ///
    /// The whole company rather than one list, for the tests that assert what
    /// a company read left in the view regardless of which department the
    /// operator happens to be on.
    #[must_use]
    pub const fn everybody(&self) -> &BTreeMap<String, Vec<PersonRow>> {
        &self.people
    }

    /// EVERY person of the selected department, in canonical order.
    ///
    /// It used to be the live ones only, with the rest drawn dimmed beneath and
    /// unclickable. The operator ruled that out: a company's sleeping people are
    /// the ones you most need to see and act on, and a rail that hides them
    /// cannot be used to wake anybody. Each row now carries its own
    /// [`PersonState`], so "nobody works here" and "they are all asleep" are
    /// told apart by what is written on the row rather than by which list it
    /// landed in.
    #[must_use]
    pub fn people(&self) -> Vec<&PersonRow> {
        self.selected
            .as_ref()
            .and_then(|id| self.people.get(id))
            .map(|rows| rows.iter().collect())
            .unwrap_or_default()
    }

    /// Re-apply TMUX's liveness to the rows already drawn.
    ///
    /// # Why this exists at all
    ///
    /// **Liveness is tmux's fact, and chiefd never learns it** — `resident.rs`'s
    /// "Nothing goes up" means no report of a started or dead pane ever travels
    /// back. The rail's only wake is chiefd's changefeed, so **nothing wakes the
    /// rail when a pane dies.** A person could therefore sit in the People
    /// section, drawn live and clickable, indefinitely after their pane was
    /// gone — and clicking them did nothing at all, silently, forever.
    /// That is the defect the operator reported as "it kicks me back to CEO":
    /// the click resolved to no pane, `show_person` returned without moving
    /// anything, and the screen stayed on whatever tmux already had. Three
    /// clicks, three `sidebar.focus.unresolved` warnings, no change on the
    /// glass.
    ///
    /// A full [`View::refresh`] cannot be used here: it reads chiefd over the
    /// network and is `async`, and the mouse handler is neither. This re-reads
    /// the ONE fact that can have changed without anybody being told — who has
    /// a live pane — and leaves chiefd's structure alone.
    pub fn set_live(&mut self, live: &std::collections::BTreeSet<String>) {
        for rows in self.people.values_mut() {
            for row in rows.iter_mut() {
                row.live = live.contains(&row.id);
            }
        }
        for dept in &mut self.departments {
            dept.live = self
                .people
                .get(&dept.id)
                .map_or(0, |rows| rows.iter().filter(|row| row.live).count());
        }
        let ceiling = self.tree_rows().len().saturating_sub(1);
        self.department_scroll = self.department_scroll.min(ceiling);
    }

    /// Select and expand a department without moving the operator's scroll.
    ///
    /// Selection and scrolling are separate gestures. A department row click
    /// changes the destination and disclosure state, while only wheel input
    /// changes which tree line is at the top of the rail.
    pub fn select(&mut self, department_id: &str) {
        if self.departments.iter().any(|row| row.id == department_id) {
            self.selected = Some(department_id.to_owned());
            self.expanded.insert(department_id.to_owned());
            // The selection moves to the department row, so no person carries
            // it. Two marked rows would leave the operator unable to
            // tell which one their next action applies to.
            self.selected_person = None;
        }
    }

    /// Toggle only the explicit disclosure control for this department.
    ///
    /// A row click does not enter here. Repeated row clicks keep the department
    /// open; only the `+`/`−` cell changes disclosure.
    pub fn toggle_department_disclosure(&mut self, department_id: &str) {
        if !self.departments.iter().any(|row| row.id == department_id) {
            return;
        }
        if !self.expanded.remove(department_id) {
            self.expanded.insert(department_id.to_owned());
        }
    }

    /// Replace the facts, keeping the operator's place where it still exists.
    ///
    /// **SELECTION IS OPERATOR STATE AND SURVIVES EVERY REFRESH.** This runs on
    /// every changefeed wake, and a wake may change who is alive, what state
    /// they are in and who exists — it must never move the operator's cursor. A
    /// rail that loses your place once a second is unusable however well it
    /// lays panes out.
    ///
    /// The ONLY thing that drops a selection here is the selected department
    /// ceasing to exist, and that is LOGGED (`sidebar.selection.reset`) rather
    /// than defaulted silently — a cursor that moves for a reason nobody can
    /// grep for is indistinguishable from a cursor that moves at random.
    ///
    /// The fallback, when the selected department has genuinely left the tree,
    /// is the first row. There is one selection in the session now, so there is
    /// no per-window "home" to fall back to and no window whose rail could
    /// disagree with another about where the operator is.
    ///
    /// Both scroll offsets are re-clamped, because the lists they indexed have
    /// changed length.
    pub fn refresh(
        &mut self,
        company: String,
        departments: Vec<DepartmentRow>,
        people: BTreeMap<String, Vec<PersonRow>>,
    ) {
        self.company = company;
        // The company has now been read, whatever it turned out to contain. An
        // empty roster from here IS "nobody works here"; before here it was
        // "not yet known".
        self.read = true;
        // Whatever could not be read a moment ago has been read now.
        self.unreadable = false;
        let had = self.selected.take();
        let exists = |id: &String| departments.iter().any(|row| &row.id == id);
        let kept = had.clone().filter(&exists);
        if let (None, Some(gone)) = (&kept, &had) {
            tracing::info!(
                event = "sidebar.selection.reset",
                department = %gone,
                "the selected department left the tree; the rail falls back rather than \
                 showing an empty list forever"
            );
        }
        let fell_back = kept.is_none();
        let kept = kept.or_else(|| departments.first().map(|row| row.id.clone()));
        self.departments = departments;
        self.people = people;
        self.selected = kept;
        self.expanded.retain(|id| self.departments.iter().any(|row| &row.id == id));
        if fell_back {
            if let Some(selected) = &self.selected {
                self.expanded.insert(selected.clone());
            }
        }
        self.department_scroll =
            self.department_scroll.min(self.tree_rows().len().saturating_sub(1));
    }

    /// Scroll the unified tree by `delta` lines, clamped at both ends.
    pub fn scroll(&mut self, delta: isize) {
        let len = self.tree_rows().len();
        let offset = &mut self.department_scroll;
        let ceiling = len.saturating_sub(1);
        *offset = offset.saturating_add_signed(delta).min(ceiling);
    }

    /// Where the unified tree starts.
    #[must_use]
    pub const fn scroll_offset(&self) -> usize {
        self.department_scroll
    }

    /// Collapse or expand.
    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    /// Restore the session-local collapse preference when the brain starts.
    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.collapsed = collapsed;
    }
}

/// One visible line in the unified company tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeRow<'a> {
    /// One empty, non-interactive line immediately before a department.
    DepartmentSpacer(&'a DepartmentRow),
    /// A department disclosure and selection row.
    Department(&'a DepartmentRow),
    /// The first line of a person child row: identity, manager badge and state.
    Person(&'a DepartmentRow, &'a PersonRow),
    /// The second line of a person child row: durable human-readable role.
    Role(&'a DepartmentRow, &'a PersonRow),
}

/// Shared tree geometry. Render and hit-testing use the same columns.
pub(super) const TREE_GUTTER: usize = 1;

/// How many columns one level of department nesting indents a row.
///
/// The store nests departments (`parent_department_id`) and publishes the tree
/// in preorder; the rail reflects that tree by stepping every row of a
/// department — its disclosure, its label, and its people — this many columns
/// to the right per level of [`DepartmentRow::depth`]. Render and hit-testing
/// both read it, so the drawn disclosure cell and the cell a click toggles are
/// one column at every depth.
pub(super) const DEPARTMENT_INDENT_STEP: usize = 2;

/// How many columns a department at `depth` indents.
///
/// THE ROOT COSTS NO LEVEL (operator ruling, 2026-08-19). Depth 0 is the
/// executive root and depth 1 is a top-level department, and BOTH draw flush
/// left: every company has exactly one root, so spending a whole indentation
/// level on a fact that is true of every row buys no information and pushes the
/// whole tree right for nothing. Indentation starts where it says something --
/// at depth 2, a department INSIDE another department -- and each level below
/// that adds one more step.
///
/// So the drawn geometry is `depth - 1` steps, floored at zero, not `depth`
/// steps. `saturating_sub` is that floor: it is what makes depth 0 and depth 1
/// share a column instead of making the root a negative index.
#[must_use]
pub(super) const fn department_indent(depth: usize) -> usize {
    depth.saturating_sub(1) * DEPARTMENT_INDENT_STEP
}

/// The column of a department's disclosure cell, given its depth in the tree.
///
/// Depth-derived, not fixed: a sub-department's `+`/`−` sits beside its own
/// indented label rather than stranded in a shared left gutter, so the control
/// and the branch it opens read as one thing.
#[must_use]
pub(super) const fn department_disclosure_column(depth: usize) -> usize {
    TREE_GUTTER + department_indent(depth)
}

impl View {
    /// Flatten the company tree into the exact lines that draw and hit-test.
    #[must_use]
    pub fn tree_rows(&self) -> Vec<TreeRow<'_>> {
        let mut rows = Vec::new();
        for department in &self.departments {
            rows.push(TreeRow::DepartmentSpacer(department));
            rows.push(TreeRow::Department(department));
            if self.expanded.contains(&department.id) {
                for person in self.people.get(&department.id).into_iter().flatten() {
                    rows.push(TreeRow::Person(department, person));
                    rows.push(TreeRow::Role(department, person));
                }
            }
        }
        rows
    }

    /// Whether a department currently discloses its people.
    #[must_use]
    pub fn is_expanded(&self, department_id: &str) -> bool {
        self.expanded.contains(department_id)
    }
}

/// THE BINARY A FRESHLY MINTED RAIL RUNS, resolved once and honestly.
///
/// # The defect this exists for, measured on a live company
///
/// Linux answers `/proc/self/exe` for a binary that has been REPLACED on disk
/// with the original path plus a literal ` (deleted)` suffix — and
/// `std::env::current_exe()` hands that string straight back. Upgrading `chief`
/// while a company is attached is an ordinary thing to do (`bun run release`
/// overwrites it), and every rail already running then minted new rails as
/// `/path/to/chief (deleted)`, which cannot be executed. tmux created the pane,
/// the exec failed, the pane died before its tag landed — observed as
/// `set-option: no such pane: %104` — and the window was left with no sidebar
/// at all: one full-width pane where the rail should be.
///
/// So the suffix is stripped and the result is CHECKED. A path that does not
/// exist yields `None`, which mints no rail and says so, rather than a pane that
/// is born dead.
#[must_use]
pub fn rail_program() -> Option<String> {
    let raw = std::env::current_exe().ok()?;
    let text = raw.display().to_string();
    let path = text.strip_suffix(" (deleted)").unwrap_or(&text);
    let candidate = std::path::Path::new(path);
    // A library or integration-test executable does not implement the
    // `sidebar` verb. Starting it as a rail can leave a live, unowned pane
    // beside a person. Unit tests use scripted tmux calls and deliberately
    // exercise the argv builder, so only that compile mode can use its test
    // executable as an inert program name.
    let is_chief = candidate.file_name().is_some_and(|name| name == "chief");
    if candidate.is_file() && (cfg!(test) || is_chief) {
        return Some(path.to_owned());
    }
    tracing::error!(
        event = "sidebar.rail.program-missing",
        candidate = %path,
        raw = %text,
        "this client cannot find its own executable on disk, so a new window opens without \
         a rail rather than with a pane that cannot start; reattach the company to repair it"
    );
    None
}

/// Turn the three facts into the two lists the rail draws.
///
/// The three inputs are deliberately from three different authorities, and none
/// of them is re-derived here:
///
/// * `roster` — chiefd's STRUCTURE. Every department appears, in the canonical
///   `order` chiefd published (read the field, never the array position), and a
///   person is listed under the department they work in — under THAT ONE
///   department and no other, heads included. A department with
///   nobody in it still gets a row: the operator asked for all of them, and a
///   window is what an empty department does not get, not a row.
/// * `desired` — chiefd's MEMBERSHIP: exactly who should be running.
/// * `live` — TMUX's answer to who IS running, which chiefd does not have.
///
/// **EVERY person in the roster is drawn**, each carrying its own
/// [`PersonState`]. Nobody is filtered out: a person who is neither desired nor
/// live is SLEEPING, which is a state the operator asked to see and act on, not
/// an absence to hide. `idle` is the set whose settle clock is running.
///
/// * `crashing` — THE ACTUATOR's answer to whose boot keeps dying, and the
///   numbers about it. Desired, paneless and crashing is
///   [`PersonState::Crashing`]; desired, paneless and not crashing is
///   [`PersonState::Starting`]. Without this map the two collapse into one word
///   and a person who has been failing for an hour reads as one who is on their
///   way.
/// * `refused` — CHIEFD's LAUNCH GATE, person id to the gate's own reason. A
///   person in here is desired and cannot start, which is a third thing: not
///   `Starting` (nobody is on their way) and not `Sleeping` (they are wanted).
///   It arrives beside `crashing` because it is the same shape of fact about the
///   same moment — the actuator reads the desired set and the launch catalog in
///   one pass and hands both here — and it keeps the gate in the one process
///   that can see the disk it gates on.
#[must_use]
pub fn project(
    roster: &crate::roster::Roster,
    desired: &std::collections::BTreeSet<String>,
    live: &std::collections::BTreeSet<String>,
    idle: &std::collections::BTreeSet<String>,
    crashing: &BTreeMap<String, CrashNotice>,
    refused: &BTreeMap<String, String>,
) -> (Vec<DepartmentRow>, BTreeMap<String, Vec<PersonRow>>) {
    let ceo_id = roster
        .department(&roster.root_department_id)
        .map(|department| department.head_person_id.as_str());
    let parents: BTreeMap<&str, Option<&str>> = roster
        .departments
        .iter()
        .map(|dept| (dept.id.as_str(), dept.parent_department_id.as_deref()))
        .collect();
    let depth_of = |id: &str| -> usize {
        let mut depth = 0;
        let mut cursor = parents.get(id).copied().flatten();
        // Bounded by the department count: a cycle in a tree chiefd published
        // would otherwise spin here, and a rail that hangs is worse than a rail
        // that indents wrongly.
        while let Some(parent) = cursor {
            depth += 1;
            if depth > roster.departments.len() {
                break;
            }
            cursor = parents.get(parent).copied().flatten();
        }
        depth
    };

    let mut people: BTreeMap<String, Vec<PersonRow>> = BTreeMap::new();
    // THE ONE PERSON THE RAIL DOES NOT DRAW. Everybody else appears whatever
    // state they are in, because "asleep" is something the operator acts on. A
    // DEPARTED person is not asleep — they were fired, they are never coming
    // back, and the operator ruled it outright: "we never see fired employees".
    // They stay in the ROSTER, which is what lets the reap tell this company's
    // own leaked pane from a stranger's; they are only kept off the list.
    let mut ordered: Vec<&crate::roster::RosterPerson> =
        roster.people.iter().filter(|person| !person.departed()).collect();
    ordered.sort_by_key(|person| person.display_order);
    for person in ordered {
        // A DEPARTMENT'S PEOPLE ARE ITS HEAD PLUS ITS WORKERS. Heads are people,
        // and this repo's own model says a head lives in the unit they head —
        // appointing somebody head MOVES their `department_id` there. So
        // `department_id` alone SHOULD already list them.
        //
        // It is a union with `is_head_of` anyway, and deliberately: a roster
        // whose two fields somehow disagreed would otherwise drop that person
        // off this list entirely, and the operator reported precisely that —
        // `engineering-head` live in tmux, present in the rail's own
        // `live_people` field, and absent from the list. The union is right
        // whether or not the two agree, so the rail cannot be wrong about it
        // again.
        //
        // NESTING: there is none. A department's people are its OWN members,
        // and a head is a member of exactly one department — the one they head.
        //
        // A one-level roll-up used to list a child department's head under the
        // PARENT as well, on the reasoning that a manager is a report of the
        // unit above. The operator ruled it out on their own screen: selecting
        // `Executive` listed `Head of Engineering`, who works in Engineering,
        // and a list that answers "who works here" with somebody who works
        // elsewhere cannot be read. Selecting Engineering already shows that
        // person, which is the only place they belong.
        //
        // So every person appears on exactly ONE department's list, and the
        // department row's live and TOTAL counts are computed from this same
        // map below — the numbers beside a row and the list under it cannot
        // disagree, and neither double-counts a head.
        let mut homes = vec![person.department_id.clone()];
        if let Some(headed) = person.is_head_of.as_deref() {
            if headed != person.department_id {
                homes.push(headed.to_owned());
            }
        }
        for home in homes {
            // THE HEAD OF THIS DEPARTMENT, decided per row: the person heads
            // the very department the row is filed under. It is read off the
            // roster's `is_head_of`, never off a title.
            let manager = person.is_head_of.as_deref() == Some(home.as_str());
            people.entry(home).or_default().push(PersonRow {
                id: person.id.clone(),
                name: person_first_name(&person.display_name),
                title: person_display_role(
                    &person.display_name,
                    &person.title,
                    ceo_id == Some(person.id.as_str()),
                ),
                live: live.contains(&person.id),
                desired: desired.contains(&person.id),
                idle: idle.contains(&person.id),
                crash: crashing.get(&person.id).cloned(),
                refused: refused.get(&person.id).cloned(),
                manager,
            });
        }
    }

    let mut departments: Vec<&crate::roster::RosterDepartment> =
        roster.departments.iter().collect();
    departments.sort_by_key(|dept| dept.order);
    let rows = departments
        .into_iter()
        .map(|dept| {
            let department_people = people.get(&dept.id);
            DepartmentRow {
                id: dept.id.clone(),
                name: if dept.id == roster.root_department_id {
                    ROOT_DEPARTMENT_DISPLAY_NAME.to_owned()
                } else {
                    dept.name.clone()
                },
                depth: depth_of(&dept.id),
                live: department_people
                    .map_or(0, |rows| rows.iter().filter(|row| row.live).count()),
                total: department_people.map_or(0, Vec::len),
            }
        })
        .collect();
    (rows, people)
}

/// Decide what a click at row `row` of a rail `height` rows tall means.
///
/// Row coordinates are the rail pane's own, so the caller does no arithmetic:
/// tmux delivers a pane-relative position and this consumes it directly.
#[must_use]
pub fn click(view: &View, height: usize, column: usize, row: usize) -> Action {
    let control_row = height.saturating_sub(1);
    if row == control_row {
        return Action::ToggleCollapsed;
    }
    if view.collapsed() {
        // A collapsed rail is the control and nothing else: there are no rows
        // to hit, so a stray click must not select the department that happens
        // to sit at that index when it expands again.
        return Action::Ignored;
    }
    let index = view.scroll_offset() + row;
    view.tree_rows().get(index).map_or(Action::Ignored, |tree_row| match tree_row {
        TreeRow::DepartmentSpacer(_) => Action::Ignored,
        TreeRow::Department(department) => {
            if column == department_disclosure_column(department.depth) {
                Action::ToggleDepartmentDisclosure(department.id.clone())
            } else {
                Action::SelectDepartment(department.id.clone())
            }
        }
        TreeRow::Person(department, person) | TreeRow::Role(department, person) => {
            Action::FocusPerson {
                department_id: department.id.clone(),
                person_id: person.id.clone(),
            }
        }
    })
}

// ONE BRAIN PER SESSION: the process that owns this View, hit-tests every
// click against the frame it last pushed, and performs the gesture. See the
// module doc for the coordination protocol it replaces.
pub mod brain;
// The thin rail client: raw stdin up, whole frames blitted down. It holds no
// state at all, which is what lets a freshly minted window paint in one socket
// round trip.
pub mod client;
/// The overview shown when the operator clicks a DEPARTMENT row: who is in it,
/// who heads it, who is up and what each of them is running. It replaced the
/// tiled grid of that department's live people — see the module doc for the
/// resize the grid forced on every agent it held.
pub mod department_card;
pub mod effects;
// The per-gesture correlator every line a click causes is stamped with. See the
// module doc for why no funnel in this product could be trusted without one.
pub mod gesture;
// The one mouse decoder in the product: the brain reads what a thin client
// forwards. See the module doc for why the client parses nothing.
pub mod input;
pub mod render;
/// The interactive card shown before a sleeping person is woken.
pub mod sleeping_card;
// The brain-to-thin-client protocol: raw input up, whole frames down.
pub mod wire;

#[cfg(test)]
mod tests;
