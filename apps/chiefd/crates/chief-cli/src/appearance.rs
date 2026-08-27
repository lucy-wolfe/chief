//! The ONE reader of this box's appearance authority.
//!
//! # Why one reader, and why it is a module rather than a helper beside a
//! caller
//!
//! The browser bridge writes `light` or `dark` into `/run/tribes-theme`, and
//! that file is the whole authority: it is the operator's CURRENT choice, it
//! changes while every process on this box keeps running, and it is the only
//! statement of appearance that is not a guess. Everything else — a
//! `TRIBES_THEME` in the environment, a `COLORFGBG` a terminal set at the
//! moment a process was born — is a snapshot of what was true when a process
//! started, which for a daemon means "what was true at boot" and for a pane a
//! daemon launched means "what was true at the DAEMON's boot".
//!
//! Two surfaces already read it independently: the rail
//! ([`crate::sidebar::render`]) on every draw, and the sleeping card. A third
//! reader now exists that is not a drawing surface at all —
//! [`crate::actuate::spawn_cmd`] seeds the value into a pane's launch
//! environment — and three copies of one derivation is how two of them drift
//! and the operator gets a light rail beside a dark pane. So the derivation
//! lives here, once, and each caller states only WHAT it wants from it: the
//! declaration alone ([`read_declared`]) or the full resolution including the
//! terminal's own hint ([`read_is_light`]).
//!
//! # Declaration and resolution are two different questions
//!
//! [`declared`] answers "did the authority SAY something?" and returns
//! [`None`] when it did not. [`is_light`] answers "what should I draw?", which
//! must always have an answer, so it falls through to `COLORFGBG` and finally
//! to dark. A drawing surface needs the second; anything that must decide
//! whether to speak at all needs the first, because inventing an appearance
//! there is worse than staying quiet.

use std::fs;

/// Where the browser bridge publishes the operator's current appearance. Read
/// on demand, never cached: a long-running Chief process keeps its launch
/// environment but the operator can flip this at any moment, and a cached
/// answer is stale from the first flip onward.
const LIVE_THEME_PATH: &str = "/run/tribes-theme";

/// The two grounds a terminal can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// A light ground: dark text on a near-white surface.
    Light,
    /// A dark ground: light text on a near-black surface.
    Dark,
}

impl Appearance {
    /// The conventional `COLORFGBG` value for this ground.
    ///
    /// `COLORFGBG` is `<foreground>;<background>` in ANSI palette indices, and
    /// the two values below are what every terminal that sets the variable at
    /// all writes: `0;15` is black on white and `15;0` is white on black. The
    /// exact indices matter because a reader (Pi's own
    /// `detectTerminalBackgroundFromEnv`) takes the LAST field and computes its
    /// relative luminance — 15 is white and reads light, 0 is black and reads
    /// dark. Anything else would be a value this product invented, which no
    /// terminal writes and no reader is written against.
    #[must_use]
    pub fn colorfgbg(self) -> &'static str {
        match self {
            Self::Light => "0;15",
            Self::Dark => "15;0",
        }
    }

    /// Whether this is the light ground.
    #[must_use]
    pub fn is_light(self) -> bool {
        matches!(self, Self::Light)
    }
}

/// What the authority DECLARED, or [`None`] if it declared nothing.
///
/// The bridge file wins over the environment for the reason the bridge exists:
/// `TRIBES_THEME` is inherited once at exec and the bridge is rewritten
/// whenever the operator changes their mind. An unreadable or unrecognised
/// value on either is treated as no declaration rather than as a default —
/// "the file says `auto`" and "there is no file" are the same fact, namely
/// that nobody has stated an appearance.
#[must_use]
pub fn declared(live_theme: Option<&str>, env_theme: Option<&str>) -> Option<Appearance> {
    let live = live_theme.map(str::trim).filter(|theme| matches!(*theme, "light" | "dark"));
    match live.or(env_theme).map(str::to_ascii_lowercase).as_deref() {
        Some("light") => Some(Appearance::Light),
        Some("dark") => Some(Appearance::Dark),
        _ => None,
    }
}

/// The full resolution a drawing surface needs: the declaration if there is
/// one, else the terminal's own `COLORFGBG` hint, else dark.
///
/// `COLORFGBG` is the terminal convention for telling a client whether its
/// default ground is light or dark; the background index is the last field,
/// and 7 (light grey) plus 10..=15 (the bright half) are the light grounds.
#[must_use]
pub fn is_light(
    live_theme: Option<&str>,
    env_theme: Option<&str>,
    colorfgbg: Option<&str>,
) -> bool {
    if let Some(appearance) = declared(live_theme, env_theme) {
        return appearance.is_light();
    }
    colorfgbg
        .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
        .is_some_and(|background| matches!(background, 7 | 10..=15))
}

/// [`declared`], read from this box: the bridge file, then `TRIBES_THEME`.
#[must_use]
pub fn read_declared() -> Option<Appearance> {
    let live_theme = fs::read_to_string(LIVE_THEME_PATH).ok();
    declared(live_theme.as_deref(), std::env::var("TRIBES_THEME").ok().as_deref())
}

/// [`is_light`], read from this box: the bridge file, `TRIBES_THEME`, then
/// this process's own inherited `COLORFGBG`.
#[must_use]
pub fn read_is_light() -> bool {
    let live_theme = fs::read_to_string(LIVE_THEME_PATH).ok();
    is_light(
        live_theme.as_deref(),
        std::env::var("TRIBES_THEME").ok().as_deref(),
        std::env::var("COLORFGBG").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_outranks_every_inherited_snapshot() {
        assert_eq!(declared(Some("light\n"), Some("dark")), Some(Appearance::Light));
        assert_eq!(declared(Some("dark"), Some("light")), Some(Appearance::Dark));
        assert!(is_light(Some("light"), Some("dark"), Some("15;0")));
        assert!(!is_light(Some("dark"), Some("light"), Some("0;15")));
    }

    #[test]
    fn nothing_stated_is_no_declaration_rather_than_a_default() {
        for unstated in [None, Some(""), Some("auto"), Some("invalid")] {
            assert_eq!(declared(unstated, None), None, "{unstated:?}");
        }
        assert_eq!(declared(Some("auto"), Some("light")), Some(Appearance::Light));
    }

    #[test]
    fn a_drawing_surface_always_gets_an_answer_and_falls_back_to_the_terminal() {
        assert!(is_light(None, None, Some("0;15")));
        assert!(is_light(None, Some("auto"), Some("0;7")));
        assert!(!is_light(None, None, Some("15;0")));
        assert!(!is_light(None, None, None), "no hint at all draws dark");
    }

    /// The exact bytes a launcher seeds into a pane. Pi reads the LAST field
    /// and computes its luminance, so these two indices — not merely "some
    /// light-ish pair" — are what make the pane agree with the rail.
    #[test]
    fn the_seeded_colorfgbg_is_the_conventional_pair_for_each_ground() {
        assert_eq!(Appearance::Light.colorfgbg(), "0;15");
        assert_eq!(Appearance::Dark.colorfgbg(), "15;0");
        // Round trip: what this module writes, this module reads back the same
        // way — a seeded pane and a rail cannot disagree about the value.
        assert!(is_light(None, None, Some(Appearance::Light.colorfgbg())));
        assert!(!is_light(None, None, Some(Appearance::Dark.colorfgbg())));
    }
}
