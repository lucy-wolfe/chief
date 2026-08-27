//! `chief upgrade` — replace this install with the latest release.
//!
//! # Why the product owns its own updater
//!
//! An external updater could download a tarball and unpack it. It could not do
//! the two things that make this verb worth writing:
//!
//!   * **The Pi floor.** chief declares a minimum Pi version
//!     (`host_primitives::pi_floor`). A release that needs a newer Pi than the
//!     box has must say so BEFORE it changes anything, and it must be able to
//!     offer Pi's own updater — which only ever installs LATEST, a fact the
//!     prompt states out loud rather than implying.
//!   * **The live-daemon rule.** A running company holds its binaries open.
//!     Re-pointing a symlink leaves those processes alone (Unix keeps an open
//!     inode alive), so an upgrade never interrupts a company — and the
//!     operator, not this verb, decides when to restart one.
//!
//! # The order, and why the swap is last
//!
//! Download, verify, unpack to a staging directory, rename into place, PROBE
//! THE NEW BINARIES, and only then re-point the symlinks. Every step before the
//! swap is reversible by deleting a directory, and the swap itself is a
//! `rename(2)`. There is no window in which a broken binary is live, which is
//! why `--rollback` exists for a bad RELEASE rather than for a bad upgrade.
//!
//! # curl, deliberately, and this is not the transport rule being broken
//!
//! `chief-cli`'s Cargo manifest says "hyper, never `reqwest`, and never a
//! shelled-out `curl` — a curl subprocess is a thread that cannot be
//! cancelled". That rule is about the LOOPBACK JSON transport this client uses
//! inside a resident actuator loop, thousands of times, under a cancellation
//! budget. This is a one-shot foreground operator verb that must speak TLS to
//! github.com, and the alternative is linking a TLS stack into every binary for
//! one verb nobody runs in a loop. `curl` is already a prerequisite of the
//! installer that put this binary on the box.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use host_primitives::pi_floor::{version_meets, MINIMUM_PI_VERSION};

/// The repository releases are cut from.
const RELEASES_LATEST: &str = "https://api.github.com/repos/tribes-protocol/chief/releases/latest";

/// Exit code for "an upgrade exists", so a script can ask without parsing text.
///
/// NOT 1. A non-zero exit that means "there is news" must be distinguishable
/// from a non-zero exit that means "this did not work", or every wrapper that
/// checks it treats a network failure as an available upgrade.
const UPGRADE_AVAILABLE_EXIT: u8 = 10;

/// What the operator asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Report and change nothing. Exit 0 when current, [`UPGRADE_AVAILABLE_EXIT`] otherwise.
    Check,
    /// Install the latest release over this one.
    Install {
        /// Proceed even when the installed Pi is below the target's floor.
        skip_pi_check: bool,
    },
    /// Re-point the symlinks at the previously installed version.
    Rollback,
}

/// Parse `chief upgrade`'s own flags.
///
/// # Errors
/// The flag that was not understood, so the caller can name it.
pub(crate) fn parse_mode(rest: &[String]) -> Result<Mode, String> {
    let mut check = false;
    let mut rollback = false;
    let mut skip_pi_check = false;
    for argument in rest {
        match argument.as_str() {
            "--check" => check = true,
            "--rollback" => rollback = true,
            "--skip-pi-check" => skip_pi_check = true,
            other => return Err(other.to_owned()),
        }
    }
    // Two verbs in one argv is a typo, not an instruction. Refusing beats
    // silently honouring whichever the match arms happen to reach first.
    if check && rollback {
        return Err("--check --rollback".to_owned());
    }
    if rollback {
        return Ok(Mode::Rollback);
    }
    if check {
        // `--skip-pi-check` with `--check` asks to skip a gate that never runs.
        // Accepted silently rather than refused: it is what a script that
        // always passes the flag will do, and there is nothing to get wrong.
        return Ok(Mode::Check);
    }
    Ok(Mode::Install { skip_pi_check })
}

/// The release asset for one version and target.
#[must_use]
pub(crate) fn asset_name(version: &str, target: &str) -> String {
    format!("chief-{version}-{target}.tar.gz")
}

/// This binary's build target, as the release assets spell it.
#[must_use]
pub(crate) fn host_target() -> Option<&'static str> {
    // Named per (os, arch) rather than derived, because the release workflow's
    // matrix is the authority for which of these exist and there are exactly
    // four. An unlisted pair is `None` — a refusal that names the platform,
    // never a download of an asset that cannot be there.
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

/// The version this binary reports, which is also the directory it lives in.
#[must_use]
pub(crate) fn installed_version() -> &'static str {
    env!("CHIEF_VERSION")
}

/// A release tag, with the `v` a tag carries and a version does not.
///
/// `v2.0.7` and `2.0.7` name the same release and are used in different places
/// — the tag names the GitHub release, the version names the install directory
/// and what `chief --version` prints. Normalising in one function is what stops
/// a comparison of the two reporting a perfectly current install as stale.
#[must_use]
pub(crate) fn version_of_tag(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// The digest `SHA256SUMS` records for `asset`.
///
/// `sha256sum` format: `<hex>  <name>`, two spaces. Matched on the NAME rather
/// than by position, because a file listing four assets in a matrix-dependent
/// order is a file whose order nobody controls.
#[must_use]
pub(crate) fn expected_digest(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once("  ")?;
        (name.trim() == asset).then(|| digest.trim().to_owned())
    })
}

/// What the Pi gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PiGate {
    /// At or above the target's floor. Proceed.
    Meets,
    /// Below it. The two numbers, for the prompt.
    Below { installed: String, floor: String },
    /// No version could be read from `pi --version`, or Pi did not answer.
    ///
    /// PROCEEDS, and that is deliberate. A Pi that declines to name itself is a
    /// Pi nobody can judge, and blocking an upgrade on a formatting change
    /// upstream would strand every user behind a cosmetic release of somebody
    /// else's program.
    Unknown,
}

