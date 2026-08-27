//! One person's identity ACCENT — a colour derived from the roster.
//!
//! # Derivation and publication are separate
//!
//! This module is a pure function of the roster. The sidebar and browser call
//! it directly. New worker-home creation also publishes its answer into that
//! worker's create-once Pi theme, but the allocator never reads the file back
//! and never treats it as durable company state.
//!
//! # Allocation is by POSITION, and the position must be identity-stable
//!
//! [`identity_accent_order`] orders by `createdAt` (with `id` as a
//! deterministic tiebreak) rather than by any roster ordering that could
//! re-sort on a hire or a transfer, so a new hire always sorts LAST and takes
//! the next free slot without moving anybody already allocated. A person whose
//! chip changed colour because somebody else was hired would read as a
//! different person.

use std::collections::{BTreeMap, BTreeSet};

use chiefd_core::store::organization::PersonRecord;

use crate::materialize::{MaterializeError, ACCENT_EXHAUSTED};

/// The identity-stable ordering the accent allocator must be fed.
///
/// A person's accent is allocated by POSITION in the order given, so this
/// orders by `createdAt` (persisted once at registration, `id` as a
/// deterministic tiebreak) rather than any roster ordering that could re-sort
/// on hire/transfer — a new hire always sorts LAST and takes the next free
/// slot without moving anyone already allocated.
#[must_use]
pub fn identity_accent_order(people: &BTreeMap<String, PersonRecord>) -> Vec<String> {
    let mut order: Vec<(&str, &str)> =
        people.iter().map(|(id, person)| (person.created_at.as_str(), id.as_str())).collect();
    order.sort_unstable();
    order.into_iter().map(|(_, id)| id.to_owned()).collect()
}

/// The Chief Executive's identity accent, before the band rebalance.
///
/// Operator ruling, 2026-08-24: the Chief's title bar is PURPLE with white
/// text, not the palette red it took by being roster position 0. This is the
/// hue the operator picked, measured off their own screen.
///
/// Published rather than private because it is the INPUT the ruling names, and
/// the stored [`CHIEF_EXECUTIVE_ACCENT`] is a derivation from it — a reader
/// who finds only the derived hex cannot tell a decision from a typo.
pub const CHIEF_EXECUTIVE_ACCENT_SOURCE: &str = "#9076c7";

/// The Chief Executive's identity accent, FIXED rather than allocated.
///
/// [`CHIEF_EXECUTIVE_ACCENT_SOURCE`] rebalanced into [`RAW_ACCENT_LUMINANCE`],
/// and the rebalance is the whole point rather than a tidiness: the raw hue
/// sits at luminance ~0.230, ABOVE the identity band, and every consumer that
/// derives a ground from an accent keys on that band. `chief-cli`'s
/// `person_chip_background` darkens an in-band accent to 0.16 and then picks
/// ink that reads on the RESULT; an out-of-band accent is used raw, stays
/// light, and `contrast_foreground` correctly answers BLACK on it. So storing
/// `#9076c7` verbatim would have shipped a purple bar with black text — the
/// one thing the ruling names. In the band it darkens like every other
/// identity colour and white ink wins by measurement, with no special case
/// anywhere downstream.
///
/// The value is pinned as bytes rather than computed at each call because it
/// is an identity: `chief_executive_accent_is_the_operator_purple_rebalanced`
/// proves it is exactly the rebalance of the source hue.
pub const CHIEF_EXECUTIVE_ACCENT: &str = "#896dc3";

/// One person's identity accent, allocated by roster position.
///
/// Every roster person in the order is allocated one — the Chief included —
/// because tmux still uses the Chief's accent as a pane identity. The separate
/// home writer skips the Chief, so this allocation does not give the Chief a
/// Pi theme override.
///
/// `chief_person_id` names the CEO when the caller knows one. The CEO is the
/// one person whose colour is NOT its palette slot: it always answers
/// [`CHIEF_EXECUTIVE_ACCENT`]. It still CONSUMES its slot, so nobody else's
/// colour moves, and the fixed purple is held out of the allocator so a hue
/// wrap can never hand it to a later hire.
///
/// # Errors
/// [`ACCENT_EXHAUSTED`].
pub fn organization_person_accent(
    people_order: &[String],
    chief_person_id: Option<&str>,
    person_id: &str,
) -> Result<String, MaterializeError> {
    let index = people_order.iter().position(|id| id == person_id).ok_or_else(|| {
        MaterializeError::refuse(
            ACCENT_EXHAUSTED,
            format!("Cannot allocate an organization accent for unknown person '{person_id}'"),
        )
    })?;
    let accents = organization_person_accents(people_order, chief_person_id)?;
    accents.get(index).cloned().ok_or_else(|| {
        MaterializeError::refuse(
            ACCENT_EXHAUSTED,
            format!("Cannot allocate an organization accent for unknown person '{person_id}'"),
        )
    })
}

