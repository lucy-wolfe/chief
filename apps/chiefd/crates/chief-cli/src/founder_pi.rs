//! The Founder Pi session's argv and environment.
//!
//! Ported from the deleted `apps/cli/src/FounderPi.ts` — `founderPiArgs`,
//! `founderEnvironment`, `STRIPPED_ENV` — together with the four
//! `@chief/piing` resolvers it called (`resolvePiBinary`, `piingSkillsRoot`,
//! `piingExtensionsRoot`, `loopExtensionPath`). Every one of those four is a
//! path join against the launcher checkout root, which is why the port is a
//! page of joins rather than a second runtime.
//!
//! # Spawning Pi is legitimate; a TypeScript dispatcher is not
//!
//! `chief` used to reach a JavaScript runtime twice over: chiefd spawned
//! Bun against a 179-line TypeScript app, that app parsed one argv token, and
//! only then did it spawn Pi. Pi genuinely needs a JavaScript runtime — its CLI
//! starts `#!/usr/bin/env node` — so **that** child stays. The dispatcher in
//! front of it does not: it was a second claimant for chiefd's command surface,
//! and the one that answered `chief ls` with `unknown command 'ls'`. The argv
//! below is chiefd's decision, so chiefd is where it is built.
//!
//! `scripts/test/no-ts-cli-stub.test.mjs` fails if any Rust file shells out to
//! a JavaScript CLI again.
//!
//! # These paths agree with `chiefd-host` by construction, not by luck
//!
//! `chiefd_host::runtime_lifecycle::launcher_assets` derives the very same
//! `packages/piing/{skills,extensions}` locations when it materializes a
//! managed person's Pi home. The literals are stated once here and once there
//! because the two answer different questions (one session's argv versus a
//! whole company's materialization) — [`REQUIRED_CHECKOUT_MARKER`] is the
//! shared probe, and it is the same directory `launcher_assets` refuses a
//! launcher root over.

use std::path::Path;
use std::process::Command;

/// The variable the Founder extension reads to know it is the Founder.
const LAUNCH_MODE_ENV: &str = "CHIEFD_LAUNCH_MODE";

/// Its one value. There is one pre-company identity.
const LAUNCH_MODE_FOUNDER: &str = "founder";

/// Environment a Founder session must NOT inherit.
///
/// Every one of these names a company, a person, or a runtime that Founder is
/// not part of. Founder runs before any company exists; inheriting an
/// operator's ambient pointers would let a stray shell variable decide what a
/// brand-new company is attached to.
///
/// # Two deliberate decisions about the company DIRECTORY
///
/// `ORG_LAUNCHER_DATA_ROOT` is gone from this list because it is gone from the
/// product: nothing sets it and nothing reads it, so stripping it named a
/// variable that does not exist.
///
/// **`ORG_LAUNCHER_ORG_DIR` is deliberately NOT put in its place**, and it is
/// the one entry a reader will expect to find. It is the company directory, so
/// it looks exactly like the ambient pointer this list exists to strip — but it
/// is not ambient here. `chief` runs IN the directory the company will
/// occupy, this process stamps the Founder pane with that directory itself
/// (`daemon::COMPANY_DIR_ENV`, and the pane's own `-c`), and `chiefd-log` reads
/// it to put the Founder's jsonl in `<dir>/.chief/log/` — which is the log of
/// the very launch it is narrating. Stripping it would send the loudest 4½
/// minutes in the product back to writing nothing, which is the defect #1051
/// exists to close.
///
/// The distinction the list actually draws is INHERITED versus STAMPED: a value
/// this process resolved and set is a decision, and a value it merely found in
/// the operator's shell is a guess. Everything below is the second kind.
const STRIPPED_ENV: [&str; 4] = [
    "ORG_LAUNCHER_INTERCOM_LIFECYCLE",
    "ORG_LAUNCHER_ORGANIZATION",
    "ORG_LAUNCHER_PERSON",
    "ORG_LAUNCHER_ROOT",
];