/// Judge the installed Pi against the TARGET release's floor.
///
/// The target's floor, never this build's: the release being installed is the
/// one whose requirement matters, and it travels in that release's
/// `manifest.json` precisely so this can be asked before anything is swapped.
#[must_use]
pub(crate) fn pi_gate(reported: Option<&str>, floor: &str) -> PiGate {
    let Some(version) = reported else { return PiGate::Unknown };
    match version_meets(version, floor) {
        Some(true) => PiGate::Meets,
        Some(false) => PiGate::Below { installed: version.to_owned(), floor: floor.to_owned() },
        None => PiGate::Unknown,
    }
}

/// What a release's `manifest.json` says that this verb reads.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Manifest {
    /// The version, which must equal the directory it was unpacked into.
    pub(crate) version: String,
    /// The minimum Pi version THIS release needs.
    pub(crate) pi_floor: String,
}

/// The release the API reports as latest.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct LatestRelease {
    /// `v2.0.7`.
    pub(crate) tag_name: String,
}

/// One row of beacond's `/v1/list`: a company and its daemon's base URL.
#[derive(Debug, Clone, serde::Deserialize)]
struct CompanyEntry {
    dir: String,
    slug: String,
    #[serde(default)]
    url: Option<String>,
}

/// beacond's `/v1/list` in its object form.
#[derive(Debug, Clone, serde::Deserialize)]
struct CompanyListing {
    companies: Vec<CompanyEntry>,
}

/// One company's daemon, observed through beacond and its health surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompanyVersion {
    /// The company's display slug.
    pub(crate) slug: String,
    /// The company's directory — its identity, and where the operator runs the
    /// restart.
    pub(crate) dir: String,
    /// The daemon's reported release version, or `None` when the company has no
    /// live daemon URL, the daemon did not answer, or it is too old to report
    /// one. None is "unknown", never evidence of a version — the same stance
    /// the client's skew check takes.
    pub(crate) version: Option<String>,
}

/// The companies whose live daemon runs a version other than `installed`.
///
/// A company with no readable version is skipped: a daemon that cannot say what
/// it is is not evidence that it is stale.
#[must_use]
pub(crate) fn stale_companies(observed: &[CompanyVersion], installed: &str) -> Vec<CompanyVersion> {
    observed
        .iter()
        .filter(|company| company.version.as_deref().is_some_and(|version| version != installed))
        .cloned()
        .collect()
}

/// The version-directory names a live daemon still runs from. Prune must keep
/// these even past keep-two: the binaries survive deletion as open inodes, but
/// `resources/` is read by PATH at materialization, so deleting a live
/// version's directory breaks that company at its next materialization.
#[must_use]
pub(crate) fn live_version_dirs(observed: &[CompanyVersion]) -> std::collections::HashSet<String> {
    observed.iter().filter_map(|company| company.version.clone()).collect()
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// Run `chief upgrade`.
#[must_use]
pub(crate) fn run(mode: Mode) -> ExitCode {
    match execute(mode) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("chief upgrade: {message}");
            ExitCode::FAILURE
        }
    }
}

fn execute(mode: Mode) -> Result<ExitCode, String> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or("HOME is unset, so there is no install to upgrade")?;
    match mode {
        Mode::Rollback => rollback(&home).map(|()| ExitCode::SUCCESS),
        Mode::Check => check(),
        Mode::Install { skip_pi_check } => {
            install(&home, skip_pi_check).map(|()| ExitCode::SUCCESS)
        }
    }
}

fn check() -> Result<ExitCode, String> {
    let installed = installed_version();
    println!("installed: {installed}");
    println!("Pi installed: {}", reported_pi_version().unwrap_or_else(|| "unreported".to_owned()));
    let tag = latest_tag();
    let latest = tag.as_deref().map(version_of_tag);
    match &latest {
        Ok(version) => println!("latest: {version}"),
        // A NETWORK FAILURE IS NOT "UP TO DATE" — a box behind a proxy or past
        // GitHub's unauthenticated rate limit must be told plainly, and exit
        // non-zero-but-not-10 so a wrapper never reads a failed check as news.
        Err(why) => println!("latest: could not be resolved ({why})"),
    }
    // H.3-R1: the floor that MATTERS is the TARGET release's, not this build's
    // — read it from that release's published `manifest.json` asset. The
    // labeled this-build floor is the fallback when that asset cannot be
    // reached (an older release that predates the asset, or no network).
    match tag.as_deref().ok().and_then(target_pi_floor) {
        Some(floor) => println!("Pi floor (latest release): {floor}"),
        None => println!("Pi floor (this build): {MINIMUM_PI_VERSION}"),
    }
    let verdict = check_verdict(installed, latest.ok());
    match verdict {
        CheckVerdict::Current => println!("chief is up to date."),
        CheckVerdict::UpgradeAvailable => println!("An upgrade is available. Run 'chief upgrade'."),
        CheckVerdict::CheckFailed => {
            println!("The update check did not complete; chief was not changed.")
        }
    }
    Ok(verdict.exit_code())
}

/// The three ways `--check` can end, and their exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckVerdict {
    /// Installed equals latest. Exit 0.
    Current,
    /// An upgrade exists. Exit [`UPGRADE_AVAILABLE_EXIT`].
    UpgradeAvailable,
    /// The check itself could not complete. Exit 1 — never 0 (which a wrapper
    /// reads as "current") and never 10 (which it reads as "upgrade exists").
    CheckFailed,
}

