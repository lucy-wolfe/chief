#!/usr/bin/env bun
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { RELEASE_BINARIES, assembleVersionTree, releaseVersion } from "./release-chiefd";

/**
 * Package ONE target's release tarball for CI.
 *
 * `scripts/release-chiefd.ts` installs a version INTO `~/.chief` on the machine
 * that ran it; this script produces the artifact CI uploads to a GitHub
 * release, for a target that is not necessarily the runner's own. The two share
 * `assembleVersionTree` deliberately — the tree an operator upgrades into and
 * the tree shipped as a release asset are the same shape and carry the same
 * `manifest.json`, so neither the layout nor the manifest is written twice
 * where the two copies could drift.
 *
 * The tarball is `chief-<version>-<target>.tar.gz`, whose top level is exactly
 * what `chief upgrade` unpacks into `~/.chief/versions/<version>`:
 *
 *   bin/{chief,chiefd,beacond}
 *   resources/…
 *   manifest.json
 *
 * `chief upgrade` and `install.sh` verify the tarball against the release's
 * `SHA256SUMS` before unpacking, so this script does not sign the archive; the
 * release workflow computes `SHA256SUMS` across every target's tarball once.
 */

/** The four targets the release workflow builds, and the only ones this packages. */
export const RELEASE_TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
] as const;

/** The repository root — this file lives one directory below it. */
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** The release asset name for one version and target. Matched by `chief upgrade`. */
export function assetName(version: string, target: string): string {
  return `chief-${version}-${target}.tar.gz`;
}

/**
 * Where `cargo build --target <target>` leaves its binaries.
 *
 * A `--target` build lands under `target/<triple>/release`, NOT the bare
 * `target/release` a host-native `cargo build` uses. Reading the wrong one is
 * how a cross-target job would package the RUNNER's architecture under the
 * cross target's name — an asset that installs and then cannot run.
 */
export function targetArtifactDir(
  root: string,
  target: string,
  environment: Record<string, string | undefined> = process.env,
): string {
  const base = environment.CARGO_TARGET_DIR?.trim() || join(root, "apps", "chiefd", "target");
  return join(base, target, "release");
}

export interface PackageReleaseOptions {
  /** The target triple to package. Validated against `RELEASE_TARGETS` at runtime. */
  target: string;
  /** Repository root; defaults to this script's own location. */
  root?: string;
  /** Directory the tarball is written into; defaults to `<root>/dist`. */
  outDir?: string;
  environment?: Record<string, string | undefined>;
  /** Skip the cargo build (the binaries are already built, or the caller stubs them). */
  skipCargo?: boolean;
  /** Test seam for the cargo build. */
  cargoBuild?: (root: string, target: string, version: string) => void;
  /** Test seam for resolving the three built binary paths. */
  resolveBinaries?: (root: string, target: string) => Record<string, string>;
  /** Test seam for the archive step. */
  archive?: (tarball: string, treeDir: string) => void;
}

export interface PackageReleaseResult {
  version: string;
  target: string;
  tarball: string;
}

function defaultCargoBuild(root: string, target: string, version: string): void {
  const result = Bun.spawnSync(
    [
      "cargo",
      "build",
      "--release",
      "--locked",
      "--target",
      target,
      "--manifest-path",
      join(root, "apps", "chiefd", "Cargo.toml"),
      ...RELEASE_BINARIES.flatMap((name) => ["--bin", name]),
    ],
    { stdout: "inherit", stderr: "inherit", env: { ...process.env, CHIEF_RELEASE_VERSION: version } },
  );
  if (result.exitCode !== 0) {
    throw new Error(`cargo build --target ${target} failed with exit code ${result.exitCode}`);
  }
}

function defaultResolveBinaries(
  root: string,
  target: string,
  environment: Record<string, string | undefined> = process.env,
): Record<string, string> {
  const dir = targetArtifactDir(root, target, environment);
  return Object.fromEntries(RELEASE_BINARIES.map((name) => [name, join(dir, name)]));
}

function defaultArchive(tarball: string, treeDir: string): void {
  // Reproducibility is not required — `chief upgrade` verifies the tarball
  // against `SHA256SUMS`, then reads the version out of the unpacked
  // `manifest.json`; it never re-hashes the archive against a stored digest.
  const result = Bun.spawnSync(
    ["tar", "-czf", tarball, "-C", treeDir, "bin", "resources", "manifest.json"],
    { stdout: "inherit", stderr: "inherit" },
  );
  if (result.exitCode !== 0) {
    throw new Error(`tar failed to package ${tarball} (exit code ${result.exitCode})`);
  }
}

/** Build (unless skipped) and package one target's release tarball. */
export function packageRelease(options: PackageReleaseOptions): PackageReleaseResult {
  if (!RELEASE_TARGETS.some((target) => target === options.target)) {
    throw new Error(
      `package-release supports macOS and Linux only; ${JSON.stringify(options.target)} is not one of ${RELEASE_TARGETS.join(", ")}`,
    );
  }
  const root = options.root ?? repositoryRoot;
  const environment = options.environment ?? process.env;
  const version = releaseVersion(root, environment);
  const outDir = options.outDir ?? join(root, "dist");

  if (!options.skipCargo) {
    (options.cargoBuild ?? defaultCargoBuild)(root, options.target, version);
  }

  const binaries = (options.resolveBinaries ?? ((r, t) => defaultResolveBinaries(r, t, environment)))(
    root,
    options.target,
  );

  const staging = mkdtempSync(join(tmpdir(), `chief-package-${version}-`));
  try {
    assembleVersionTree(staging, version, binaries, root, options.target);
    mkdirSync(outDir, { recursive: true });
    const tarball = join(outDir, assetName(version, options.target));
    rmSync(tarball, { force: true });
    (options.archive ?? defaultArchive)(tarball, staging);
    if (!options.archive && !existsSync(tarball)) {
      throw new Error(`tar reported success but ${tarball} is missing`);
    }
    return { version, target: options.target, tarball };
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

/** Parse `--target <triple>` and optional `--out <dir>` from an argv tail. */
export function parseArgs(argv: readonly string[]): { target: string; outDir?: string } {
  let target: string | undefined;
  let outDir: string | undefined;
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--target") {
      target = argv[(index += 1)];
    } else if (flag === "--out") {
      outDir = argv[(index += 1)];
    } else {
      throw new Error(`package-release: unknown argument ${JSON.stringify(flag)}`);
    }
  }
  if (!target) {
    throw new Error("package-release: --target <triple> is required");
  }
  return { target, outDir };
}

if (import.meta.main) {
  const { target, outDir } = parseArgs(process.argv.slice(2));
  const result = packageRelease({ target, outDir });
  process.stdout.write(`${result.tarball}\n`);
}