/// The one directory whose presence proves a path is a launcher checkout.
///
/// Relative to the launcher root. Every asset path below is built from that
/// root without asking whether it exists, so the wrong root yields a Founder
/// with no `chiefd_launch_company` at all — a session that can discuss a
/// company forever and never create one. `chiefd_host`'s `launcher_assets`
/// refuses over exactly this directory, for exactly this reason.
pub(crate) const REQUIRED_CHECKOUT_MARKER: &str = "packages/piing/extensions";

/// The Founder's one skill, under `packages/piing/skills`.
///
/// The directory was `founder-launch` until the skill set was cut down to
/// `founder` + `manager`; the EXTENSION `founder-launch.ts` kept its own name.
const FOUNDER_SKILL: &str = "founder";

/// The session name Pi displays.
const FOUNDER_NAME: &str = "Founder";

/// The extensions a Founder session loads, under `packages/piing/extensions`.
///
/// Ordered as the deleted TypeScript ordered them.
const FOUNDER_EXTENSIONS: [&str; 3] = ["founder-launch.ts", "team-ui.ts", "tribes-welcome.ts"];

// TOMBSTONE: `PINNED_PI` (`node_modules/.bin/pi`) and `pi_binary`, which were
// rungs 1 and 2 of the Pi ladder — an operator's `TEAM_LAUNCHER_PI` pin, then
// the launcher checkout's own pinned build. Both are deleted. Pi is whatever
// the operator installed, found on `PATH` by
// `preflight::resolve_pi_runtime`, and the golden argv below takes the
// resolved program as a parameter rather than deriving one.

// TOMBSTONE: `FOUNDER_TOOLS`, an eight-name `--tools` allowlist. It was the
// last of four fences that made Founder a restricted appliance rather than the
// operator's own Pi -- and it filtered EVERY tool a loaded skill brought with
// it, so even lifting `--no-skills` alone would have left the operator's
// skills present and uncallable. Operator ruling: "Founder mode should be a
// regular pi instance that loads the current directory as its base dir with
// the extra founder's skills to launch."

/// The exact argv the Founder Pi session runs as, program included.
///
/// Byte-identical to the deleted `founderPiArgs()`; the golden vector below
/// pins that, captured from the TypeScript before it was deleted.
#[must_use]
pub(crate) fn founder_pi_argv(launcher_root: &Path, pi: &Path) -> Vec<String> {
    let skills = launcher_root.join("packages/piing/skills");
    let extensions = launcher_root.join("packages/piing/extensions");
    // EVERYTHING HERE IS ADDITIVE. Founder is the operator's ordinary Pi, in
    // the operator's own directory, with the founding skill and its extensions
    // ADDED -- not a restricted appliance.
    //
    // Four flags used to fence it off from the operator's own installation:
    // `--no-skills`, `--no-extensions`, `--no-context-files`, and a `--tools`
    // allowlist of eight names. The effect was reported live: in Founder mode
    // the `zipbox-browser` skill said it could not reach Chromium. It was not
    // failing to reach anything -- `--no-skills` meant it was never loaded,
    // and `--tools` would have filtered its tools out even if it had been.
    //
    // The framing this restores is already the ledger's for the CEO: that is
    // the OPERATOR's own Pi, not a managed agent. Founder was the one
    // pre-company session inconsistent with it.
    let mut argv = vec![
        pi.display().to_string(),
        "--skill".to_owned(),
        skills.join(FOUNDER_SKILL).display().to_string(),
    ];
    for extension in FOUNDER_EXTENSIONS {
        argv.push("--extension".to_owned());
        argv.push(extensions.join(extension).display().to_string());
    }
    argv.extend([
        // chiefd has already resolved and validated its launcher checkout
        // before this runs, so the project is approved explicitly rather than
        // left to Pi's interactive trust prompt, which a fresh Founder would
        // block on. KEPT: this is about not blocking a first run, not about
        // fencing the session.
        "--approve".to_owned(),
        "--name".to_owned(),
        FOUNDER_NAME.to_owned(),
    ]);
    argv
}