impl CheckVerdict {
    fn exit_code(self) -> ExitCode {
        match self {
            Self::Current => ExitCode::SUCCESS,
            Self::UpgradeAvailable => ExitCode::from(UPGRADE_AVAILABLE_EXIT),
            Self::CheckFailed => ExitCode::FAILURE,
        }
    }
}

/// Decide `--check`'s verdict. `latest` is `None` when it could not be resolved.
#[must_use]
pub(crate) fn check_verdict(installed: &str, latest: Option<&str>) -> CheckVerdict {
    match latest {
        None => CheckVerdict::CheckFailed,
        Some(version) if version == installed => CheckVerdict::Current,
        Some(_) => CheckVerdict::UpgradeAvailable,
    }
}

fn install(home: &Path, skip_pi_check: bool) -> Result<(), String> {
    let target = host_target()
        .ok_or("chief publishes releases for macOS and Linux only, on x86_64 and arm64")?;
    let tag = latest_tag()?;
    let version = version_of_tag(&tag).to_owned();
    if version == installed_version() {
        println!("chief {version} is already current.");
        return Ok(());
    }

    let staging = tempdir_under(&crate::paths::versions_dir(home), &version)?;
    let asset = asset_name(&version, target);
    let base = format!("https://github.com/tribes-protocol/chief/releases/download/{tag}");
    let tarball = staging.join(&asset);
    download(&format!("{base}/{asset}"), &tarball)?;
    let sums_path = staging.join("SHA256SUMS");
    download(&format!("{base}/SHA256SUMS"), &sums_path)?;

    let sums = std::fs::read_to_string(&sums_path).map_err(|error| error.to_string())?;
    let expected = expected_digest(&sums, &asset)
        .ok_or_else(|| format!("SHA256SUMS names no {asset}; refusing to install it"))?;
    let actual = digest_of(&tarball)?;
    if actual != expected {
        // NEVER unpack a tarball whose digest disagrees. This is the one check
        // between a compromised or truncated download and an executable that
        // replaces the operator's own client.
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "the downloaded {asset} does not match SHA256SUMS (expected {expected}, got {actual}). \
             Nothing was installed."
        ));
    }

    unpack(&tarball, &staging)?;
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(staging.join("manifest.json"))
            .map_err(|error| format!("the release has no readable manifest.json: {error}"))?,
    )
    .map_err(|error| format!("the release's manifest.json could not be read: {error}"))?;
    if manifest.version != version {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "release {version} contains a manifest for {}; refusing to install a mismatched \
             artifact",
            manifest.version
        ));
    }

    if !skip_pi_check {
        pi_gate_or_abort(&manifest.pi_floor, &staging)?;
    }

    // PROBE THE NEW BINARIES IN STAGING, BEFORE ANYTHING IS PUBLISHED. A binary
    // that cannot run its own `--version` must never reach `versions/<v>` at
    // all: `resource_root_from_exe` resolves a version directory by EXISTENCE,
    // so a broken one renamed into place would be a half-written version
    // directory the next boot would try to resolve — the exact thing the
    // stage-then-rename discipline exists to prevent. `--version` needs no
    // resources, so probing from the staging tree is safe. A failure here
    // removes staging and leaves the install untouched: the swap never
    // happened.
    if let Err(error) = probe(&staging.join("bin/chief"), &version)
        .and_then(|()| probe(&staging.join("bin/chiefd"), &version))
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    // STAGE-THEN-RENAME, now that the staged binaries are proven. A killed
    // upgrade leaves a `.staging-*` directory the next run sweeps, never a
    // half-written version directory.
    let destination = crate::paths::version_dir(home, &version);
    std::fs::remove_dir_all(&destination).ok();
    // The client OWNS `~/.chief`; publish-by-rename of a version tree it just
    // staged and probed is exactly the atomic swap the seam sanctions, on the
    // client's own install rather than a company's files (clippy.toml §5.6).
    #[allow(clippy::disallowed_methods)]
    let published = std::fs::rename(&staging, &destination);
    published.map_err(|error| format!("could not publish {version}: {error}"))?;

    let previous = installed_version().to_owned();
    point_bin(home, &destination)?;
    record_previous(home, &previous)?;
    // Enumerate live daemons ONCE: prune must not delete a version one still
    // runs (H.5.6), and the report names the ones still on an older version
    // (H.6.1). `None` means beacond could not be reached — prune then keeps
    // everything and the report says nothing.
    let observed = observe_company_versions();
    prune_old_versions(home, &version, &previous, observed.as_deref());

    println!("chief {previous} → {version}");
    println!(
        "Companies that are already running keep the binaries they started with. Restart each \
         one with 'chief stop && chief attach' in its directory when convenient — nothing is \
         broken until you do."
    );
    report_stale_companies(&version, observed.as_deref());
    Ok(())
}

/// What the Pi gate's DECISION logic resolved to, before any effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PiGateOutcome {
    /// Proceed with the upgrade.
    Proceed,
    /// Abort with this operator-facing message. The caller removes staging.
    Abort(String),
}

