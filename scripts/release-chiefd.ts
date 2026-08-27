#!/usr/bin/env bun
import { chmodSync, copyFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * The repository this script publishes, established from the script's own
 * location.
 *
 * This was previously imported from `apps/cli/src/legacy/foundation/paths`.
 * When commit ca2da9b57 deleted that tree, `bun run release` — the one
 * documented way to install chiefd and beacond — stopped LOADING on a clean
 * clone, failing at module resolution before it did any work. It kept working
 * on every machine whose working tree still carried the deleted file, so the
 * break was invisible to everyone who already had the repo and total for
 * anyone who did not.
 *
 * A build script has no business importing a path constant from an
 * application it builds. `scripts/` sits one level below the repository root,
 * and that is a fact this file can establish for itself with no dependency at
 * all — which is also what makes it survive on a clone with no `node_modules`.
 */
const launcherRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/**
 * # Build speed: this script is ALREADY incremental
 *
 * It runs `bun install --force --frozen-lockfile`, then runs
 * `cargo build --release --locked`. It never cleans Cargo
 * output — no `cargo clean`, no `rm -rf target`. So `git pull && bun run
 * release` rebuilds only changed Rust inputs. There is no "make it do deltas"
 * switch to find; that is the default.
 *
 * What makes a rebuild feel slow is the SHIPPED profile, not a lack of
 * incrementality (`apps/chiefd/Cargo.toml`):
 *
 *   [profile.release]  opt-level = 3  lto = "thin"  codegen-units = 1  strip = true
 *
 * `codegen-units = 1` deliberately serializes codegen for runtime performance,
 * and cargo disables incremental compilation for `--release` by default. Touch
 * `chiefd-core` and all four crates recompile through a single codegen unit.
 * That is correct for a shipped binary and wrong for a debug loop.
 *
 * Two levers, both measured on this repo:
 *
 * 1. `bun run release:fast` — the same script and the same output path,
 *    with `CARGO_PROFILE_RELEASE_{CODEGEN_UNITS=16,LTO=false,INCREMENTAL=true}`.
 *    A dev-tuned binary: marginally slower at runtime, much faster to produce.
 *    Never use it for a release; `release` carries no overrides and a
 *    test pins that separation.
 * 2. `export CARGO_TARGET_DIR=<repo>/apps/chiefd/target` — share ONE target
 *    directory across git worktrees. Without it every worktree builds cold;
 *    with it a warm rebuild reuses artifacts. Caveat: cargo takes an exclusive
 *    lock on the target dir, so concurrent agents serialize rather than race.
 */

/**
 * The user-owned ChiefD OPERATOR CLIENT. No privileged global install.
 *
 * THREE executables land in `~/.chief/bin`, not two. P6 of
 * the design record split the single `chiefd` binary —
 * which was operator CLI and backend daemon at once — into `chief` (built
 * from `crates/chief-cli`) and `chiefd` (built from
 * `crates/chiefd-daemon`), because one binary forced the operator half to link
 * the backend's crates. A release that stamps only one of the pair produces an
 * install where every operator verb works until the moment it needs a company
 * daemon, and then fails with a missing-file error nobody can act on. Both are
 * built by the same `cargo build`, installed by the same atomic copy, and
 * size-checked the same way.
 */
export function defaultChiefBin(home: string): string {
  return join(home, ".chief", "bin", "chief");
}

/** The user-owned backend daemon, installed beside the client. */
export function defaultChiefdBin(home: string): string {
  return join(home, ".chief", "bin", "chiefd");
}

export function defaultBeacondBin(home: string): string {
  return join(home, ".chief", "bin", "beacond");
}

/**
 * Every executable a release installs, in `<cargo bin target>` order.
 *
 * ONE list. The `--bin` flags, the built-artifact resolution, the install loop
 * and the zero-byte check all read it, so a fourth binary is one edit and a
 * missing one is impossible rather than merely unlikely.
 */
export const RELEASE_BINARIES = ["chief", "chiefd", "beacond"] as const;

/**
 * The binary an install must REMOVE, not install.
 *
 * The P6 pair used to be `chiefd` (client) and `chiefd-daemon` (backend). The
 * names are now `chief` and `chiefd`, so a host released before the rename
 * carries a `chiefd-daemon` that nothing resolves any more. It is deleted
 * rather than left behind: an orphaned executable with a name this project
 * once spawned is a second answer to "which program serves a company".
 */
export const OBSOLETE_RELEASE_BINARIES = ["chiefd-daemon"] as const;

/**
 * TOMBSTONE: `defaultFounderSkillDir` and `~/.chief/skills/founder`.
 *
 * A second copy of `packages/piing/skills/founder/{SKILL.md,AGENTS.md}` was
 * published there, described as "the payload that makes Founder mode
 * self-contained". NOTHING EVER READ IT: `chief-cli`'s `founder_pi.rs` builds
 * its `--skill` argument as `<resource root>/packages/piing/skills/founder`,
 * and no Rust file in the workspace names `~/.chief/skills`. It was written on
 * every release and read by nobody.
 *
 * The versioned `resources/` payload below is what actually makes an install
 * self-contained, and it carries the same two files at the path the binary
 * really resolves. A second copy at a path nothing reads is a second answer
 * waiting to be believed.
 */
/**
 * The checkout-relative directories copied into a version's `resources/`.
 *
 * THE SHAPE IS THE CONTRACT. Each of these is joined by name in Rust —
 * `founder_pi.rs`'s `packages/piing/{skills,extensions}`,
 * `chiefd-host`'s `organization_extension_paths` and
 * `runtime_lifecycle`'s `packages/piing/dist/extensionruntime/index.js` — so
 * `resources/` deliberately mirrors the checkout rather than flattening it.
 * That is what lets `--launcher-root <checkout>` and an installed
 * `resources/` root resolve every subpath through one expression, and it is
 * why the test harness needs no special case.
 */
export const RESOURCE_SUBTREES = [
  "packages/piing/extensions",
  "packages/piing/skills",
  "packages/piing/dist/extensionruntime",
  // THE WHOLE `chiefing` DIST, not just its `extensionruntime`.
  //
  // `piing`'s extension runtime is self-contained: every import in it is a
  // sibling file or a `node:` builtin. `chiefing`'s is not — its `index.js`
  // imports `../Errors.js`, `../transport/FetchTransport.js`,
  // `../sse/SseWatcher.js` and a dozen more, all of which sit ABOVE the
  // directory. Packaging `packages/chiefing/dist/extensionruntime` by symmetry
  // with the line above would ship a directory whose every import escapes it,
  // and it would fail at launch in a release while passing every test that
  // runs from a checkout — which is the shape of the defect this line fixes.
  "packages/chiefing/dist",
] as const;

/**
 * The package specifiers the shipped extensions import, and the file each one
 * must resolve to inside `resources/`.
 *
 * # Why an installed release needs this at all
 *
 * A checkout resolves `@chief/piing/extension-runtime` through
 * `node_modules/@chief/piing`, a workspace link `bun install` creates. An
 * INSTALL has no `node_modules` at all, so the same import — from the same
 * extension source, now sitting under `resources/` — resolves against nothing
 * and Pi exits 1 before the person's pane can do anything.
 *
 * Measured live: three people crash-looping, every card blank, and the pane's
 * own stderr saying `Cannot find module '@chief/piing/extension-runtime'`.
 *
 * # Why a SHIM and not the real `package.json`
 *
 * Copying `packages/piing/package.json` in would work and would put a second
 * `exports` map in the tree describing entry points that are NOT shipped
 * (`"."` resolves to `dist/index.js`, which a release has no reason to carry).
 * A manifest that advertises what is absent is the "second answer waiting to be
 * believed" this file's header warns about. The shim declares exactly the one
 * subpath the extensions actually import, and points at the one copy of the
 * runtime that ships.
 *
 * A relative SYMLINK would be the other obvious answer and is deliberately not
 * used: `filesUnder` walks `resources/` to hash every file into the manifest,
 * and a symlink to a directory is neither a directory it recurses into nor a
 * file it can hash. Two real, tiny files stay honest to the integrity check.
 */
export const EXTENSION_RUNTIME_SHIMS = [
  { pkg: "@chief/piing", from: "packages/piing/dist/extensionruntime/index.js" },
  { pkg: "@chief/chiefing", from: "packages/chiefing/dist/extensionruntime/index.js" },
] as const;

export interface ReleaseChiefdOptions {
  /**
   * Test-only seam for an install root other than `$HOME/.chief`.
   *
   * TOMBSTONE: this was `chiefPath`, a seam naming ONE file. An install is a
   * versioned TREE now — `bin/` symlinks, `versions/<v>/{bin,resources}`, a
   * manifest — so a seam that names one leaf inside it cannot express a
   * fixture install at all. Naming the root does.
   */
  installHome?: string;
  skipCargo?: boolean;
  /** Test seam for the final selected Cargo build command. */
  cargoBuild?: (command: readonly string[], version: string) => void;
  /** Test seam for a Cargo version probe. */
  cargoVersion?: (command: readonly string[]) => string;
  /** Test seam for user-local compatible-toolchain provisioning. */
  provisionCompatibleCargo?: (home: string) => readonly string[];
  /** Synchronizes the lockfile-pinned launcher dependencies before a real release build. */
  installDependencies?: (root: string) => void;
  /** Test seam for the native-linker preflight (#707). */
  ensureNativeLinker?: (environment: Record<string, string | undefined>) => void;
  root?: string;
  resolveBuiltChiefBinary?: (root: string) => string;
  resolveBuiltChiefdBinary?: (root: string) => string;
  resolveBuiltBeacondBinary?: (root: string) => string;
  environment?: Record<string, string | undefined>;
}

export interface ReleaseChiefdResult {
  /** `~/.chief/bin/chief` — the symlink an operator's PATH names. */
  chiefPath: string;
  chiefdPath: string;
  beacondPath: string;
  action: "installed" | "updated";
  /** The version this release installed, and the directory name under `versions/`. */
  version: string;
  /** `~/.chief/versions/<version>` — binaries, resources and manifest together. */
  versionPath: string;
  /** `<versionPath>/resources` — what every binary resolves its Pi assets through. */
  resourcesPath: string;
  /** `<versionPath>/manifest.json`. */
  manifestPath: string;
}

// `apps/chiefd/Cargo.toml` declares `rust-version = "1.95"`. Its
// `[workspace.lints]` inheritance is also valid Cargo syntax, but old Cargo
// releases predate that table and noisily call it an unused key before they
// fail the build for the unsupported toolchain. Select a compatible toolchain
// before they read the manifest, rather than weakening the workspace lint contract.
const MINIMUM_CARGO_VERSION = { major: 1, minor: 95 } as const;
const REQUIRED_RUST_TOOLCHAIN = "1.95.0";
const RUSTUP_INSTALLER_URL = "https://sh.rustup.rs";

function cargoVersionIsSupported(version: string): boolean {
  const match = /^cargo\s+(\d+)\.(\d+)(?:\.\d+)?(?:\s|$)/.exec(version.trim());
  if (!match) return false;
  const major = Number(match[1]);
  const minor = Number(match[2]);
  return major > MINIMUM_CARGO_VERSION.major
    || (major === MINIMUM_CARGO_VERSION.major && minor >= MINIMUM_CARGO_VERSION.minor);
}

function defaultCargoVersion(command: readonly string[]): string {
  const result = Bun.spawnSync([...command, "--version"], { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    const stderr = new TextDecoder().decode(result.stderr).trim();
    throw new Error(`release could not run ${command.join(" ")} --version${stderr ? `: ${stderr}` : ""}.`);
  }
  return new TextDecoder().decode(result.stdout).trim();
}

function requireSupportedCargo(version: string, command: readonly string[]): void {
  if (cargoVersionIsSupported(version)) return;
  throw new Error(
    `release provisioned ${command.join(" ")}, but it is not Cargo ${MINIMUM_CARGO_VERSION.major}.${MINIMUM_CARGO_VERSION.minor} or newer ` +
    `(ChiefD declares rust-version = \"1.95\"); found ${JSON.stringify(version || "unavailable")}. Repair ~/.cargo or install Rustup and rerun.`,
  );
}

function runRequired(command: readonly string[], label: string): void {
  try {
    const result = Bun.spawnSync([...command], { stdout: "inherit", stderr: "inherit" });
    if (result.exitCode !== 0) throw new Error(`release ${label} failed with exit code ${result.exitCode ?? "unknown"}`);
  } catch (error) {
    if (error instanceof Error && error.message.startsWith("release")) throw error;
    throw new Error(`release ${label}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

/** Generic "is this a real, runnable command" probe: `<command> --version` exits 0. */
function commandIsAvailable(command: string): boolean {
  try {
    const result = Bun.spawnSync([command, "--version"], { stdout: "ignore", stderr: "ignore" });
    return result.exitCode === 0;
  } catch {
    return false;
  }
}

function rustupIsAvailable(command: string): boolean {
  return commandIsAvailable(command);
}

function bootstrapRustup(home: string): string {
  const directory = mkdtempSync(join(tmpdir(), "chiefd-rustup-"));
  const installer = join(directory, "rustup-init.sh");
  try {
    console.log("release: installing the user-local Rust 1.95 toolchain (old distro Cargo cannot build ChiefD)");
    runRequired(["curl", "--fail", "--location", "--proto", "=https", "--tlsv1.2", "--output", installer, RUSTUP_INSTALLER_URL], "could not download the official rustup installer; install curl with HTTPS support or Rustup, then rerun");
    runRequired(["sh", installer, "-y", "--profile", "minimal", "--default-toolchain", REQUIRED_RUST_TOOLCHAIN, "--no-modify-path"], "could not install the user-local Rust toolchain");
    const rustup = join(home, ".cargo", "bin", "rustup");
    if (!rustupIsAvailable(rustup)) {
      throw new Error(`release rustup bootstrap completed but ${rustup} is unavailable; repair the user-local Rustup installation and rerun.`);
    }
    return rustup;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

/**
 * An ambient `rustup` on PATH belongs to the caller's HOME.  A release is
 * explicitly allowed to target a different, clean HOME, so using that ambient
 * shim would make Rustup look for proxies in the target's empty `~/.cargo`
 * directory and fail before ChiefD is installed.  Only a Rustup binary that
 * lives under the selected release home is reusable; every other case must
 * bootstrap a self-contained user-local toolchain there.
 */
export function userLocalRustupCommand(
  home: string,
  available: (command: string) => boolean = rustupIsAvailable,
): string | undefined {
  const rustup = join(home, ".cargo", "bin", "rustup");
  return available(rustup) ? rustup : undefined;
}

function defaultProvisionCompatibleCargo(home: string): readonly string[] {
  const rustup = userLocalRustupCommand(home) ?? bootstrapRustup(home);
  runRequired([rustup, "toolchain", "install", REQUIRED_RUST_TOOLCHAIN, "--profile", "minimal"], `could not provision Rust ${REQUIRED_RUST_TOOLCHAIN}`);
  return [rustup, "run", REQUIRED_RUST_TOOLCHAIN, "cargo"];
}

/**
 * #707: on a clean Linux host, Cargo/Rustup can be fully present while the
 * native linker (`cc`) is not — `cargo build` then fails mid-compile with
 * "linker `cc` not found", after crates have already been downloaded and
 * partially built. That is a late, opaque failure a user cannot act on: the
 * error names a linker, not a package to install. Detect it BEFORE Cargo is
 * ever invoked, so the failure — if this cannot fix it itself — is an exact,
 * actionable command instead of a raw linker error discovered mid-build.
 *
 * True on THIS command's own PATH, checked directly rather than inferred
 * from `apt`/`dpkg` state: `cc` on a clean Linux host is provided by the
 * `build-essential` (Debian/Ubuntu) or equivalent package, but the
 * authoritative question is only ever "can Cargo actually find a linker
 * right now," and the cheapest, most direct way to answer it is to try.
 */
export function nativeLinkerIsPresent(available: (command: string) => boolean = commandIsAvailable): boolean {
  return available("cc");
}

/**
 * Detect and refuse — never install. #707's complaint is that the failure
 * is INCOMPREHENSIBLE (a raw linker error naming no package), not that
 * installation is laborious; a clear refusal naming the exact command
 * solves the actual problem without `bun run release` silently
 * mutating the host as a side effect of a build.
 *
 * Deliberately does not attempt an automatic install, on three grounds:
 *  - it would mutate the user's machine (installing system packages) as a
 *    side effect of a build command, on a laptop/CI runner/container the
 *    build does not own;
 *  - it would require root, a different security posture than a build
 *    that only reads;
 *  - it would assume a package manager (`apt-get`) that only exists on
 *    Debian/Ubuntu — macOS is a first-class target for this product
 *    (CLAUDE.md: every change must behave identically on Darwin and
 *    Linux), and a fix that only works on one OS's package manager is the
 *    cross-platform failure this project explicitly guards against, in a
 *    new costume.
 *
 * `platform` is an injected parameter (not read from `process.platform`
 * internally) so both branches (Linux, macOS) are directly unit-testable
 * without depending on which OS the test itself happens to run on.
 */
export function ensureNativeLinkerWith(
  _environment: Record<string, string | undefined>,
  deps: {
    linkerPresent?: () => boolean;
    platform?: NodeJS.Platform;
  } = {},
): void {
  const linkerPresent = deps.linkerPresent ?? nativeLinkerIsPresent;
  const platform = deps.platform ?? process.platform;
  if (linkerPresent()) return;
  const fix = platform === "darwin"
    ? "xcode-select --install"
    : "sudo apt-get update && sudo apt-get install -y build-essential  (Debian/Ubuntu; use your distro's equivalent C toolchain package otherwise)";
  throw new Error(
    "release: no C compiler / native linker (`cc`) was found on PATH. Cargo/Rustup being present is not " +
      "enough — compiling ChiefD also needs a native build toolchain, and this is checked before Cargo is " +
      "invoked so the failure is an exact command, not a raw linker error discovered mid-build.\n\n" +
      `Install it, then rerun release:\n\n    ${fix}\n`,
  );
}

function defaultEnsureNativeLinker(environment: Record<string, string | undefined>): void {
  ensureNativeLinkerWith(environment);
}

function compatibleCargoCommand(options: ReleaseChiefdOptions, home: string): readonly string[] {
  const cargo = ["cargo"];
  const cargoVersion = options.cargoVersion ?? defaultCargoVersion;
  let installedVersion = "";
  try {
    installedVersion = cargoVersion(cargo);
  } catch {
    // A minimal host may not have Cargo at all. The user-local Rustup path
    // below is the supported release bootstrap in that case too.
  }
  if (cargoVersionIsSupported(installedVersion)) return cargo;

  console.log(`release: selecting Rust ${REQUIRED_RUST_TOOLCHAIN} because system Cargo is ${JSON.stringify(installedVersion || "unavailable")}`);
  const selected = (options.provisionCompatibleCargo ?? defaultProvisionCompatibleCargo)(home);
  requireSupportedCargo(cargoVersion(selected), selected);
  return selected;
}

/**
 * `version` is passed to cargo as `CHIEF_RELEASE_VERSION`, where each binary's
 * `build.rs` bakes it into `--version`.
 *
 * The SAME value names the version directory these binaries are installed
 * into, so `chief --version` and the directory it lives in cannot disagree.
 * `chief upgrade` compares the installed version against the latest release; a
 * disagreement here would be an upgrade that reports itself as never having
 * landed and offers itself again for ever.
 */
function defaultCargoBuild(command: readonly string[], version: string): void {
  const result = Bun.spawnSync(
    [
      ...command,
      "build",
      "--release",
      "--locked",
      "--manifest-path",
      "apps/chiefd/Cargo.toml",
      ...RELEASE_BINARIES.flatMap((name) => ["--bin", name]),
    ],
    { stdout: "inherit", stderr: "inherit", env: { ...process.env, CHIEF_RELEASE_VERSION: version } },
  );
  if (result.exitCode !== 0) throw new Error(`cargo build --release --locked --manifest-path apps/chiefd/Cargo.toml failed with exit code ${result.exitCode}`);
}

/**
 * Materializes the lockfile-pinned launcher dependencies before a release build.
 *
 * TOMBSTONE: this also spawned `bun run --cwd packages/piing attest:pi`, which
 * proved `node_modules/.bin/pi` was the pinned PATCHED build. The patch is
 * deleted, so the attestation has no subject and bun's lockfile already
 * guarantees the integrity of what it installs.
 *
 * The `bun install` below STAYS, and not because it is left over: its reason
 * has nothing to do with patching -- a frozen install can relink stale
 * hidden-store bytes, which is a release-input problem either way.
 */
export function prepareReleaseDependencies(root: string): void {
  // An ordinary frozen install can relink stale hidden-store bytes after the
  // public package link is removed. Force Bun to reconstruct the package from
  // the lockfile before any release build starts.
  const result = Bun.spawnSync(["bun", "install", "--force", "--frozen-lockfile"], {
    cwd: root,
    stdout: "inherit",
    stderr: "inherit",
  });
  if (result.exitCode !== 0) {
    throw new Error(`bun install --force --frozen-lockfile failed with exit code ${result.exitCode ?? "unknown"}`);
  }

}

function releaseArtifactDir(root: string, environment: Record<string, string | undefined>): string {
  return join(environment.CARGO_TARGET_DIR?.trim() || join(root, "apps", "chiefd", "target"), "release");
}

export function defaultBuiltChiefBinary(root: string, environment: Record<string, string | undefined> = process.env): string {
  return join(releaseArtifactDir(root, environment), "chief");
}

export function defaultBuiltChiefdBinary(root: string, environment: Record<string, string | undefined> = process.env): string {
  return join(releaseArtifactDir(root, environment), "chiefd");
}

export function defaultBuiltBeacondBinary(root: string, environment: Record<string, string | undefined> = process.env): string {
  return join(releaseArtifactDir(root, environment), "beacond");
}

function homeFor(environment: Record<string, string | undefined>): string {
  const home = environment.HOME?.trim();
  if (!home) throw new Error("release requires HOME to install under ~/.chief");
  return home;
}

function lstatIfPresent(path: string): ReturnType<typeof lstatSync> | undefined {
  try {
    return lstatSync(path);
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") return undefined;
    throw error;
  }
}

function requireRealDirectory(path: string, label: string): void {
  const existing = lstatIfPresent(path);
  if (existing && (!existing.isDirectory() || existing.isSymbolicLink())) {
    throw new Error(`release refuses unsafe ${label} at ${path}: expected a real directory, not a symlink or file`);
  }
}

function prepareUserChiefBin(installHome: string, targets: readonly string[]): void {
  const chiefdHome = installHome;
  const bin = join(chiefdHome, "bin");
  requireRealDirectory(chiefdHome, "ChiefD home");
  mkdirSync(chiefdHome, { recursive: true, mode: 0o700 });
  chmodSync(chiefdHome, 0o700);
  requireRealDirectory(bin, "ChiefD binary directory");
  mkdirSync(bin, { recursive: true, mode: 0o700 });
  chmodSync(bin, 0o700);
  for (const target of targets) {
    const existing = lstatIfPresent(target);
    // A SYMLINK IS THE EXPECTED SHAPE NOW — that is what an install publishes
    // here — so the refusal is narrowed to what it was always really about: a
    // directory, a socket, or anything else that is neither a link nor a file.
    if (existing && !existing.isSymbolicLink() && !existing.isFile()) {
      throw new Error(`release refuses unsafe ${target.split("/").pop()} binary target at ${target}: expected a symlink or a regular file`);
    }
  }
}


function installFileAtomically(source: string, target: string, mode: number): void {
  mkdirSync(dirname(target), { recursive: true });
  const temporary = join(dirname(target), `.${target.split("/").pop()}.tmp-${process.pid}-${Math.random().toString(16).slice(2)}`);
  copyFileSync(source, temporary);
  chmodSync(temporary, mode);
  renameSync(temporary, target);
}

/**
 * The version this release installs, and therefore the directory it installs into.
 *
 * `CHIEF_RELEASE_VERSION` wins when it is set — that is how the release
 * workflow stamps the tag it is building — and the Cargo workspace's own
 * `[workspace.package] version` is the answer otherwise. The SAME value is
 * handed to `cargo build` as `CHIEF_RELEASE_VERSION`, where each binary's
 * `build.rs` bakes it into `--version`, so the directory name and what the
 * binary says about itself cannot disagree. `chief upgrade` compares those two,
 * so a disagreement would be an upgrade loop that never converges.
 *
 * Read from the MANIFEST rather than by running the binary, deliberately: a
 * build that has not run yet, and every fixture install in the test suite, must
 * still resolve a version.
 */
export function releaseVersion(root: string, environment: Record<string, string | undefined>): string {
  const stamped = environment.CHIEF_RELEASE_VERSION?.trim();
  const version = stamped && stamped.length > 0 ? stamped : workspaceVersion(root);
  // A version becomes a DIRECTORY NAME and a symlink target. Anything with a
  // separator in it would install somewhere nobody asked for.
  if (!/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(version)) {
    throw new Error(`release refuses the version ${JSON.stringify(version)}: it becomes a directory name, so it must be a plain identifier`);
  }
  return version;
}

function workspaceVersion(root: string): string {
  const manifest = readFileSync(join(root, "apps", "chiefd", "Cargo.toml"), "utf8");
  const [, block = ""] = manifest.split(/^\[workspace\.package\]$/m);
  const match = /^version\s*=\s*"([^"]+)"/m.exec(block);
  if (!match) {
    throw new Error("release cannot read [workspace.package] version from apps/chiefd/Cargo.toml");
  }
  return match[1];
}

/**
 * The minimum Pi version, parsed out of its ONE definition in Rust.
 *
 * Never transcribed here. `scripts/test/pi-floor-single-definition.test.mjs`
 * fails if this file carries a copy of the number, because the manifest a
 * release publishes is what `chief upgrade` reads to decide whether to offer
 * Pi's own updater — a stale copy would offer the wrong answer to the one
 * person who cannot check it.
 */
export function piFloor(root: string): string {
  const source = readFileSync(join(root, "apps/chiefd/crates/host-primitives/src/pi_floor.rs"), "utf8");
  const match = /pub const MINIMUM_PI_VERSION: &str = "([^"]+)";/.exec(source);
  if (!match) throw new Error("release cannot read MINIMUM_PI_VERSION from host-primitives/src/pi_floor.rs");
  return match[1];
}

/** The target triple this host builds for, in the spelling the release assets use. */
export function hostTarget(platform: string = process.platform, arch: string = process.arch): string {
  const cpu = arch === "arm64" ? "aarch64" : arch === "x64" ? "x86_64" : arch;
  if (platform === "darwin") return `${cpu}-apple-darwin`;
  if (platform === "linux") return `${cpu}-unknown-linux-gnu`;
  // macOS and Linux only, stated as a refusal rather than a silent guess.
  throw new Error(`release supports macOS and Linux only; this host reports ${platform}`);
}

function sha256(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/** Every file under `dir`, as paths relative to `dir`, sorted. */
function filesUnder(dir: string, relative = "", collected: string[] = []): string[] {
  for (const entry of readdirSync(join(dir, relative), { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
    const next = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) filesUnder(dir, next, collected);
    else collected.push(next);
  }
  return collected;
}

/**
 * Stage one version's whole tree, then move it into place under one rename.
 *
 * STAGE-THEN-RENAME, because a half-written version directory is worse than no
 * version directory: the binaries and the resources beside them were built
 * together and are only correct together, and `resource_root_from_exe` resolves
 * `resources/` by EXISTENCE. A directory that exists and is half-populated is
 * exactly the empty-`extensions/` company the pointer this replaces used to
 * produce. A killed release leaves a `.staging-*` directory that the next run
 * sweeps.
 */
/**
 * Write one version's whole tree — `bin/`, `resources/`, `manifest.json` — into
 * `treeDir`, which the caller has already emptied.
 *
 * ONE DEFINITION, two callers. `publishVersion` assembles into a `.staging-*`
 * directory it then renames under `~/.chief/versions/`; `scripts/package-release.ts`
 * assembles into a directory it then tars for a release asset. The tree an
 * operator upgrades INTO and the tree CI ships must be byte-for-byte the same
 * shape and carry the same manifest, so the shape and the manifest are written
 * exactly once, here — not copied into the packager where the two could drift.
 */
export function assembleVersionTree(
  treeDir: string,
  version: string,
  binaries: Record<string, string>,
  root: string,
  target: string = hostTarget(),
): { manifestPath: string; resourcesPath: string } {
  mkdirSync(join(treeDir, "bin"), { recursive: true, mode: 0o700 });
  for (const [name, source] of Object.entries(binaries)) {
    installFileAtomically(source, join(treeDir, "bin", name), 0o755);
    if (statSync(join(treeDir, "bin", name)).size === 0) {
      throw new Error(`release staged a zero-byte ${name} binary`);
    }
  }

  const resources = join(treeDir, "resources");
  for (const subtree of RESOURCE_SUBTREES) {
    const from = join(root, subtree);
    if (!existsSync(from)) {
      throw new Error(
        `release cannot package ${subtree}: it is missing from ${root}. ` +
        (subtree.includes("/dist/")
          ? "Run `bun install` (its postinstall builds the workspace) before releasing."
          : "Check the checkout."),
      );
    }
    cpSync(from, join(resources, subtree), { recursive: true, dereference: true });
  }

  // THE PACKAGE IDENTITY THE EXTENSIONS IMPORT BY.
  //
  // Written BEFORE the manifest below, deliberately: these files are part of
  // the payload an install verifies, not scaffolding beside it. A resources
  // tree whose extensions cannot load is not a resources tree.
  for (const { pkg, from } of EXTENSION_RUNTIME_SHIMS) {
    const runtime = join(resources, from);
    if (!existsSync(runtime)) {
      throw new Error(
        `release cannot shim ${pkg}: ${from} is missing from the staged resources. ` +
        "RESOURCE_SUBTREES must package the dist that backs every shim.",
      );
    }
    const dir = join(resources, "node_modules", pkg);
    mkdirSync(dir, { recursive: true });
    writeFileSync(
      join(dir, "package.json"),
      `${JSON.stringify(
        {
          name: pkg,
          type: "module",
          exports: { "./extension-runtime": "./extension-runtime.js" },
        },
        undefined,
        2,
      )}\n`,
      { mode: 0o644 },
    );
    // `export *` and not a re-export list: the runtime's surface is whatever
    // it exports, and a hand-listed set here would be a third place to keep in
    // step with it. Named exports are all the extensions use -- there is no
    // default export to lose.
    const back = relative(dir, join(resources, from)).split(sep).join("/");
    writeFileSync(
      join(dir, "extension-runtime.js"),
      `// Generated by scripts/release-chiefd.ts. The one copy of this runtime\n` +
      `// lives at resources/${from}; this file only gives it the package name\n` +
      `// the shipped extensions import it by.\n` +
      `export * from ${JSON.stringify(back.startsWith(".") ? back : `./${back}`)};\n`,
      { mode: 0o644 },
    );
  }

  const manifestPath = join(treeDir, "manifest.json");
  const manifest = {
    version,
    target,
    piFloor: piFloor(root),
    binaries: Object.fromEntries(
      Object.keys(binaries).sort().map((name) => [name, sha256(join(treeDir, "bin", name))]),
    ),
    resources: Object.fromEntries(
      filesUnder(resources).map((file) => [file, sha256(join(resources, file))]),
    ),
  };
  writeFileSync(manifestPath, `${JSON.stringify(manifest, undefined, 2)}\n`, { mode: 0o644 });
  return { manifestPath, resourcesPath: resources };
}

function publishVersion(
  installHome: string,
  version: string,
  binaries: Record<string, string>,
  root: string,
): { versionPath: string; resourcesPath: string; manifestPath: string } {
  const versions = join(installHome, "versions");
  requireRealDirectory(versions, "ChiefD versions directory");
  mkdirSync(versions, { recursive: true, mode: 0o700 });
  chmodSync(versions, 0o700);

  // Sweep any staging directory a killed run left behind, before minting one.
  for (const entry of readdirSync(versions, { withFileTypes: true })) {
    if (entry.isDirectory() && entry.name.startsWith(".staging-")) {
      rmSync(join(versions, entry.name), { recursive: true, force: true });
    }
  }

  const staging = join(versions, `.staging-${version}-${process.pid}`);
  rmSync(staging, { recursive: true, force: true });
  assembleVersionTree(staging, version, binaries, root);

  const versionPath = join(versions, version);
  // Re-releasing the SAME version is the ordinary dev loop (`bun run release`
  // twice), so the old tree is removed and replaced rather than refused.
  rmSync(versionPath, { recursive: true, force: true });
  renameSync(staging, versionPath);

  return {
    versionPath,
    resourcesPath: join(versionPath, "resources"),
    manifestPath: join(versionPath, "manifest.json"),
  };
}

/**
 * Point `~/.chief/bin/<name>` at this version's binary.
 *
 * A SYMLINK, replaced by `rename(2)` over a sibling temporary — never an
 * overwrite of the file itself. That is what makes an install safe while a
 * company is running: a live `chiefd` holds its own inode open, Unix keeps it
 * alive, and the daemon goes on resolving ITS OWN version's resources until the
 * operator restarts it. Overwriting the binary in place would change what a
 * running process's `current_exe()` names underneath it.
 */
function pointBinLink(binDirectory: string, name: string, versionPath: string): string {
  const link = join(binDirectory, name);
  const existing = lstatIfPresent(link);
  if (existing && !existing.isSymbolicLink() && !existing.isFile()) {
    throw new Error(`release refuses unsafe ${name} entry at ${link}: expected a symlink or a regular file`);
  }
  const temporary = join(binDirectory, `.${name}.tmp-${process.pid}-${Math.random().toString(16).slice(2)}`);
  rmSync(temporary, { force: true });
  symlinkSync(join(versionPath, "bin", name), temporary);
  renameSync(temporary, link);
  return link;
}

/**
 * Build the three binaries and publish them, with the resources they were built
 * with, as one versioned install.
 *
 * # The shape, and why it is not three files in a bin directory any more
 *
 * ```text
 * ~/.chief/
 *   bin/{chief,chiefd,beacond}       symlinks into the version below
 *   versions/<version>/
 *     bin/{chief,chiefd,beacond}
 *     resources/packages/piing/...   what every binary resolves Pi assets through
 *     manifest.json                  version, target, Pi floor, checksums
 * ```
 *
 * TOMBSTONE: `~/.chief/launcher-root`. This function used to write the absolute
 * path of THIS CHECKOUT into that file, and the installed binaries read it to
 * find their Pi extensions and skills. It worked, and it made the binaries a
 * front end for a git working copy that had to stay on disk at a compatible
 * revision — so a user who never cloned anything could not be served, and
 * neither the curl installer nor `chief upgrade` could exist. The resources are
 * COPIED into the version directory now, and each binary finds them beside
 * itself. `host_primitives::install` carries the full account, including the
 * two incidents the pointer's silent absence caused.
 *
 * The dev loop is unchanged in shape: `bun run release` after a pull installs a
 * new version tree and re-points the symlinks. It is NOT a special path — it
 * produces the same layout the release tarballs unpack to, which is the point.
 */
export function releaseChiefd(options: ReleaseChiefdOptions = {}): ReleaseChiefdResult {
  const root = options.root ?? launcherRoot;
  const environment = options.environment ?? process.env;
  const home = homeFor(environment);
  const version = releaseVersion(root, environment);
  // `node_modules` is release input, not an incidental developer cache: without
  // this sync a git pull can leave the new launcher source paired with stale Pi
  // bytes. The ATTESTATION that used to catch that at Founder startup is
  // deleted with the patch, which makes this sync the only thing standing
  // between a stale hidden store and a launcher that runs against it.
  if (!options.skipCargo) {
    (options.ensureNativeLinker ?? defaultEnsureNativeLinker)(environment);
    (options.installDependencies ?? prepareReleaseDependencies)(root);
    const cargo = compatibleCargoCommand(options, home);
    (options.cargoBuild ?? defaultCargoBuild)(cargo, version);
  }
  // One resolution per binary, then ONE loop that installs them. The
  // chief/chiefd pair is only useful as a pair (P6: the client
  // `exec`s the daemon for `run`, and spawns it for every company), so a
  // release that could install one without the other would be an install that
  // works right up until the first `chief attach`.
  const sources: Record<(typeof RELEASE_BINARIES)[number], string> = {
    chief: (options.resolveBuiltChiefBinary ?? ((r) => defaultBuiltChiefBinary(r, environment)))(root),
    chiefd: (options.resolveBuiltChiefdBinary ?? ((r) => defaultBuiltChiefdBinary(r, environment)))(root),
    beacond: (options.resolveBuiltBeacondBinary ?? ((r) => defaultBuiltBeacondBinary(r, environment)))(root),
  };
  for (const name of RELEASE_BINARIES) {
    if (!existsSync(sources[name])) {
      throw new Error(`release built ${name} but no binary was found at ${sources[name]}; check CARGO_TARGET_DIR and the cargo build`);
    }
  }

  const installHome = options.installHome ?? join(home, ".chief");
  const binDirectory = join(installHome, "bin");
  const action: ReleaseChiefdResult["action"] = existsSync(join(binDirectory, "chief")) ? "updated" : "installed";
  prepareUserChiefBin(installHome, RELEASE_BINARIES.map((name) => join(binDirectory, name)));

  // THE VERSION TREE FIRST, THE SYMLINKS SECOND, and never the other way
  // round. A symlink pointing into a directory that is still being written is
  // an install that is broken for exactly as long as the copy takes, and the
  // window is the whole resources tree.
  const published = publishVersion(installHome, version, sources, root);

  const targets: Record<(typeof RELEASE_BINARIES)[number], string> = {
    chief: pointBinLink(binDirectory, "chief", published.versionPath),
    chiefd: pointBinLink(binDirectory, "chiefd", published.versionPath),
    beacond: pointBinLink(binDirectory, "beacond", published.versionPath),
  };

  // The one destructive step, and it is deliberate: a host released before
  // `chiefd-daemon` became `chiefd` keeps that file until something deletes it,
  // and nothing resolves it any more. Same for `launcher-root` and the Founder
  // skill copy, both of which this release stopped writing — an install that
  // upgrades into this layout must not keep a file whose readers are gone.
  for (const obsolete of OBSOLETE_RELEASE_BINARIES) {
    rmSync(join(binDirectory, obsolete), { force: true });
  }
  rmSync(join(installHome, "launcher-root"), { force: true });
  rmSync(join(installHome, "skills"), { recursive: true, force: true });

  return {
    chiefPath: targets.chief,
    chiefdPath: targets.chiefd,
    beacondPath: targets.beacond,
    action,
    version,
    versionPath: published.versionPath,
    resourcesPath: published.resourcesPath,
    manifestPath: published.manifestPath,
  };
}

if (import.meta.main) {
  try {
    const args = process.argv.slice(2);
    if (args.some((arg) => arg !== "--skip-cargo" && arg !== "--prepare-only")) {
      throw new Error("release supports only --skip-cargo or --prepare-only");
    }
    if (args.includes("--skip-cargo") && args.includes("--prepare-only")) {
      throw new Error("release cannot combine --skip-cargo with --prepare-only");
    }
    if (args.includes("--prepare-only")) {
      prepareReleaseDependencies(launcherRoot);
      console.log("release: launcher dependencies are materialized");
      process.exit(0);
    }
    const result = releaseChiefd({ skipCargo: args.includes("--skip-cargo") });
    console.log(`${result.action} ${result.chiefPath}`);
    console.log(`${result.action} ${result.chiefdPath}`);
    console.log(`${result.action} ${result.beacondPath}`);
    // A hint nobody can copy is a hint nobody follows. Founder and ChiefD
    // flows invoke the installed path directly and never consult PATH, so
    // this line matters only for the human shell -- print the exact command.
    console.log("");
    console.log("To run `chief` from a shell, put its bin directory on PATH:");
    console.log(`    export PATH="${dirname(result.chiefPath)}:$PATH"`);
    console.log("");
    console.log("Then:");
    console.log("    chief       # open Founder here, or enter this directory's company");
    console.log("    chief ls    # list your companies");
    console.log("");
    console.log(`installed version ${result.version} at ${result.versionPath}`);
    console.log(`packaged resources at ${result.resourcesPath}`);
  } catch (error) {
    console.error(`release: ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