/// Apply the Founder environment: scrub what it must not inherit and declare
/// what it is. Pi keeps ownership of provider discovery and model metadata.
pub(crate) fn apply_founder_environment(command: &mut Command) {
    for name in STRIPPED_ENV {
        command.env_remove(name);
    }
    command.env(LAUNCH_MODE_ENV, LAUNCH_MODE_FOUNDER);
}

#[cfg(test)]
mod tests {
    use super::{
        apply_founder_environment, founder_pi_argv, FOUNDER_EXTENSIONS, FOUNDER_NAME,
        FOUNDER_SKILL, LAUNCH_MODE_ENV, LAUNCH_MODE_FOUNDER, REQUIRED_CHECKOUT_MARKER,
        STRIPPED_ENV,
    };
    use std::path::Path;

    /// The checkout root the golden vector below was captured from.
    const GOLDEN_ROOT: &str = "/root/chief-p3";

    /// THE GOLDEN ARGV — the literal output of the deleted TypeScript.
    ///
    /// Captured before `apps/cli` was deleted, by running
    /// `bun -e 'import { founderPiArgs } from "./apps/cli/src/FounderPi.ts";
    /// console.log(JSON.stringify(founderPiArgs()))'` in a real checkout at
    /// [`GOLDEN_ROOT`] with `TEAM_LAUNCHER_PI` unset. Reproducible by cloning
    /// the repo to that path and running the same line against
    /// `git show <this commit>~1:apps/cli/src/FounderPi.ts`.
    ///
    /// This is the regression test for the port. A green build proves the Rust
    /// compiles; only this proves it asks Pi for the same session.
    ///
    /// THE DIVERGENCES ARE DELIBERATE, and none of them is this port's: the
    /// capture names two extensions the product has since deleted — pi-loop
    /// (`6e1ddb78a`, reminders are the only recurrence mechanism now) and
    /// tavily-search (the operator's web-search deletion, which also took
    /// `tavily_search`/`tavily_extract` out of `--tools`). The vector is kept
    /// VERBATIM, with each divergence named and removed by [`golden`] below,
    /// rather than quietly re-captured — an edited golden is no longer
    /// evidence of anything.
    const GOLDEN_ARGV: [&str; 23] = [
        "/root/chief-p3/node_modules/.bin/pi",
        "--no-skills",
        "--skill",
        "/root/chief-p3/packages/piing/skills/founder-launch",
        "--no-extensions",
        "--extension",
        "/root/chief-p3/packages/piing/extensions/founder-launch.ts",
        "--extension",
        "/root/chief-p3/packages/piing/extensions/team-ui.ts",
        "--extension",
        "/root/chief-p3/packages/piing/extensions/tavily-search.ts",
        "--extension",
        "/root/chief-p3/packages/piing/extensions/tribes-welcome.ts",
        "--extension",
        "/root/chief-p3/packages/piing/extensions/zipbox-tribe-addons.ts",
        "--extension",
        "/root/chief-p3/node_modules/@koltmcbride/pi-loop/loop.ts",
        "--no-context-files",
        "--approve",
        "--tools",
        "read,bash,edit,write,grep,find,ls,tavily_search,tavily_extract,chiefd_launch_company",
        "--name",
        "Founder",
    ];