/// The Pi gate's whole decision, as PURE logic over injected effects.
///
/// The three product rules live here so they can be tested without a terminal
/// or a real `pi`: a DECLINE removes nothing of Pi and names `--skip-pi-check`;
/// a FAILED `pi update` leaves chief untouched and surfaces Pi's own error; and
/// — the case the plan names and the first cut skipped — a `pi update` that
/// SUCCEEDS but leaves Pi still below the floor is a chief RELEASE bug (Pi's
/// latest is older than chief requires), so it refuses with "file an issue"
/// rather than looping. `reprobe` supplies the gate as re-read AFTER the
/// update, which is the only way to see that last case.
pub(crate) fn resolve_pi_gate(
    gate: PiGate,
    confirmed: bool,
    run_pi_update: impl FnOnce() -> Result<(), String>,
    reprobe: impl FnOnce() -> PiGate,
) -> PiGateOutcome {
    let PiGate::Below { .. } = gate else { return PiGateOutcome::Proceed };
    if !confirmed {
        return PiGateOutcome::Abort(
            "nothing was changed. Update Pi yourself with 'pi update' (or \
             'npm install -g --ignore-scripts @earendil-works/pi-coding-agent'), then run 'chief \
             upgrade' again. To upgrade chief anyway, run 'chief upgrade --skip-pi-check'."
                .to_owned(),
        );
    }
    if let Err(why) = run_pi_update() {
        return PiGateOutcome::Abort(why);
    }
    // RE-PROBE. `pi update` installs Pi's LATEST; if that is still below this
    // release's floor, no amount of updating Pi will satisfy it — the release
    // asked for a Pi that does not exist yet, which is chief's bug, not the
    // operator's.
    match reprobe() {
        PiGate::Below { installed, floor } => PiGateOutcome::Abort(format!(
            "Pi is now {installed}, still below this release's floor {floor} — Pi's latest is \
             older than chief requires. This is a chief release bug; please file an issue. \
             Nothing was changed."
        )),
        _ => PiGateOutcome::Proceed,
    }
}

/// Run `pi update`, mapping its outcome to the abort message on failure.
fn run_pi_update() -> Result<(), String> {
    match Command::new("pi").arg("update").status() {
        Ok(status) if status.success() => Ok(()),
        // NO RETRY LOOP, and chief is left exactly as it was. Pi's own error is
        // the useful output here; a wrapper that swallowed it and tried again
        // would produce a slower failure with less information.
        Ok(status) => Err(format!(
            "'pi update' exited {}. chief was not changed. Read Pi's output above, or run \
             'npm install -g --ignore-scripts @earendil-works/pi-coding-agent' yourself.",
            status.code().unwrap_or(-1)
        )),
        Err(error) => {
            Err(format!("'pi update' could not be started: {error}. chief was not changed."))
        }
    }
}

/// The Pi gate, wired to the real terminal, `pi`, and staging directory.
fn pi_gate_or_abort(floor: &str, staging: &Path) -> Result<(), String> {
    let gate = pi_gate(reported_pi_version().as_deref(), floor);
    if let PiGate::Below { installed, floor } = &gate {
        println!("This release needs Pi {floor} or newer. This box has Pi {installed}.");
        // PI'S UPDATER CANNOT TARGET A VERSION. `pi update` asks pi.dev for the
        // latest and installs that; there is no `pi update <version>`. Saying
        // so is the difference between an informed yes and a surprise.
        println!("Pi's own updater installs the LATEST Pi, not a specific version.");
    }
    // FAIL CLOSED, through the same decision the destructive verbs use: a
    // non-interactive caller that cannot be asked is a caller that did not say
    // yes. `chief upgrade` in a script must never run somebody else's installer
    // because nobody was there to decline it.
    let confirmed =
        crate::confirm::decide("Run 'pi update' now? [y/N] ", false, &crate::confirm::Terminal)
            == crate::confirm::Confirmation::Confirmed;
    let floor = floor.to_owned();
    match resolve_pi_gate(gate, confirmed, run_pi_update, || {
        pi_gate(reported_pi_version().as_deref(), &floor)
    }) {
        PiGateOutcome::Proceed => Ok(()),
        PiGateOutcome::Abort(message) => {
            let _ = std::fs::remove_dir_all(staging);
            Err(message)
        }
    }
}

