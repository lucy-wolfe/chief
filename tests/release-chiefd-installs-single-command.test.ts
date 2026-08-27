// PARKED — NOT RUN IN CI. This whole `tests/` corpus is the parked `bun test
// tests` suite (see `.github/workflows/ci.yml`'s header and
// `docs/testing/parked-suite-triage.json`); the live jobs are not it. The
// versioned-install / release-artifact emitters this file once covered now have
// their LIVE, CI-gated coverage in
// `packages/testing/test/ReleaseArtifact.test.ts` (the ReactiveScan.test.ts
// precedent — a root-`scripts/` module tested from an active package). Edit that
// file, not this one, for release-emitter behaviour.

import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RESOURCE_SUBTREES, defaultBeacondBin, defaultBuiltBeacondBinary, defaultChiefBin, ensureNativeLinkerWith, hostTarget, nativeLinkerIsPresent, piFloor, releaseChiefd, releaseVersion, userLocalRustupCommand } from "../scripts/release-chiefd";

const dirs: string[] = [];
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), "release-chiefd-600-"));
  dirs.push(dir);
  return dir;
}
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }); });

/**
 * A fixture checkout: three built binaries, plus everything a release READS out
 * of a checkout rather than out of `target/`.
 *
 * The second half grew with the versioned layout, and each entry is a real
 * dependency rather than defensive scaffolding: `RESOURCE_SUBTREES` is copied
 * into the version's `resources/` and the release refuses a missing one;
 * `Cargo.toml`'s `[workspace.package] version` is what names the version
 * directory; and `pi_floor.rs` is where the manifest's Pi floor is parsed from,
 * because a release must never carry a second copy of that number.
 */
function seedBuiltBinary(root: string, contents: string): string {
  const dir = join(root, "apps", "chiefd", "target", "release");
  mkdirSync(dir, { recursive: true });
  const path = join(dir, "chief");
  writeFileSync(path, contents);
  chmodSync(path, 0o755);
  writeFileSync(join(dir, "chiefd"), `CHIEFD:${contents}`);
  chmodSync(join(dir, "chiefd"), 0o755);
  writeFileSync(join(dir, "beacond"), `BEACOND:${contents}`);
  chmodSync(join(dir, "beacond"), 0o755);
  for (const subtree of RESOURCE_SUBTREES) {
    mkdirSync(join(root, subtree), { recursive: true });
  }
  const founder = join(root, "packages", "piing", "skills", "founder");
  mkdirSync(founder, { recursive: true });
  writeFileSync(join(founder, "SKILL.md"), "---\nname: founder\n---\n\n# Founder\n");
  writeFileSync(join(founder, "AGENTS.md"), "# Founder contract\n");
  writeFileSync(join(root, "packages", "piing", "extensions", "organization-intercom.ts"), "export const intercom = 1\n");
  writeFileSync(join(root, "packages", "piing", "dist", "extensionruntime", "index.js"), "export const runtime = 1\n");
  mkdirSync(join(root, "apps", "chiefd", "crates", "host-primitives", "src"), { recursive: true });
  writeFileSync(
    join(root, "apps", "chiefd", "crates", "host-primitives", "src", "pi_floor.rs"),
    'pub const MINIMUM_PI_VERSION: &str = "0.80.10";\n',
  );
  writeFileSync(join(root, "apps", "chiefd", "Cargo.toml"), '[workspace.package]\nversion = "9.9.9"\n');
  return path;
}