    /// The `--extension` pairs later commits retired, each named with the
    /// package or capability that took it away.
    ///
    /// `pi-loop` went with `6e1ddb78a` (reminders are the only recurrence
    /// mechanism now); `tavily-search` went with the operator's deletion of
    /// the whole web-search capability. Both are DERIVED removals from the
    /// verbatim capture rather than a re-capture, for the reason above: an
    /// edited golden is no longer evidence of anything.
    const RETIRED_EXTENSIONS: [[&str; 2]; 3] = [
        ["--extension", "/root/chief-p3/node_modules/@koltmcbride/pi-loop/loop.ts"],
        ["--extension", "/root/chief-p3/packages/piing/extensions/tavily-search.ts"],
        // Deleted with provider/model management: `zipbox-tribe-addons.ts` was
        // the custom-provider transport registration, and with no
        // `ORG_CUSTOM_PROVIDERS` and no chief-selected route it has no subject.
        // The Founder resolves its model from the operator's own Pi settings,
        // which is what it did for its BUILTIN providers all along.
        ["--extension", "/root/chief-p3/packages/piing/extensions/zipbox-tribe-addons.ts"],
    ];

    /// The one `--skill` path the capture spells with its pre-rename directory
    /// name, and the name it carries today.
    ///
    /// Derived from the verbatim capture for the same reason the retired
    /// extensions are: the skill directory `packages/piing/skills/founder-launch`
    /// was renamed to `packages/piing/skills/founder` when the skill set was cut
    /// down to `founder` + `manager`. Nothing about the argv the port produces
    /// changed except that one path element, so the capture stays as captured
    /// and the rename is applied to it here.
    const RENAMED_SKILL: [&str; 2] = [
        "/root/chief-p3/packages/piing/skills/founder-launch",
        "/root/chief-p3/packages/piing/skills/founder",
    ];

    /// The tool ids `tavily-search.ts` registered, removed from the `--tools`
    /// element by the same derivation: a Founder that cannot load the
    /// extension must not be told it may call its tools.
    const RETIRED_WEB_SEARCH_TOOLS: [&str; 2] = ["tavily_search", "tavily_extract"];

    /// The golden vector, less the entries later commits retired.
    fn golden() -> Vec<String> {
        let mut expected: Vec<String> =
            GOLDEN_ARGV.iter().map(|value| (*value).to_owned()).collect();
        for retired in RETIRED_EXTENSIONS {
            let at = expected
                .windows(2)
                .position(|pair| pair == retired)
                .expect("the golden capture must still contain the entry being dropped");
            expected.drain(at..at + 2);
        }
        let [captured_skill, renamed_skill] = RENAMED_SKILL;
        let skill = expected
            .iter_mut()
            .find(|value| *value == captured_skill)
            .expect("the golden capture must still contain the skill path being renamed");
        *skill = renamed_skill.to_owned();
        let tools = expected
            .iter_mut()
            .find(|value| value.contains("chiefd_launch_company"))
            .expect("the golden capture must still carry the --tools element");
        let kept = tools
            .split(',')
            .filter(|id| !RETIRED_WEB_SEARCH_TOOLS.contains(id))
            .collect::<Vec<&str>>()
            .join(",");
        *tools = kept;
        expected
    }