fn rollback(home: &Path) -> Result<(), String> {
    let record = crate::paths::install_state_dir(home).join("previous");
    let previous = std::fs::read_to_string(&record)
        .map_err(|error| {
            format!("no previous version is recorded at {}: {error}", record.display())
        })?
        .trim()
        .to_owned();
    if previous.is_empty() {
        return Err(format!("the record at {} is empty", record.display()));
    }
    let destination = crate::paths::version_dir(home, &previous);
    if !destination.is_dir() {
        return Err(format!(
            "version {previous} is no longer installed at {}. Only the last two versions are \
             kept.",
            destination.display()
        ));
    }
    let current = installed_version().to_owned();
    // Both binaries, like the forward install: a rollback that re-points a
    // `chief` that runs and a `chiefd` that does not is the same broken pair,
    // just reached from the other direction.
    probe(&destination.join("bin/chief"), &previous)?;
    probe(&destination.join("bin/chiefd"), &previous)?;
    point_bin(home, &destination)?;
    record_previous(home, &current)?;
    println!("chief {current} → {previous} (rolled back)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Host effects
// ---------------------------------------------------------------------------

fn reported_pi_version() -> Option<String> {
    let output = Command::new("pi").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).lines().next().map(|line| line.trim().to_owned())
}

fn latest_tag() -> Result<String, String> {
    let body = fetch(RELEASES_LATEST)?;
    let release: LatestRelease = serde_json::from_str(&body)
        .map_err(|error| format!("the releases API answered something unreadable: {error}"))?;
    Ok(release.tag_name)
}

/// The Pi floor a release DECLARES, read from its standalone `manifest.json`
/// asset (H.3-R1). `None` when the asset is unreachable or unreadable — an
/// older release that predates the asset, or no network — so `--check` can
/// fall back to this build's floor rather than refuse.
fn target_pi_floor(tag: &str) -> Option<String> {
    let url =
        format!("https://github.com/tribes-protocol/chief/releases/download/{tag}/manifest.json");
    let manifest: Manifest = serde_json::from_str(&fetch(&url).ok()?).ok()?;
    Some(manifest.pi_floor)
}

fn fetch(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json", url])
        .output()
        .map_err(|error| format!("curl could not be started: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not reach {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn download(url: &str, into: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(into)
        .arg(url)
        .status()
        .map_err(|error| format!("curl could not be started: {error}"))?;
    if !status.success() {
        return Err(format!("could not download {url}"));
    }
    Ok(())
}

/// The box-wide beacond's base URL, resolved exactly as the client does:
/// `BEACOND_URL` if set, else beacond's one declared default. This runs before
/// any tokio runtime, so it curls beacond rather than using the async client.
fn beacond_base() -> String {
    std::env::var("BEACOND_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(beacond::config::default_url)
}

/// One daemon's reported version, off its `/v1/docs/health` body.
///
/// `curl` WITHOUT `-f`: a not-ready daemon answers 503, and its body still
/// carries `version` (the health handler stamps it on every status), so a
/// dropped `-f` reads the version a `-f` would have thrown away. `None` on a
/// transport failure or a body with no version — both "unknown".
fn daemon_health_version(base_url: &str) -> Option<String> {
    let url = format!("{}/v1/docs/health", base_url.trim_end_matches('/'));
    let output = Command::new("curl").arg("-sSL").arg(&url).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).ok()?;
    value.get("version").and_then(serde_json::Value::as_str).map(str::to_owned)
}

/// Every company beacond knows, with each live daemon's reported version.
///
/// `None` means beacond ITSELF could not be reached — the caller then knows it
/// has NO information, which is different from "beacond answered, zero
/// companies". Prune treats `None` as "prune nothing this run"; the report
/// treats it as "say nothing". Sequential curls to loopback, deliberately: this
/// is a one-shot foreground verb over a handful of companies, not a loop.
fn observe_company_versions() -> Option<Vec<CompanyVersion>> {
    let body = fetch(&format!("{}/v1/list", beacond_base())).ok()?;
    let listing: CompanyListing = serde_json::from_str(&body).ok()?;
    Some(
        listing
            .companies
            .into_iter()
            .map(|entry| CompanyVersion {
                version: entry.url.as_deref().and_then(daemon_health_version),
                slug: entry.slug,
                dir: entry.dir,
            })
            .collect(),
    )
}

/// After a swap: name the companies still running an older daemon (H.6.1).
///
/// Skips silently when beacond could not be enumerated — a report it cannot
/// build honestly is one it does not print.
fn report_stale_companies(installed: &str, observed: Option<&[CompanyVersion]>) {
    let Some(observed) = observed else { return };
    let stale = stale_companies(observed, installed);
    if stale.is_empty() {
        return;
    }
    println!(
        "Still running an older chiefd — restart each with 'chief stop && chief attach' in its \
         directory when convenient; each keeps running until you do:"
    );
    for company in stale {
        let version = company.version.as_deref().unwrap_or("unknown");
        println!("  {} ({version}) — {}", company.slug, company.dir);
    }
}

fn digest_of(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    // sha2 0.11 returns a `hybrid_array::Array`, which does not implement
    // `LowerHex` — so `format!("{:x}", …)` does not compile. The same
    // lower-case, zero-padded encoding `sha256sum` prints (and `SHA256SUMS`
    // carries) is written out by hand, because that file is what this digest is
    // compared against.
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn unpack(tarball: &Path, into: &Path) -> Result<(), String> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(into)
        .status()
        .map_err(|error| format!("tar could not be started: {error}"))?;
    if !status.success() {
        return Err("the release archive could not be unpacked".to_owned());
    }
    // Housekeeping inside the client's own staging directory, not a company's
    // files (clippy.toml README §5.6).
    #[allow(clippy::disallowed_methods)]
    std::fs::remove_file(tarball).ok();
    Ok(())
}

fn tempdir_under(versions: &Path, version: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(versions).map_err(|error| error.to_string())?;
    // Sweep what a killed run left behind before minting a new one.
    if let Ok(entries) = std::fs::read_dir(versions) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".staging-") {
                std::fs::remove_dir_all(entry.path()).ok();
            }
        }
    }
    let staging = versions.join(format!(".staging-{version}-{}", std::process::id()));
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    Ok(staging)
}

fn probe(binary: &Path, version: &str) -> Result<(), String> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|error| format!("the new {} could not be run: {error}", binary.display()))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !printed.contains(version) {
        return Err(format!(
            "the new {} did not report version {version} (it said {:?}). The install was NOT \
             switched over; this box is still running its previous version.",
            binary.display(),
            printed.trim()
        ));
    }
    Ok(())
}

fn point_bin(home: &Path, version_dir: &Path) -> Result<(), String> {
    let bin = crate::paths::install_bin(home);
    std::fs::create_dir_all(&bin).map_err(|error| error.to_string())?;
    for name in ["chief", "chiefd", "beacond"] {
        let link = bin.join(name);
        let temporary = bin.join(format!(".{name}.tmp-{}", std::process::id()));
        // Symlink-swap of the client's own PATH entries, atomic by rename so a
        // running daemon's open inode is never disturbed (clippy.toml §5.6).
        #[allow(clippy::disallowed_methods)]
        {
            std::fs::remove_file(&temporary).ok();
            std::os::unix::fs::symlink(version_dir.join("bin").join(name), &temporary)
                .map_err(|error| format!("could not stage the {name} link: {error}"))?;
            std::fs::rename(&temporary, &link)
                .map_err(|error| format!("could not point {}: {error}", link.display()))?;
        }
    }
    Ok(())
}