/// High-contrast accents assigned in organization roster order.
///
/// A compact, Material-derived ten-family palette, each rebalanced to
/// luminance ~0.202. The raw value is the stable identity input; Pi derives a
/// mode-specific foreground from it because one RGB value cannot meet normal
/// text contrast on both near-white and near-black surfaces.
const ORGANIZATION_PERSON_ACCENTS: [&str; 10] = [
    "#e24033", "#c75e00", "#a27400", "#2c8e46", "#00899a", "#3c7adf", "#6977c5", "#a74ef5",
    "#d83d98", "#c05e68",
];

/// Degrees of hue rotation applied per wrap cycle once the roster outgrows the
/// curated palette. 37 is not a divisor of 360, so repeated application walks
/// the wheel instead of landing back on the base hue.
const ACCENT_WRAP_HUE_STEP_DEGREES: f64 = 37.0;

/// The relative-luminance band of every curated raw identity accent.
///
/// Hue rotation changes relative luminance even when HSL lightness is held
/// constant. Rebalance every wrapped result to this center so a later roster
/// position cannot become a nearly-white yellow or nearly-black blue.
const RAW_ACCENT_LUMINANCE: f64 = 0.202;

/// How many rotations are attempted before the allocator gives up loudly.
const ACCENT_WRAP_MAX_ATTEMPTS: usize = 360;

fn organization_person_accents(
    people_order: &[String],
    chief_person_id: Option<&str>,
) -> Result<Vec<String>, MaterializeError> {
    let mut allocated = Vec::with_capacity(people_order.len());
    let mut taken: BTreeSet<String> = BTreeSet::new();
    // Reserved before the first allocation, whether or not the CEO is in this
    // order: a narrowed roster still must not hand the Chief's purple to
    // somebody else, and a hue wrap is the only way it could.
    taken.insert(CHIEF_EXECUTIVE_ACCENT.to_owned());
    for (index, person_id) in people_order.iter().enumerate() {
        let base = ORGANIZATION_PERSON_ACCENTS[index % ORGANIZATION_PERSON_ACCENTS.len()];
        #[allow(clippy::cast_precision_loss)]
        let cycle = (index / ORGANIZATION_PERSON_ACCENTS.len()) as f64;
        let mut candidate = if cycle == 0.0 {
            base.to_owned()
        } else {
            color_with_relative_luminance(
                &rotate_hue(base, cycle * ACCENT_WRAP_HUE_STEP_DEGREES),
                RAW_ACCENT_LUMINANCE,
            )
        };
        let mut attempt = 1_usize;
        while taken.contains(&candidate) && attempt <= ACCENT_WRAP_MAX_ATTEMPTS {
            #[allow(clippy::cast_precision_loss)]
            let offset = attempt as f64;
            candidate = color_with_relative_luminance(
                &rotate_hue(base, cycle * ACCENT_WRAP_HUE_STEP_DEGREES + offset),
                RAW_ACCENT_LUMINANCE,
            );
            attempt += 1;
        }
        if taken.contains(&candidate) {
            return Err(MaterializeError::refuse(
                ACCENT_EXHAUSTED,
                format!(
                    "Cannot allocate a distinct organization accent for roster position {index} \
                     ('{person_id}'): the palette and its hue rotations are exhausted. Refusing \
                     to hand two people the same identity color."
                ),
            ));
        }
        // The slot is consumed even when the CEO does not wear it, so the
        // override moves exactly one person's colour and nobody else's.
        taken.insert(candidate.clone());
        allocated.push(if chief_person_id == Some(person_id.as_str()) {
            CHIEF_EXECUTIVE_ACCENT.to_owned()
        } else {
            candidate
        });
    }
    Ok(allocated)
}

// --- colour math (ported from the deleted IdentityTheme.ts) ----------------

