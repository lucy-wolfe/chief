//! One worker's Pi-native automatic identity theme.
//!
//! The files in this module are written only while a new agent home is
//! created. Pi reads the project-local `light/dark` pair from the worker's
//! cwd, then selects the correct member from the terminal color scheme. The
//! Chief has no agent home and never enters this writer, so the Chief keeps
//! Pi's ordinary neutral appearance.
//!
//! # Both halves are PROJECT scope, and until #1307 only one of them was
//!
//! The theme SETTING has always been project scope, in `<home>/.pi/settings.json`.
//! The theme FILES sat in `<home>/themes/`, which resolved only because chief
//! pointed `PI_CODING_AGENT_DIR` at the home, making it Pi's USER theme root
//! (`resource-loader.js` reads `join(agentDir, "themes")`). With that redirect
//! gone, the user root is the operator's own `~/.pi/agent/themes` and a file
//! here would never be read again. The files moved to `<home>/.pi/themes/`,
//! which `package-manager.js` auto-discovers as a project resource, so the
//! setting and the files it names now live in the same scope.
//!
//! That discovery is gated on the project being TRUSTED. chief passes
//! `--approve` on every managed launch, which is what admits these files at
//! all — see `spawn_cmd::launch_command`.

use std::path::Path;

use serde_json::{json, Value};

use crate::materialize::MaterializeError;

const THEME_SCHEMA: &str = "https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/src/modes/interactive/theme/theme-schema.json";

// These targets leave rounding margin above 4.5:1 on Pi 0.80.10's complete
// Light and Dark surface sets. They change HSL lightness only; the stable raw
// roster hue remains the identity input.
const LIGHT_IDENTITY_LUMINANCE: f64 = 0.080;
const DARK_IDENTITY_LUMINANCE: f64 = 0.400;

/// Write the worker's project setting and its two mode-specific theme files.
///
/// # Errors
/// [`MaterializeError`] on a filesystem or serialization failure.
pub(crate) fn write_agent_theme(
    home: &Path,
    person_id: &str,
    identity_color: &str,
) -> Result<(), MaterializeError> {
    let light_name = theme_name(person_id, "light");
    let dark_name = theme_name(person_id, "dark");
    publish_json(
        &home.join(".pi/settings.json"),
        &json!({ "theme": format!("{light_name}/{dark_name}") }),
    )?;
    publish_json(
        &home.join(format!(".pi/themes/{light_name}.json")),
        &theme(light_name, identity_color, Mode::Light),
    )?;
    publish_json(
        &home.join(format!(".pi/themes/{dark_name}.json")),
        &theme(dark_name, identity_color, Mode::Dark),
    )
}

/// Refresh Chief-owned organization theme files without changing the agent's
/// project settings or any other home content.
pub(crate) fn write_agent_theme_files(
    home: &Path,
    person_id: &str,
    identity_color: &str,
) -> Result<(), MaterializeError> {
    let light_name = theme_name(person_id, "light");
    let dark_name = theme_name(person_id, "dark");
    publish_existing_json(
        &home.join(format!(".pi/themes/{light_name}.json")),
        &theme(light_name, identity_color, Mode::Light),
    )?;
    publish_existing_json(
        &home.join(format!(".pi/themes/{dark_name}.json")),
        &theme(dark_name, identity_color, Mode::Dark),
    )
}

fn publish_json(path: &Path, value: &Value) -> Result<(), MaterializeError> {
    let text = serde_json::to_string_pretty(value).map_err(|error| {
        MaterializeError::filesystem(format!("cannot encode {}: {error}", path.display()))
    })?;
    crate::materialize::publish_text(path, &format!("{text}\n"), 0o644)
}

fn publish_existing_json(path: &Path, value: &Value) -> Result<(), MaterializeError> {
    let text = serde_json::to_string_pretty(value).map_err(|error| {
        MaterializeError::filesystem(format!("cannot encode {}: {error}", path.display()))
    })?;
    crate::files::publish_atomically_if_changed_in_existing_directory(
        path,
        &format!("{text}\n"),
        0o644,
    )
    .map_err(MaterializeError::from)
}

fn theme_name(person_id: &str, mode: &str) -> String {
    format!("organization-{person_id}-{mode}")
}