fn record_previous(home: &Path, version: &str) -> Result<(), String> {
    let state = crate::paths::install_state_dir(home);
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    // The rollback record, in the client's own install-state directory
    // (clippy.toml README §5.6).
    #[allow(clippy::disallowed_methods)]
    let written = std::fs::write(state.join("previous"), format!("{version}\n"));
    written.map_err(|error| format!("could not record the previous version: {error}"))
}

/// Keep the current version, the one before it, and any version a live daemon
/// still runs from; delete the rest.
///
/// TWO is the FLOOR, not the whole rule (H.5.6): `--rollback` needs the
/// previous one, and a directory per release on a project that cuts one from
/// every green commit would fill a disk within a month — but a company that has
/// been up across three releases still reads its `resources/` by PATH at
/// materialization, so deleting the version it runs from breaks it with no
/// upgrade "failure" anywhere in sight. `observed` is beacond's answer about
/// which versions are live; `None` means beacond could not be reached, and then
/// this prunes NOTHING rather than delete a tree it cannot prove is dead.
fn prune_old_versions(
    home: &Path,
    current: &str,
    previous: &str,
    observed: Option<&[CompanyVersion]>,
) {
    let Some(observed) = observed else { return };
    let live = live_version_dirs(observed);
    let versions = crate::paths::versions_dir(home);
    let Ok(entries) = std::fs::read_dir(&versions) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == current
            || name == previous
            || name.starts_with(".staging-")
            || live.contains(&name)
        {
            continue;
        }
        std::fs::remove_dir_all(entry.path()).ok();
    }
}

#[cfg(test)]
mod tests {
    // These tests stage fake binaries and state files under a `tempfile`
    // directory to exercise the swap, probe and rollback on a real tree. The
    // writes are the fixture, not a product effect, so the seam's `#[allow]`
    // sits once at the module boundary rather than on every fixture line —
    // matching `bearer.rs`'s test module.
    #![allow(clippy::disallowed_methods)]
    use super::*;

    #[test]
    fn the_flags_parse_and_an_unknown_one_is_named() {
        assert_eq!(parse_mode(&[]), Ok(Mode::Install { skip_pi_check: false }));
        assert_eq!(parse_mode(&["--check".to_owned()]), Ok(Mode::Check));
        assert_eq!(parse_mode(&["--rollback".to_owned()]), Ok(Mode::Rollback));
        assert_eq!(
            parse_mode(&["--skip-pi-check".to_owned()]),
            Ok(Mode::Install { skip_pi_check: true })
        );
        assert_eq!(parse_mode(&["--force".to_owned()]), Err("--force".to_owned()));
    }

    #[test]
    fn check_and_rollback_together_are_refused_rather_than_silently_ordered() {
        // Two verbs in one argv is a typo. Honouring whichever arm the match
        // reaches first would do something the operator did not ask for, to an
        // install, without saying so.
        assert!(parse_mode(&["--check".to_owned(), "--rollback".to_owned()]).is_err());
    }

    #[test]
    fn a_tag_and_a_version_name_the_same_release() {
        // The bug this exists to stop: comparing `v2.0.7` against `2.0.7`
        // reports a perfectly current install as stale, for ever.
        assert_eq!(version_of_tag("v2.0.7"), "2.0.7");
        assert_eq!(version_of_tag("2.0.7"), "2.0.7");
        assert_eq!(version_of_tag("v0.1.0"), "0.1.0");
    }