    /// THE FENCES ARE GONE, AND EXACTLY THE ADDITIVE FLAGS REMAIN.
    ///
    /// This test used to assert `argv == golden()` -- byte-identical to the
    /// deleted TypeScript -- and that was the right invariant while the only
    /// question was whether the Rust port asked Pi for the same session. It is
    /// not the question any more: the operator ruled that Founder should be a
    /// regular Pi instance with the founder skill added, so the argv now
    /// DELIBERATELY differs from the capture, and an assertion of sameness
    /// would be asserting the bug.
    ///
    /// The capture is KEPT rather than deleted, and it is still evidence --
    /// of what the four fences were, so this test can name them and prove
    /// each one is absent. `golden()` is the ported shape; the difference
    /// between it and the live argv is the whole of this change, checked
    /// element by element rather than described.
    #[test]
    fn founder_is_the_operators_own_pi_with_the_founding_skill_added() {
        let root = Path::new(GOLDEN_ROOT);
        let argv = founder_pi_argv(root, &Path::new(GOLDEN_ROOT).join("node_modules/.bin/pi"));

        // THE FENCES, by name. Each one is why a skill the operator installed
        // was absent or uncallable in a Founder session.
        for fence in ["--no-skills", "--no-extensions", "--no-context-files", "--tools"] {
            assert!(
                !argv.iter().any(|value| value == fence),
                "{fence} fences Founder off from the operator's own Pi installation: {argv:?}"
            );
        }

        // THE ADDITIONS, exactly. The founding skill, its extensions, the
        // trust approval that stops a first run blocking, and the name.
        let expected: Vec<String> = {
            let skills = root.join("packages/piing/skills");
            let extensions = root.join("packages/piing/extensions");
            let mut want = vec![
                root.join("node_modules/.bin/pi").display().to_string(),
                "--skill".to_owned(),
                skills.join(FOUNDER_SKILL).display().to_string(),
            ];
            for extension in FOUNDER_EXTENSIONS {
                want.push("--extension".to_owned());
                want.push(extensions.join(extension).display().to_string());
            }
            want.extend(["--approve".to_owned(), "--name".to_owned(), FOUNDER_NAME.to_owned()]);
            want
        };
        assert_eq!(
            argv, expected,
            "the argv is the program plus the additive flags, and nothing else"
        );

        // AND THE DIFFERENCE IS ONLY SUBTRACTION PLUS THE RETIREMENTS. Every
        // element still present was present in the ported shape too -- this
        // change removes fences, it does not invent flags.
        let ported = golden();
        for value in &argv {
            assert!(
                ported.contains(value),
                "{value} is in the live argv but was never in the ported shape -- this change is \
                 subtraction, and a new element needs its own reason: {argv:?}"
            );
        }
    }

    /// THE RESOLVED PI IS THE PROGRAM, whatever it is, and it changes nothing
    /// else.
    ///
    /// This asserted that a `TEAM_LAUNCHER_PI` pin beat the checkout's own
    /// build. Both rungs are deleted and the program now arrives from `PATH`,
    /// so the property that survives — and the one that always mattered — is
    /// that the program is the only element the caller can move.
    #[test]
    fn the_resolved_program_replaces_only_the_program() {
        let root = Path::new(GOLDEN_ROOT);
        let argv = founder_pi_argv(root, Path::new("/opt/pi/bin/pi"));
        // Compared against ANOTHER CALL rather than against the ported
        // capture: the property is that the program is the only element the
        // caller can move, and holding it against a fixed vector would make
        // this test fail for any deliberate argv change as well -- which is
        // the failure the golden test above just had.
        let other = founder_pi_argv(root, &Path::new(GOLDEN_ROOT).join("node_modules/.bin/pi"));
        assert_eq!(argv[0], "/opt/pi/bin/pi", "the resolved runtime is argv[0]");
        assert_eq!(argv[1..], other[1..], "nothing but the program may move");
    }

    /// Every path in the argv hangs off the launcher root — no PATH lookup, no
    /// operator home, no ambient override.
    #[test]
    fn every_resource_is_resolved_under_the_launcher_root() {
        let root = Path::new("/somewhere/else");
        let argv = founder_pi_argv(root, &root.join("node_modules/.bin/pi"));
        for value in &argv {
            if value.starts_with('/') {
                assert!(value.starts_with("/somewhere/else/"), "{value} escaped the checkout");
            }
        }
        let marker = format!("/somewhere/else/{REQUIRED_CHECKOUT_MARKER}/founder-launch.ts");
        assert!(argv.contains(&marker), "the launch tool's extension must be loaded: {argv:?}");
    }