fn rgb(hex_color: &str) -> [f64; 3] {
    let bytes = hex_color.as_bytes();
    let channel = |offset: usize| -> f64 {
        if bytes.len() < offset + 2 {
            return 0.0;
        }
        std::str::from_utf8(&bytes[offset..offset + 2])
            .ok()
            .and_then(|slice| u8::from_str_radix(slice, 16).ok())
            .map_or(0.0, f64::from)
    };
    [channel(1), channel(3), channel(5)]
}

fn hex(values: [f64; 3]) -> String {
    let mut out = String::from("#");
    for value in values {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = value.clamp(0.0, 255.0) as u8;
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn rgb_to_hsl(hex_color: &str) -> [f64; 3] {
    let normalized = rgb(hex_color).map(|channel| channel / 255.0);
    let (r, g, b) = (normalized[0], normalized[1], normalized[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    if (max - min).abs() < f64::EPSILON {
        return [0.0, 0.0, lightness];
    }
    let delta = max - min;
    let saturation = if lightness > 0.5 { delta / (2.0 - max - min) } else { delta / (max + min) };
    let hue = if (max - r).abs() < f64::EPSILON {
        ((g - b) / delta + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() < f64::EPSILON {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    [hue * 360.0, saturation, lightness]
}

fn hsl_to_hex(hue: f64, saturation: f64, lightness: f64) -> String {
    let h = (((hue % 360.0) + 360.0) % 360.0) / 360.0;
    if saturation == 0.0 {
        let value = (lightness * 255.0).round();
        return hex([value, value, value]);
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let channel = |offset: f64| -> f64 {
        let mut t = h + offset;
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 0.5 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    };
    hex([
        (channel(1.0 / 3.0) * 255.0).round(),
        (channel(0.0) * 255.0).round(),
        (channel(-1.0 / 3.0) * 255.0).round(),
    ])
}

/// Rotate a color's hue, preserving saturation and lightness.
fn rotate_hue(hex_color: &str, degrees: f64) -> String {
    let [hue, saturation, lightness] = rgb_to_hsl(hex_color);
    hsl_to_hex(hue + degrees, saturation, lightness)
}

fn linear_channel(channel: f64) -> f64 {
    let value = channel / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance for one `#rrggbb` color.
#[must_use]
pub(crate) fn relative_luminance(hex_color: &str) -> f64 {
    let [r, g, b] = rgb(hex_color).map(linear_channel);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Move only HSL lightness until `hex_color` reaches `target` relative
/// luminance. Hue and saturation stay fixed, so identity remains recognizable.
#[must_use]
pub(crate) fn color_with_relative_luminance(hex_color: &str, target: f64) -> String {
    let [hue, saturation, _] = rgb_to_hsl(hex_color);
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..32 {
        let middle = (low + high) / 2.0;
        let candidate = hsl_to_hex(hue, saturation, middle);
        if relative_luminance(&candidate) < target {
            low = middle;
        } else {
            high = middle;
        }
    }
    let darker = hsl_to_hex(hue, saturation, low);
    let lighter = hsl_to_hex(hue, saturation, high);
    if (relative_luminance(&darker) - target).abs() <= (relative_luminance(&lighter) - target).abs()
    {
        darker
    } else {
        lighter
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiefd_core::store::organization::{EmploymentState, PersonKind};

    fn person(id: &str, created_at: &str) -> PersonRecord {
        PersonRecord {
            id: id.to_owned(),
            name: id.to_owned(),
            title: "Analyst".to_owned(),
            mandate: "Own it.".to_owned(),
            kind: PersonKind::Worker,
            department_id: "quant".to_owned(),
            employment_state: EmploymentState::Active,
            activation: "resident".to_owned(),
            tools: Vec::new(),
            prompts: Vec::new(),
            created_at: created_at.to_owned(),
            staffing_history: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn accents_are_allocated_by_stable_roster_position_and_never_duplicated() {
        let order: Vec<String> = (0..25).map(|index| format!("p{index}")).collect();
        let mut seen = BTreeSet::new();
        for person_id in &order {
            let accent = organization_person_accent(&order, None, person_id).expect("accent");
            assert!(seen.insert(accent), "two people must never share an identity color");
        }
        assert_eq!(organization_person_accent(&order, None, "p0").expect("accent"), "#e24033");
        assert!(organization_person_accent(&order, None, "nobody").is_err());
    }

    #[test]
    fn chief_executive_accent_is_the_operator_purple_rebalanced_into_the_identity_band() {
        assert_eq!(
            CHIEF_EXECUTIVE_ACCENT,
            color_with_relative_luminance(CHIEF_EXECUTIVE_ACCENT_SOURCE, RAW_ACCENT_LUMINANCE),
            "the stored purple is exactly the operator's hue rebalanced, never a hand-typed hex"
        );
        // The band is what makes the chip darken and the ink come out white.
        // The raw hue is ABOVE it, which is why it is not what we store.
        assert!(relative_luminance(CHIEF_EXECUTIVE_ACCENT_SOURCE) > 0.21);
        let delta = (relative_luminance(CHIEF_EXECUTIVE_ACCENT) - RAW_ACCENT_LUMINANCE).abs();
        assert!(delta <= 0.003, "the chief's purple left the raw luminance band by {delta}");
        assert!(!ORGANIZATION_PERSON_ACCENTS.contains(&CHIEF_EXECUTIVE_ACCENT));
    }

    #[test]
    fn the_ceo_wears_the_fixed_purple_and_moves_nobody_else() {
        let order: Vec<String> = (0..25).map(|index| format!("p{index}")).collect();
        let without = organization_person_accents(&order, None).expect("accents");
        let with = organization_person_accents(&order, Some("p0")).expect("accents");

        assert_eq!(with[0], CHIEF_EXECUTIVE_ACCENT, "the CEO's colour is fixed, not allocated");
        assert_eq!(without[0], "#e24033", "and it is the palette slot it replaces");
        assert_eq!(with[1..], without[1..], "no other person's accent moves with the override");
        assert_eq!(with[1], "#c75e00", "the CEO still CONSUMES slot 0, so slot 1 stays put");
        assert_eq!(
            organization_person_accent(&order, Some("p0"), "p0").expect("accent"),
            CHIEF_EXECUTIVE_ACCENT
        );
    }

    #[test]
    fn a_non_ceo_at_roster_position_zero_still_takes_the_palette_slot() {
        let order: Vec<String> = (0..3).map(|index| format!("p{index}")).collect();
        // The override keys on IDENTITY, never on position: the CEO is the
        // oldest row today, and a roster whose first row is somebody else must
        // still be painted from the palette.
        let accents = organization_person_accents(&order, Some("p2")).expect("accents");
        assert_eq!(accents[0], "#e24033");
        assert_eq!(accents[1], "#c75e00");
        assert_eq!(accents[2], CHIEF_EXECUTIVE_ACCENT);
    }

    #[test]
    fn the_chiefs_purple_is_held_out_of_the_allocator_even_with_no_chief_in_the_order() {
        let order: Vec<String> = (0..200).map(|index| format!("p{index}")).collect();
        for accents in [
            organization_person_accents(&order, None).expect("accents"),
            organization_person_accents(&order, Some("p0")).expect("accents"),
        ] {
            let purple = CHIEF_EXECUTIVE_ACCENT;
            let wearers = accents.iter().filter(|hex| hex.as_str() == purple).count();
            assert!(wearers <= 1, "a hue wrap handed {wearers} people the Chief's purple");
        }
    }

    #[test]
    fn every_hue_wrap_returns_to_the_raw_identity_luminance_band() {
        let order: Vec<String> = (0..200).map(|index| format!("p{index}")).collect();
        let accents = organization_person_accents(&order, None).expect("accents");
        assert_eq!(
            &accents[10..20],
            [
                "#9f7517", "#788300", "#5c8900", "#2b8a7e", "#3c72ff", "#8566e6", "#9468c5",
                "#e10dc2", "#da4a45", "#a37240",
            ],
            "Rust and the extension allocator pin this first wrap byte for byte"
        );
        for accent in accents.iter().skip(ORGANIZATION_PERSON_ACCENTS.len()) {
            let delta = (relative_luminance(accent) - RAW_ACCENT_LUMINANCE).abs();
            assert!(delta <= 0.003, "{accent} left the raw luminance band by {delta}");
        }
    }

    #[test]
    fn the_accent_order_is_by_creation_then_id_so_a_new_hire_sorts_last() {
        let mut people = BTreeMap::new();
        people.insert("zed".to_owned(), person("zed", "2026-01-01T00:00:00.000Z"));
        people.insert("abe".to_owned(), person("abe", "2026-06-01T00:00:00.000Z"));
        assert_eq!(identity_accent_order(&people), vec!["zed".to_string(), "abe".to_string()]);
    }
}