describe("#600 release:chiefd installs one public ChiefD command", () => {
  test("never reuses a different user's ambient Rustup for a clean release home", () => {
    const home = "/srv/clean-operator";
    const expected = join(home, ".cargo", "bin", "rustup");
    const calls: string[] = [];

    expect(userLocalRustupCommand(home, (command) => {
      calls.push(command);
      return command === expected;
    })).toBe(expected);
    expect(calls).toEqual([expected]);

    expect(userLocalRustupCommand(home, () => false)).toBeUndefined();
  });

  test("a real release synchronizes the lockfile-pinned Pi patch before it builds and publishes ChiefD", () => {
    const root = scratch();
    const home = scratch();
    const calls: string[] = [];
    seedBuiltBinary(root, "CHIEFD-SYNCED\n");

    const result = releaseChiefd({
      root,
      environment: { HOME: home },
      ensureNativeLinker: () => {},
      installDependencies: (installedRoot) => { calls.push(`install:${installedRoot}`); },
      cargoBuild: () => { calls.push("cargo"); },
    });

    expect(calls).toEqual([`install:${root}`, "cargo"]);
    expect(readFileSync(result.chiefPath, "utf8")).toBe("CHIEFD-SYNCED\n");
  });

  test("defaults to the user-owned ChiefD home binary, never /usr/local/bin or sudo", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-MACOS\n");
    const result = releaseChiefd({ root, skipCargo: true, environment: { HOME: home, PATH: "/no/global/bin" } });
    expect(result.chiefPath).toBe(defaultChiefBin(home));
    expect(result.chiefPath).toBe(join(home, ".chief", "bin", "chief"));
    expect(readFileSync(result.chiefPath, "utf8")).toBe("CHIEFD-MACOS\n");
    expect(result.beacondPath).toBe(defaultBeacondBin(home));
    expect(readFileSync(result.beacondPath, "utf8")).toBe("BEACOND:CHIEFD-MACOS\n");
    expect(result.chiefPath).not.toContain("/usr/local/bin");
    // TOMBSTONE: `~/.chief/skills/founder`, a second copy of the Founder skill
    // that nothing in the workspace ever read — `founder_pi.rs` builds its
    // `--skill` path under the resource root, and no Rust file names
    // `~/.chief/skills`. The resources tree carries the same two files at the
    // path the binary actually resolves, so the copy is deleted rather than
    // kept, and an install that upgrades into this layout removes it.
    expect(existsSync(join(home, ".chief", "skills"))).toBe(false);
    expect(readFileSync(join(result.resourcesPath, "packages/piing/skills/founder/AGENTS.md"), "utf8"))
      .toContain("Founder contract");
  });

  test("refuses a symlinked ChiefD bin DIRECTORY, and replaces a hostile bin ENTRY without following it", () => {
    const root = scratch();
    const home = scratch();
    const outside = scratch();
    seedBuiltBinary(root, "CHIEFD-SAFE\n");
    mkdirSync(join(home, ".chief"), { recursive: true });
    symlinkSync(outside, join(home, ".chief", "bin"));
    expect(() => releaseChiefd({ root, skipCargo: true, environment: { HOME: home } })).toThrow(/unsafe ChiefD binary directory/);

    // THE ENTRY IS A DIFFERENT QUESTION NOW, and the answer changed with the
    // layout rather than being relaxed. `bin/chief` IS a symlink in a correct
    // install, so "refuse every symlink here" would refuse every second
    // release. What made the old refusal necessary was the WRITE: an atomic
    // copy over `bin/chief` followed the link and wrote through it. The
    // publisher does not write through anything — it creates a sibling
    // temporary link and `rename(2)`s it over whatever is there — so a hostile
    // link is replaced, not followed, and the file it pointed at is untouched.
    rmSync(join(home, ".chief", "bin"));
    mkdirSync(join(home, ".chief", "bin"), { recursive: true });
    const victim = join(outside, "redirected-chief");
    writeFileSync(victim, "do not touch\n");
    symlinkSync(victim, join(home, ".chief", "bin", "chief"));

    const result = releaseChiefd({ root, skipCargo: true, environment: { HOME: home } });

    expect(readFileSync(victim, "utf8")).toBe("do not touch\n");
    expect(lstatSync(result.chiefPath).isSymbolicLink()).toBeTrue();
    expect(readFileSync(result.chiefPath, "utf8")).toBe("CHIEFD-SAFE\n");
  });

  test("refuses a bin entry that is neither a symlink nor a regular file", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-DIR-ENTRY\n");
    // A DIRECTORY at `bin/beacond`. `rename(2)` over one fails with EISDIR
    // partway through publication, which is a half-installed box; refusing up
    // front is the difference between one message and three broken symlinks.
    mkdirSync(join(home, ".chief", "bin", "beacond"), { recursive: true });

    expect(() => releaseChiefd({ root, skipCargo: true, environment: { HOME: home } }))
      .toThrow(/unsafe beacond binary target/);
  });

  test("publishes a versioned install: bin symlinks, resources beside the binaries, and a manifest", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-600\\n");

    const result = releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } });

    expect(result.version).toBe("9.9.9");
    expect(result.versionPath).toBe(join(installHome, "versions", "9.9.9"));
    expect(result.chiefPath).toBe(join(installHome, "bin", "chief"));

    // THE BIN ENTRY IS A SYMLINK, and that is the load-bearing property: an
    // upgrade re-points it with rename(2) rather than overwriting a file, so a
    // running daemon keeps its own inode and its own resources.
    expect(lstatSync(result.chiefPath).isSymbolicLink()).toBeTrue();
    expect(readFileSync(result.chiefPath, "utf8")).toBe("CHIEFD-600\\n");
    expect(statSync(join(result.versionPath, "bin", "chief")).mode & 0o111).not.toBe(0);

    // RESOURCES TWO LEVELS ABOVE THE BINARY — the exact expression
    // `host_primitives::install::resource_root_beside` evaluates.
    expect(result.resourcesPath).toBe(join(result.versionPath, "resources"));
    for (const subtree of RESOURCE_SUBTREES) {
      expect(existsSync(join(result.resourcesPath, subtree))).toBe(true);
    }
    expect(readFileSync(join(result.resourcesPath, "packages/piing/skills/founder/SKILL.md"), "utf8"))
      .toContain("name: founder");

    const manifest = JSON.parse(readFileSync(result.manifestPath, "utf8")) as Record<string, unknown>;
    expect(manifest.version).toBe("9.9.9");
    expect(manifest.target).toBe(hostTarget());
    // Read from the Rust constant, never transcribed: `chief upgrade` reads
    // this field to decide whether to offer Pi's own updater, and the person it
    // decides for cannot check it.
    expect(manifest.piFloor).toBe(piFloor(root));
    expect(Object.keys(manifest.binaries as object).sort()).toEqual(["beacond", "chief", "chiefd"]);
  });

  test("a stamped version names the install directory, so the binary and its directory cannot disagree", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-STAMPED\n");
    let stampedForCargo: string | undefined;

    const result = releaseChiefd({
      installHome,
      root,
      environment: { HOME: home, CHIEF_RELEASE_VERSION: "2.0.7" },
      ensureNativeLinker: () => {},
      installDependencies: () => {},
      cargoBuild: (_command, version) => { stampedForCargo = version; },
    });

    // ONE value: what cargo bakes into `--version` and what names the
    // directory. `chief upgrade` compares the two; a disagreement is an
    // upgrade that reports itself as never having landed, for ever.
    expect(stampedForCargo).toBe("2.0.7");
    expect(result.version).toBe("2.0.7");
    expect(result.versionPath).toBe(join(installHome, "versions", "2.0.7"));
    expect(releaseVersion(root, { CHIEF_RELEASE_VERSION: "2.0.7" })).toBe("2.0.7");
    expect(releaseVersion(root, {})).toBe("9.9.9");
  });

  test("refuses a version that is not a plain identifier, because it becomes a directory name", () => {
    const root = scratch();
    seedBuiltBinary(root, "CHIEFD-BAD-VERSION\n");
    expect(() => releaseVersion(root, { CHIEF_RELEASE_VERSION: "../../etc" })).toThrow(/becomes a directory name/);
    expect(() => releaseVersion(root, { CHIEF_RELEASE_VERSION: "2.0/7" })).toThrow(/becomes a directory name/);
  });

  test("refuses to publish a version whose resources are missing rather than installing half of one", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-NO-DIST\n");
    rmSync(join(root, "packages", "piing", "dist"), { recursive: true, force: true });

    expect(() => releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } }))
      .toThrow(/dist\/extensionruntime/);
    // STAGE-THEN-RENAME: nothing was published, so nothing resolves. A version
    // directory that exists and is half-populated is the empty-`extensions/`
    // company the launcher-root pointer used to produce.
    expect(existsSync(join(installHome, "versions", "9.9.9"))).toBe(false);
  });

  test("re-releasing the same version replaces its tree and leaves no staging directory behind", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    seedBuiltBinary(root, "FIRST\n");
    const first = releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } });
    seedBuiltBinary(root, "SECOND\n");
    const second = releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } });

    expect(first.action).toBe("installed");
    expect(second.action).toBe("updated");
    expect(readFileSync(second.chiefPath, "utf8")).toBe("SECOND\n");
    expect(readdirSync(join(installHome, "versions"))).toEqual(["9.9.9"]);
  });

  test("installs nothing at all when the build produced no binary", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    // The version fixture without the binaries: the release must refuse before
    // it mints a single directory.
    mkdirSync(join(root, "apps", "chiefd"), { recursive: true });
    writeFileSync(join(root, "apps", "chiefd", "Cargo.toml"), '[workspace.package]\nversion = "9.9.9"\n');

    expect(() => releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } })).toThrow(/no binary was found/);
    expect(existsSync(join(installHome, "bin", "chief"))).toBe(false);
  });

  test("resolves beacond beside chief and fails before installing when it is missing", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD\n");
    rmSync(defaultBuiltBeacondBinary(root, {}));

    expect(defaultBuiltBeacondBinary(root, { CARGO_TARGET_DIR: "/shared/target" }))
      .toBe("/shared/target/release/beacond");
    expect(() => releaseChiefd({ root, skipCargo: true, environment: { HOME: home } }))
      .toThrow(/beacond.*no binary was found/);
    expect(existsSync(defaultChiefBin(home))).toBe(false);
  });

  test("beacond lands beside chief, as a symlink into the same version", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD\n");

    const result = releaseChiefd({ root, skipCargo: true, environment: { HOME: home } });

    expect(result.beacondPath).toBe(defaultBeacondBin(home));
    expect(lstatSync(result.beacondPath).isSymbolicLink()).toBeTrue();
    // ONE VERSION for all three. The pair is only useful as a pair, and a box
    // where `chief` and `chiefd` resolve to different builds is the deploy
    // failure this list of binaries exists to make impossible.
    expect(readFileSync(result.beacondPath, "utf8")).toBe("BEACOND:CHIEFD\n");
    expect(existsSync(join(result.versionPath, "bin", "chiefd"))).toBe(true);
  });

  test("a clean old-Cargo host provisions Rustup Cargo before the release build", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-TOOLCHAIN\n");
    let provisioned = false;
    let buildStarted = false;
    let buildCommand: readonly string[] = [];
    const result = releaseChiefd({
      root,
      environment: { HOME: home },
      ensureNativeLinker: () => {},
      installDependencies: () => {},
      cargoVersion: (command) => command[0] === "rustup" ? "cargo 1.95.0 (rustup)" : "cargo 1.66.1 (debian)",
      provisionCompatibleCargo: (provisionHome) => {
        provisioned = provisionHome === home;
        return ["rustup", "run", "1.95.0", "cargo"];
      },
      cargoBuild: (command) => { buildStarted = true; buildCommand = command; },
    });
    expect(provisioned).toBe(true);
    expect(buildStarted).toBe(true);
    expect(buildCommand).toEqual(["rustup", "run", "1.95.0", "cargo"]);
    expect(readFileSync(result.chiefPath, "utf8")).toBe("CHIEFD-TOOLCHAIN\n");
  });

  test("a host without Cargo also takes the user-local compatible-toolchain path", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-NO-CARGO\n");
    let provisioned = false;

    releaseChiefd({
      root,
      environment: { HOME: home },
      ensureNativeLinker: () => {},
      installDependencies: () => {},
      cargoVersion: (command) => {
        if (command[0] === "cargo") throw new Error("cargo executable unavailable");
        return "cargo 1.95.0 (rustup)";
      },
      provisionCompatibleCargo: () => { provisioned = true; return ["rustup", "run", "1.95.0", "cargo"]; },
      cargoBuild: () => {},
    });
    expect(provisioned).toBe(true);
  });

  test("accepts the workspace's minimum Cargo version before building", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-MINIMUM\n");
    let buildStarted = false;
    let buildCommand: readonly string[] = [];

    releaseChiefd({
      root,
      environment: { HOME: home },
      ensureNativeLinker: () => {},
      installDependencies: () => {},
      cargoVersion: () => "cargo 1.95.0 (minimum)",
      cargoBuild: (command) => { buildStarted = true; buildCommand = command; },
    });
    expect(buildStarted).toBe(true);
    expect(buildCommand).toEqual(["cargo"]);
  });

  test("uses CARGO_TARGET_DIR for the built artifacts it publishes", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    const target = scratch();
    const release = join(target, "release");
    mkdirSync(release, { recursive: true });
    writeFileSync(join(release, "chief"), "CARGO-600\n");
    writeFileSync(join(release, "chiefd"), "CARGO-600-DAEMON\n");
    writeFileSync(join(release, "beacond"), "BEACOND-CARGO-600\n");
    seedBuiltBinary(root, "unused fixture binary");

    const first = releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home, CARGO_TARGET_DIR: target } });
    const second = releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home, CARGO_TARGET_DIR: target } });

    expect(first.action).toBe("installed");
    expect(second.action).toBe("updated");
    expect(readFileSync(first.chiefPath, "utf8")).toBe("CARGO-600\n");
  });

  test("removes a legacy launcher-root pointer and Founder skill copy rather than leaving readers for them", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-LEGACY\n");
    // What an install released before the versioned layout looks like.
    mkdirSync(join(installHome, "skills", "founder"), { recursive: true });
    writeFileSync(join(installHome, "launcher-root"), "/old/checkout\n");
    writeFileSync(join(installHome, "skills", "founder", "SKILL.md"), "stale\n");

    releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } });

    // BOTH DELETED, not left in place. Their readers are gone, and a file
    // nobody reads is a second control plane waiting to be believed — which is
    // exactly what `launcher-root` became the day the daemon stopped agreeing
    // with the client about which directory it lived in.
    expect(existsSync(join(installHome, "launcher-root"))).toBe(false);
    expect(existsSync(join(installHome, "skills"))).toBe(false);
  });

  test("refuses a symlinked ChiefD home before mutating anything through it", () => {
    const root = scratch();
    const home = scratch();
    const outside = scratch();
    const sentinel = join(outside, "sentinel");
    seedBuiltBinary(root, "CHIEFD-HOME-SYMLINK\n");
    writeFileSync(sentinel, "do not touch\n");
    symlinkSync(outside, join(home, ".chief"));

    expect(() => releaseChiefd({ root, skipCargo: true, environment: { HOME: home } }))
      .toThrow(/unsafe ChiefD home/);
    expect(readFileSync(sentinel, "utf8")).toBe("do not touch\n");
    expect(existsSync(join(outside, "versions"))).toBe(false);
  });

  test("refuses a symlinked versions directory instead of publishing an install through it", () => {
    const root = scratch();
    const installHome = join(scratch(), "chief-home");
    const home = scratch();
    const outside = scratch();
    seedBuiltBinary(root, "CHIEFD-VERSIONS-SYMLINK\n");
    mkdirSync(installHome, { recursive: true });
    symlinkSync(outside, join(installHome, "versions"));

    expect(() => releaseChiefd({ installHome, root, skipCargo: true, environment: { HOME: home } }))
      .toThrow(/unsafe ChiefD versions directory/);
    expect(existsSync(join(outside, "9.9.9"))).toBe(false);
  });
});