#[derive(Clone, Copy)]
enum Mode {
    Light,
    Dark,
}

fn theme(name: String, identity: &str, mode: Mode) -> Value {
    let foreground_luminance = if matches!(mode, Mode::Light) {
        LIGHT_IDENTITY_LUMINANCE
    } else {
        DARK_IDENTITY_LUMINANCE
    };
    // Every value used as text is normalized to the same mode-safe luminance.
    // This keeps each hue but makes status, detail, Markdown, tool, and custom
    // message text readable on every card ground in that mode.
    let readable =
        |color: &str| crate::accent::color_with_relative_luminance(color, foreground_luminance);
    let identity = readable(identity);
    let (vars, syntax, export) = match mode {
        Mode::Light => (
            json!({
                "identity": identity,
                "blue": readable("#547da7"),
                "green": readable("#588458"),
                "red": readable("#aa5555"),
                "yellow": readable("#9a7326"),
                "mediumGray": readable("#6c6c6c"),
                "dimGray": readable("#767676"),
                "lightGray": readable("#b0b0b0"),
                "selectedBg": "#d0d0e0",
                "userMsgBg": "#e8e8e8",
                "toolPendingBg": "#e8e8f0",
                "toolSuccessBg": "#e8f0e8",
                "toolErrorBg": "#f0e8e8",
                "customMsgBg": "#ede7f6"
            }),
            json!({
                "syntaxComment": readable("#008000"),
                "syntaxKeyword": readable("#0000FF"),
                "syntaxFunction": readable("#795E26"),
                "syntaxVariable": readable("#001080"),
                "syntaxString": readable("#A31515"),
                "syntaxNumber": readable("#098658"),
                "syntaxType": readable("#267F99"),
                "syntaxOperator": readable("#000000"),
                "syntaxPunctuation": readable("#000000")
            }),
            json!({ "pageBg": "#f8f8f8", "cardBg": "#ffffff", "infoBg": "#fffae6" }),
        ),
        Mode::Dark => (
            json!({
                "identity": identity,
                "blue": readable("#5f87ff"),
                "green": readable("#b5bd68"),
                "red": readable("#cc6666"),
                "yellow": readable("#ffff00"),
                "gray": readable("#808080"),
                "dimGray": readable("#666666"),
                "darkGray": readable("#505050"),
                "selectedBg": "#3a3a4a",
                "userMsgBg": "#343541",
                "toolPendingBg": "#282832",
                "toolSuccessBg": "#283228",
                "toolErrorBg": "#3c2828",
                "customMsgBg": "#2d2838"
            }),
            json!({
                "syntaxComment": readable("#6A9955"),
                "syntaxKeyword": readable("#569CD6"),
                "syntaxFunction": readable("#DCDCAA"),
                "syntaxVariable": readable("#9CDCFE"),
                "syntaxString": readable("#CE9178"),
                "syntaxNumber": readable("#B5CEA8"),
                "syntaxType": readable("#4EC9B0"),
                "syntaxOperator": readable("#D4D4D4"),
                "syntaxPunctuation": readable("#D4D4D4")
            }),
            json!({ "pageBg": "#18181e", "cardBg": "#1e1e24", "infoBg": "#3c3728" }),
        ),
    };

    let muted = if matches!(mode, Mode::Light) { "mediumGray" } else { "gray" };
    let border_muted = if matches!(mode, Mode::Light) { "lightGray" } else { "darkGray" };
    let mut colors = json!({
        "accent": "identity",
        "border": "blue",
        "borderAccent": "identity",
        "borderMuted": border_muted,
        "success": "green",
        "error": "red",
        "warning": "yellow",
        "muted": muted,
        "dim": "dimGray",
        "text": "identity",
        "thinkingText": "identity",
        "selectedBg": "selectedBg",
        "userMessageBg": "userMsgBg",
        "userMessageText": "identity",
        "customMessageBg": "customMsgBg",
        "customMessageText": "identity",
        "customMessageLabel": "identity",
        "toolPendingBg": "toolPendingBg",
        "toolSuccessBg": "toolSuccessBg",
        "toolErrorBg": "toolErrorBg",
        "toolTitle": "identity",
        "toolOutput": "identity",
        "mdHeading": "identity",
        "mdLink": "identity",
        "mdLinkUrl": "dimGray",
        "mdCode": "identity",
        "mdCodeBlock": "identity",
        "mdCodeBlockBorder": muted,
        "mdQuote": "identity",
        "mdQuoteBorder": muted,
        "mdHr": muted,
        "mdListBullet": "identity",
        "toolDiffAdded": "green",
        "toolDiffRemoved": "red",
        "toolDiffContext": muted,
        "thinkingOff": border_muted,
        "thinkingMinimal": "dimGray",
        "thinkingLow": "identity",
        "thinkingMedium": "identity",
        "thinkingHigh": "identity",
        "thinkingXhigh": "identity",
        "thinkingMax": "identity",
        "bashMode": "green"
    });
    if let (Some(object), Some(syntax_colors)) = (colors.as_object_mut(), syntax.as_object()) {
        for (key, value) in syntax_colors {
            object.insert(key.clone(), value.clone());
        }
    }

    json!({
        "$schema": THEME_SCHEMA,
        "name": name,
        "vars": vars,
        "colors": colors,
        "export": export
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURATED_ACCENTS: [&str; 10] = [
        "#e24033", "#c75e00", "#a27400", "#2c8e46", "#00899a", "#3c7adf", "#6977c5", "#a74ef5",
        "#d83d98", "#c05e68",
    ];

    fn contrast(left: &str, right: &str) -> f64 {
        let left = crate::accent::relative_luminance(left);
        let right = crate::accent::relative_luminance(right);
        (left.max(right) + 0.05) / (left.min(right) + 0.05)
    }

    fn resolved_color(document: &Value, token: &str) -> String {
        let value = document["colors"][token].as_str().expect("color token");
        if value.starts_with('#') {
            value.to_owned()
        } else {
            document["vars"][value]
                .as_str()
                .unwrap_or_else(|| panic!("unresolved {token}: {value}"))
                .to_owned()
        }
    }

    #[test]
    fn both_variants_have_every_pi_color_and_distinct_mode_backgrounds() {
        let light = theme("person-light".to_owned(), "#e24033", Mode::Light);
        let dark = theme("person-dark".to_owned(), "#e24033", Mode::Dark);
        let required = [
            "accent",
            "border",
            "borderAccent",
            "borderMuted",
            "success",
            "error",
            "warning",
            "muted",
            "dim",
            "text",
            "thinkingText",
            "selectedBg",
            "userMessageBg",
            "userMessageText",
            "customMessageBg",
            "customMessageText",
            "customMessageLabel",
            "toolPendingBg",
            "toolSuccessBg",
            "toolErrorBg",
            "toolTitle",
            "toolOutput",
            "mdHeading",
            "mdLink",
            "mdLinkUrl",
            "mdCode",
            "mdCodeBlock",
            "mdCodeBlockBorder",
            "mdQuote",
            "mdQuoteBorder",
            "mdHr",
            "mdListBullet",
            "toolDiffAdded",
            "toolDiffRemoved",
            "toolDiffContext",
            "syntaxComment",
            "syntaxKeyword",
            "syntaxFunction",
            "syntaxVariable",
            "syntaxString",
            "syntaxNumber",
            "syntaxType",
            "syntaxOperator",
            "syntaxPunctuation",
            "thinkingOff",
            "thinkingMinimal",
            "thinkingLow",
            "thinkingMedium",
            "thinkingHigh",
            "thinkingXhigh",
            "thinkingMax",
            "bashMode",
        ];
        for variant in [&light, &dark] {
            let colors = variant["colors"].as_object().expect("colors");
            for key in required {
                assert!(colors.contains_key(key), "missing Pi theme color {key}");
            }
            assert_eq!(colors["text"], "identity");
            assert_eq!(colors["thinkingText"], "identity");
            assert_eq!(colors["customMessageText"], "identity");
        }
        assert_ne!(light["vars"]["customMsgBg"], dark["vars"]["customMsgBg"]);
        assert_ne!(light["export"]["pageBg"], dark["export"]["pageBg"]);
    }

    #[test]
    fn every_curated_identity_foreground_meets_aa_on_every_pi_surface() {
        let light_backgrounds =
            ["#f8f8f8", "#d0d0e0", "#e8e8e8", "#ede7f6", "#e8e8f0", "#e8f0e8", "#f0e8e8"];
        let dark_backgrounds =
            ["#18181e", "#3a3a4a", "#343541", "#2d2838", "#282832", "#283228", "#3c2828"];
        for raw in CURATED_ACCENTS {
            for (mode, backgrounds) in [
                (Mode::Light, light_backgrounds.as_slice()),
                (Mode::Dark, dark_backgrounds.as_slice()),
            ] {
                let document = theme("person".to_owned(), raw, mode);
                let foreground = document["vars"]["identity"].as_str().expect("identity");
                for background in backgrounds {
                    assert!(
                        contrast(foreground, background) >= 4.5,
                        "{raw} became {foreground} with insufficient contrast on {background}"
                    );
                }
                let colors = document["colors"].as_object().expect("colors");
                for token in [
                    "accent",
                    "borderAccent",
                    "text",
                    "thinkingText",
                    "userMessageText",
                    "customMessageText",
                    "customMessageLabel",
                    "toolTitle",
                    "toolOutput",
                    "mdHeading",
                    "mdLink",
                    "mdCode",
                    "mdCodeBlock",
                    "mdQuote",
                    "mdListBullet",
                    "thinkingLow",
                    "thinkingMedium",
                    "thinkingHigh",
                    "thinkingXhigh",
                    "thinkingMax",
                ] {
                    assert_eq!(colors[token], "identity", "{token} escaped the readable identity");
                }
            }
        }
    }

    #[test]
    fn every_resolved_product_card_pair_meets_aa_in_both_modes() {
        let foreground_tokens = [
            "accent",
            "border",
            "borderAccent",
            "borderMuted",
            "success",
            "error",
            "warning",
            "muted",
            "dim",
            "text",
            "thinkingText",
            "userMessageText",
            "customMessageText",
            "customMessageLabel",
            "toolTitle",
            "toolOutput",
            "mdHeading",
            "mdLink",
            "mdLinkUrl",
            "mdCode",
            "mdCodeBlock",
            "mdCodeBlockBorder",
            "mdQuote",
            "mdQuoteBorder",
            "mdHr",
            "mdListBullet",
            "toolDiffAdded",
            "toolDiffRemoved",
            "toolDiffContext",
            "syntaxComment",
            "syntaxKeyword",
            "syntaxFunction",
            "syntaxVariable",
            "syntaxString",
            "syntaxNumber",
            "syntaxType",
            "syntaxOperator",
            "syntaxPunctuation",
            "thinkingOff",
            "thinkingMinimal",
            "thinkingLow",
            "thinkingMedium",
            "thinkingHigh",
            "thinkingXhigh",
            "thinkingMax",
            "bashMode",
        ];
        let card_background_tokens = [
            "selectedBg",
            "userMessageBg",
            "customMessageBg",
            "toolPendingBg",
            "toolSuccessBg",
            "toolErrorBg",
        ];
        let export_backgrounds = ["pageBg", "cardBg", "infoBg"];
        let order: Vec<String> = (0..40).map(|index| format!("person-{index}")).collect();
        for person_id in &order {
            let accent = crate::accent::organization_person_accent(&order, None, person_id)
                .expect("configured product accent");
            for mode in [Mode::Light, Mode::Dark] {
                let document = theme("person".to_owned(), &accent, mode);
                let backgrounds = card_background_tokens
                    .iter()
                    .map(|token| (token.to_string(), resolved_color(&document, token)))
                    .chain(export_backgrounds.iter().map(|token| {
                        (
                            token.to_string(),
                            document["export"][token]
                                .as_str()
                                .expect("export background")
                                .to_owned(),
                        )
                    }))
                    .collect::<Vec<_>>();
                for foreground_token in foreground_tokens {
                    let foreground = resolved_color(&document, foreground_token);
                    for (background_token, background) in &backgrounds {
                        let ratio = contrast(&foreground, background);
                        assert!(
                            ratio >= 4.5,
                            "{person_id} {accent}: {foreground_token}={foreground} on \
                             {background_token}={background} is only {ratio:.3}:1"
                        );
                    }
                }
            }
        }
    }
}