    #[test]
    fn the_asset_name_is_the_one_the_release_workflow_publishes() {
        assert_eq!(
            asset_name("2.0.7", "aarch64-apple-darwin"),
            "chief-2.0.7-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn a_digest_is_matched_by_asset_name_and_never_by_position() {
        let sums = "\
aaaa  chief-2.0.7-x86_64-unknown-linux-gnu.tar.gz
bbbb  chief-2.0.7-aarch64-apple-darwin.tar.gz
cccc  chief-2.0.7-x86_64-apple-darwin.tar.gz
";
        assert_eq!(
            expected_digest(sums, "chief-2.0.7-aarch64-apple-darwin.tar.gz"),
            Some("bbbb".to_owned())
        );
        assert_eq!(expected_digest(sums, "chief-2.0.7-aarch64-unknown-linux-gnu.tar.gz"), None);
    }

    #[test]
    fn an_asset_missing_from_the_sums_file_has_no_digest_rather_than_a_wrong_one() {
        assert_eq!(expected_digest("", "chief-2.0.7-x86_64-apple-darwin.tar.gz"), None);
        assert_eq!(expected_digest("not a sums file\n", "anything"), None);
    }

    #[test]
    fn the_pi_gate_passes_at_or_above_the_targets_floor() {
        assert_eq!(pi_gate(Some("0.84.3"), "0.80.10"), PiGate::Meets);
        assert_eq!(pi_gate(Some("0.80.10"), "0.80.10"), PiGate::Meets);
    }

    #[test]
    fn the_pi_gate_names_both_numbers_when_it_refuses() {
        assert_eq!(
            pi_gate(Some("0.79.0"), "0.80.10"),
            PiGate::Below { installed: "0.79.0".to_owned(), floor: "0.80.10".to_owned() }
        );
    }

    #[test]
    fn an_unreadable_pi_version_proceeds_rather_than_blocking_an_upgrade() {
        // A Pi that declines to name itself is a Pi nobody can judge. Blocking
        // here would strand every user behind a cosmetic release of somebody
        // else's program.
        assert_eq!(pi_gate(None, "0.80.10"), PiGate::Unknown);
        assert_eq!(pi_gate(Some("unreported"), "0.80.10"), PiGate::Unknown);
    }

    fn below() -> PiGate {
        PiGate::Below { installed: "0.79.0".to_owned(), floor: "0.80.10".to_owned() }
    }

    #[test]
    fn a_met_gate_proceeds_and_never_touches_pi() {
        let mut ran = false;
        let outcome = resolve_pi_gate(
            PiGate::Meets,
            false,
            || {
                ran = true;
                Ok(())
            },
            || panic!("a met gate must not re-probe"),
        );
        assert_eq!(outcome, PiGateOutcome::Proceed);
        assert!(!ran, "a met gate must not run 'pi update'");
    }

    #[test]
    fn declining_the_pi_update_aborts_and_names_the_way_out() {
        let outcome =
            resolve_pi_gate(below(), false, || panic!("declined, so pi must not run"), below);
        let PiGateOutcome::Abort(message) = outcome else { panic!("a decline must abort") };
        assert!(message.contains("--skip-pi-check"), "{message}");
        assert!(message.contains("pi update"), "{message}");
    }

    #[test]
    fn a_failed_pi_update_aborts_with_pis_own_error_and_leaves_chief_alone() {
        let outcome = resolve_pi_gate(
            below(),
            true,
            || Err("'pi update' exited 1. chief was not changed.".to_owned()),
            || panic!("a failed update must not re-probe"),
        );
        assert_eq!(
            outcome,
            PiGateOutcome::Abort("'pi update' exited 1. chief was not changed.".to_owned())
        );
    }

    #[test]
    fn a_successful_pi_update_that_clears_the_floor_proceeds() {
        let outcome = resolve_pi_gate(below(), true, || Ok(()), || PiGate::Meets);
        assert_eq!(outcome, PiGateOutcome::Proceed);
    }

    #[test]
    fn a_pi_update_that_leaves_pi_below_the_floor_is_a_release_bug() {
        // Pi's latest is older than chief requires: no update can satisfy this,
        // so it is chief's bug, and the operator is sent to file an issue
        // rather than into a loop.
        let outcome = resolve_pi_gate(below(), true, || Ok(()), below);
        let PiGateOutcome::Abort(message) = outcome else { panic!("still-below must abort") };
        assert!(message.contains("release bug"), "{message}");
        assert!(message.contains("file an issue"), "{message}");
    }

    #[test]
    fn the_check_verdict_maps_each_case_to_its_own_exit() {
        // 0 = current, 10 = upgrade exists, 1 = the check itself failed. A
        // wrapper must be able to tell all three apart.
        assert_eq!(check_verdict("2.0.7", Some("2.0.7")), CheckVerdict::Current);
        assert_eq!(check_verdict("2.0.7", Some("2.0.8")), CheckVerdict::UpgradeAvailable);
        assert_eq!(check_verdict("2.0.7", None), CheckVerdict::CheckFailed);
    }

    #[test]
    fn the_manifest_reads_the_two_fields_this_verb_needs() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"2.0.7","target":"x86_64-apple-darwin","piFloor":"0.80.10",
                "binaries":{},"resources":{}}"#,
        )
        .expect("the manifest a release publishes");
        assert_eq!(manifest.version, "2.0.7");
        assert_eq!(manifest.pi_floor, "0.80.10");
    }

    #[test]
    fn the_latest_release_is_read_from_its_tag_and_nothing_else() {
        let release: LatestRelease =
            serde_json::from_str(r#"{"tag_name":"v2.0.7","name":"2.0.7","assets":[]}"#)
                .expect("the releases API shape");
        assert_eq!(release.tag_name, "v2.0.7");
    }

    #[test]
    fn every_published_target_is_one_this_binary_can_name() {
        // The pair this build runs on must be one of the four the release
        // matrix publishes, or `chief upgrade` on it can only ever refuse.
        // Asserted for the HOST because that is what the test runner is.
        let target = host_target();
        assert!(
            target.is_some(),
            "no release target for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        assert!(target.expect("some").contains(std::env::consts::ARCH));
    }

    #[test]
    fn the_upgrade_available_exit_code_is_not_one() {
        // A wrapper must be able to tell "there is news" from "this did not
        // work". Sharing 1 with every failure makes a network outage read as an
        // available upgrade.
        assert_ne!(UPGRADE_AVAILABLE_EXIT, 1);
        assert_ne!(UPGRADE_AVAILABLE_EXIT, 0);
    }

    #[test]
    fn staging_sweeps_what_a_killed_run_left_behind() {
        let root = tempfile::tempdir().expect("tempdir");
        let versions = root.path().join("versions");
        std::fs::create_dir_all(versions.join(".staging-2.0.6-999")).expect("stale staging");
        std::fs::create_dir_all(versions.join("2.0.6")).expect("a real version");

        let staging = tempdir_under(&versions, "2.0.7").expect("staging");

        assert!(staging.is_dir());
        assert!(!versions.join(".staging-2.0.6-999").exists(), "a killed run's leftovers go");
        assert!(versions.join("2.0.6").is_dir(), "a real version stays");
    }

    #[test]
    fn the_symlink_swap_replaces_a_link_without_writing_through_it() {
        let home = tempfile::tempdir().expect("tempdir");
        let version_dir = home.path().join("versions/2.0.7");
        std::fs::create_dir_all(version_dir.join("bin")).expect("bin");
        for name in ["chief", "chiefd", "beacond"] {
            std::fs::write(version_dir.join("bin").join(name), b"new").expect("binary");
        }
        // A hostile link already sitting where the release must publish.
        let bin = home.path().join(".chief/bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let victim = home.path().join("victim");
        std::fs::write(&victim, b"do not touch").expect("victim");
        std::os::unix::fs::symlink(&victim, bin.join("chief")).expect("hostile link");

        point_bin(home.path(), &version_dir).expect("swap");

        assert_eq!(std::fs::read(&victim).expect("victim"), b"do not touch");
        assert_eq!(std::fs::read(bin.join("chief")).expect("chief"), b"new");
        assert!(std::fs::symlink_metadata(bin.join("chief")).expect("meta").is_symlink());
    }

    fn company(slug: &str, version: Option<&str>) -> CompanyVersion {
        CompanyVersion {
            slug: slug.to_owned(),
            dir: format!("/companies/{slug}"),
            version: version.map(str::to_owned),
        }
    }

    #[test]
    fn pruning_keeps_the_current_and_the_previous_version_and_nothing_else() {
        let home = tempfile::tempdir().expect("tempdir");
        let versions = home.path().join(".chief/versions");
        for name in ["2.0.5", "2.0.6", "2.0.7", ".staging-2.0.8-1"] {
            std::fs::create_dir_all(versions.join(name)).expect("version dir");
        }

        // No live daemon runs an old version — nothing to protect past keep-two.
        prune_old_versions(home.path(), "2.0.7", "2.0.6", Some(&[]));

        assert!(versions.join("2.0.7").is_dir());
        assert!(versions.join("2.0.6").is_dir(), "--rollback needs the previous one to exist");
        assert!(!versions.join("2.0.5").exists());
        assert!(
            versions.join(".staging-2.0.8-1").is_dir(),
            "a live staging directory is not ours to delete"
        );
    }

    #[test]
    fn pruning_keeps_a_version_a_live_daemon_still_runs_even_past_keep_two() {
        // H.5.6: 2.0.5 is three releases back and would normally be pruned, but
        // a company has been up on it since before two upgrades. Its binaries
        // survive as open inodes; its `resources/` does not survive a delete.
        let home = tempfile::tempdir().expect("tempdir");
        let versions = home.path().join(".chief/versions");
        for name in ["2.0.5", "2.0.6", "2.0.7"] {
            std::fs::create_dir_all(versions.join(name)).expect("version dir");
        }

        prune_old_versions(
            home.path(),
            "2.0.7",
            "2.0.6",
            Some(&[company("meridian", Some("2.0.5"))]),
        );

        assert!(versions.join("2.0.5").is_dir(), "a version a live daemon runs must survive prune");
    }

    #[test]
    fn pruning_does_nothing_when_beacond_cannot_be_reached() {
        // `None` = no answer from beacond, so we cannot prove any version is
        // dead. Pruning nothing is the safe default; disk tidiness waits.
        let home = tempfile::tempdir().expect("tempdir");
        let versions = home.path().join(".chief/versions");
        for name in ["2.0.4", "2.0.5", "2.0.6", "2.0.7"] {
            std::fs::create_dir_all(versions.join(name)).expect("version dir");
        }

        prune_old_versions(home.path(), "2.0.7", "2.0.6", None);

        assert!(versions.join("2.0.4").is_dir(), "an unenumerable box prunes nothing");
        assert!(versions.join("2.0.5").is_dir());
    }

    #[test]
    fn stale_companies_names_only_those_on_a_different_readable_version() {
        let observed = [
            company("current-one", Some("2.0.7")),
            company("old-one", Some("2.0.5")),
            company("unreadable", None),
        ];
        let stale = stale_companies(&observed, "2.0.7");
        assert_eq!(stale.len(), 1, "only the readable, different-version company is stale");
        assert_eq!(stale[0].slug, "old-one");
        // A company whose version cannot be read is NOT reported as stale — a
        // daemon that cannot say what it is is not evidence that it is old.
        assert!(!stale.iter().any(|c| c.slug == "unreadable"));
        assert!(!stale.iter().any(|c| c.slug == "current-one"));
    }

    #[test]
    fn live_version_dirs_collects_every_readable_version_and_skips_unknowns() {
        let observed =
            [company("a", Some("2.0.5")), company("b", Some("2.0.6")), company("c", None)];
        let live = live_version_dirs(&observed);
        assert!(live.contains("2.0.5") && live.contains("2.0.6"));
        assert_eq!(live.len(), 2, "an unknown version protects no directory");
    }

    #[test]
    fn a_probe_that_reports_the_wrong_version_refuses_and_says_nothing_was_switched() {
        let root = tempfile::tempdir().expect("tempdir");
        let fake = root.path().join("chief");
        std::fs::write(&fake, "#!/bin/sh\necho 'chief 1.0.0'\n").expect("fake binary");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");

        let refusal = probe(&fake, "2.0.7").expect_err("a wrong version must refuse");

        assert!(refusal.contains("2.0.7"), "{refusal}");
        assert!(
            refusal.contains("NOT"),
            "the operator must be told the swap did not happen: {refusal}"
        );
    }

    #[test]
    fn a_probe_that_reports_the_expected_version_passes() {
        let root = tempfile::tempdir().expect("tempdir");
        let fake = root.path().join("chief");
        std::fs::write(&fake, "#!/bin/sh\necho 'chief 2.0.7'\n").expect("fake binary");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");

        assert!(probe(&fake, "2.0.7").is_ok());
    }

    #[test]
    fn rollback_refuses_when_the_previous_version_is_no_longer_on_disk() {
        let home = tempfile::tempdir().expect("tempdir");
        let state = home.path().join(".chief/state");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::write(state.join("previous"), "2.0.1\n").expect("record");

        let refusal = rollback(home.path()).expect_err("a version that is gone cannot be restored");

        assert!(refusal.contains("2.0.1"), "{refusal}");
        assert!(
            refusal.contains("last two"),
            "the refusal must say why it is not there: {refusal}"
        );
    }

    #[test]
    fn rollback_refuses_when_nothing_was_ever_recorded() {
        let home = tempfile::tempdir().expect("tempdir");
        let refusal = rollback(home.path()).expect_err("nothing to roll back to");
        assert!(refusal.contains("no previous version"), "{refusal}");
    }
}