describe("#707 native linker preflight (detect and refuse, never install)", () => {
  test("present linker: a no-op, cargo build still runs", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-LINKER-PRESENT\n");
    let buildStarted = false;

    releaseChiefd({
      root,
      environment: { HOME: home },
      ensureNativeLinker: (env) => {
        ensureNativeLinkerWith(env, { linkerPresent: () => true });
      },
      installDependencies: () => {},
      cargoVersion: () => "cargo 1.95.0 (present-linker)",
      cargoBuild: () => { buildStarted = true; },
    });
    expect(buildStarted).toBe(true);
  });

  test("missing linker, Linux: refuses with the exact apt-get command, never spawns a process itself", () => {
    expect(() =>
      ensureNativeLinkerWith({ HOME: "/home/dev" }, { linkerPresent: () => false, platform: "linux" }),
    ).toThrow(/sudo apt-get update && sudo apt-get install -y build-essential/);
  });

  test("missing linker, macOS: refuses with the Xcode command-line-tools command, never the apt-get one", () => {
    let error: unknown;
    try {
      ensureNativeLinkerWith({ HOME: "/Users/dev" }, { linkerPresent: () => false, platform: "darwin" });
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(Error);
    const message = (error as Error).message;
    expect(message).toContain("xcode-select --install");
    expect(message).not.toContain("apt-get");
  });

  test("missing linker, an unmodeled third platform: still refuses (fail closed), falls through to the apt-get message rather than silently passing", () => {
    expect(() =>
      ensureNativeLinkerWith({ HOME: "/home/dev" }, { linkerPresent: () => false, platform: "win32" as NodeJS.Platform }),
    ).toThrow(/no C compiler.*native linker/);
  });

  test("never installs anything: the exported function accepts no apt-get/root/install dependency at all", () => {
    // Type-level guarantee as much as a runtime one -- if an install path were
    // reintroduced, it would need a parameter here, and this call intentionally
    // supplies nothing beyond linkerPresent/platform.
    expect(() => ensureNativeLinkerWith({}, { linkerPresent: () => false, platform: "linux" })).toThrow();
  });

  test("nativeLinkerIsPresent probes exactly the command name `cc`, nothing else", () => {
    const probed: string[] = [];
    nativeLinkerIsPresent((command) => { probed.push(command); return true; });
    expect(probed).toEqual(["cc"]);
  });

  test("releaseChiefd wires the linker preflight to run BEFORE installDependencies/cargoBuild, and a refusal there never reaches cargo", () => {
    const root = scratch();
    const home = scratch();
    seedBuiltBinary(root, "CHIEFD-PREFLIGHT-ORDER\n");
    const order: string[] = [];

    expect(() =>
      releaseChiefd({
        root,
        environment: { HOME: home },
        ensureNativeLinker: () => { order.push("linker"); throw new Error("release:chiefd: the native C linker (`cc`) was not found"); },
        installDependencies: () => { order.push("install"); },
        cargoBuild: () => { order.push("cargo"); },
      }),
    ).toThrow(/native C linker/);
    expect(order).toEqual(["linker"]);
  });
});