    /// The founding tool's extension is loaded, and a retired capability's is
    /// not.
    ///
    /// This used to assert that the tool ALLOWLIST and the extension list
    /// moved together -- Pi filtered extension tools through `--tools`, so a
    /// tool named without its extension was one the model could see and never
    /// call. There is no allowlist any more, so that half has no subject; what
    /// survives is the half that still binds, which is that the extension
    /// registering `chiefd_launch_company` is actually loaded. Without it
    /// Founder agrees to launch a company and then cannot.
    #[test]
    fn the_founding_tools_extension_is_loaded_and_retired_ones_are_not() {
        assert!(FOUNDER_EXTENSIONS.contains(&"founder-launch.ts"));
        // The web-search capability is deleted, so Founder does not load the
        // extension that registered its tools.
        assert!(!FOUNDER_EXTENSIONS.contains(&"tavily-search.ts"));
    }

    /// The scrub list names INHERITED pointers, never credentials — and never
    /// a value this process stamped itself.
    #[test]
    fn the_stripped_environment_is_the_ambient_company_pointers() {
        assert_eq!(STRIPPED_ENV.len(), 4);
        for name in STRIPPED_ENV {
            assert!(name.starts_with("ORG_LAUNCHER_"), "{name} is not an ambient company pointer");
        }
        // THE COMPANY DIRECTORY IS STAMPED, NOT INHERITED, so it is not
        // stripped. `chief` runs in the directory the company will occupy
        // and sets this itself; scrubbing it would put the Founder's own jsonl
        // back in `$HOME` instead of `<dir>/.chief/log/` — the silence #1051
        // exists to end, on the surface with the longest wait in the product.
        assert!(
            !STRIPPED_ENV.contains(&chiefd_log::sink::COMPANY_DIR_ENV),
            "the company directory is this process's decision, not an operator's stray variable"
        );
        // The declaration is the other half: Founder must know it is Founder.
        assert_eq!(LAUNCH_MODE_ENV, "CHIEFD_LAUNCH_MODE");
        assert_eq!(LAUNCH_MODE_FOUNDER, "founder");
    }

    /// The environment is applied to a real command, not merely described.
    #[test]
    fn the_founder_environment_is_applied_to_the_spawned_command() {
        let mut command = std::process::Command::new("/bin/true");
        command.env("ORG_LAUNCHER_ORGANIZATION", "someone-elses-company");
        apply_founder_environment(&mut command);
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(
            envs.contains(&("ORG_LAUNCHER_ORGANIZATION".to_owned(), None)),
            "a stripped name must be REMOVED from the child, not merely left unset here: {envs:?}"
        );
        assert!(envs.contains(&(LAUNCH_MODE_ENV.to_owned(), Some(LAUNCH_MODE_FOUNDER.to_owned()))));
        assert!(
            !envs.iter().any(|(key, _)| key == "PI_OFFLINE"),
            "Chief must not override Pi's own provider metadata refresh: {envs:?}"
        );
    }

    /// THE RULE: the Founder is plain Pi, including Pi's own model refresh.
    #[test]
    fn the_founder_pane_does_not_freeze_pi_model_metadata() {
        let mut command = std::process::Command::new("/bin/true");
        apply_founder_environment(&mut command);
        let offline = command
            .get_envs()
            .find(|(key, _)| key.to_string_lossy() == "PI_OFFLINE")
            .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()));
        assert_eq!(offline, None, "Chief must not set Pi's offline override");
    }

    /// And setting it took nothing away. The scrub is the other half of this
    /// function and a Founder that inherited a company pointer is a launch
    /// aimed at somebody else's company.
    #[test]
    fn leaving_pi_discovery_alone_did_not_stop_the_environment_being_scrubbed() {
        let mut command = std::process::Command::new("/bin/true");
        for name in STRIPPED_ENV {
            command.env(name, "inherited-from-an-operator-shell");
        }
        apply_founder_environment(&mut command);
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for name in STRIPPED_ENV {
            assert!(
                envs.contains(&(name.to_owned(), None)),
                "{name} must still be REMOVED from the Founder child: {envs:?}"
            );
        }
    }
}
