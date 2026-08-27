// The generalisation of the cold-attach defect (99e0a3e69): NO PROCESS THIS
// PRODUCT SPAWNS MAY BE NAMED BY A BARE NAME.
//
// WHAT WENT WRONG, MEASURED LIVE
// ------------------------------
// `chief attach <stopped company>` could not take an operator from nothing to
// a running company. Three processes each answered "where is Pi?" in their own
// environment and nothing made them agree:
//
//   * `chief attach`'s preflight read `TEAM_LAUNCHER_PI`, else walked ATTACH's
//     PATH -- resolved, and PASSED.
//   * `chiefd`, which builds the launch catalog, read `CHIEFD_PI_BINARY`
//     -- a variable nothing in the product ever set -- and fell back to the
//     literal `"pi"`.
//   * the CEO pane then resolved `pi` a THIRD time against the tmux SERVER's
//     PATH, where it was not, and died at creation.
//
// Every company that ever ran therefore shipped a bare name to tmux, and the
// symptom was `unusable window dimensions "\t\n"`, once per second, forever.
//
// THE RULE THIS ENFORCES
// ----------------------
// A program name written as a literal in shipped source must be an ABSOLUTE
// path. The point is not that PATH lookup is wrong; it is that the process that
// RESOLVES must be the process that RUNS. A literal is the one shape a static
// reader can prove, and it is the shape all three legs of the live defect took.
//
// DERIVED, NOT INVENTORIED
// ------------------------
// The site list is a directory walk. Nothing here names a file, a crate, a
// count, or a spawn site. A new spawn site is found because it is in the tree,
// which is the difference between this and the stale allowlist row that
// survived a file move in #963.
//
// EXCEPTIONS ARE REGISTERED, AND THE REGISTER IS CHECKED BOTH WAYS
// ---------------------------------------------------------------
// `REGISTERED_BARE_NAMES` carries the residue: programs resolved and executed
// by ONE process, in ONE step, where a failure is immediate and named. Each row
// states why. A row that stops matching FAILS and says to delete it, and a
// finding with no row FAILS -- the same bidirectional discipline as
// `scripts/test/sql-only-state.test.mjs`'s `compareSitesToAllowlist`, which
// this deliberately copies rather than inventing a second shape.
//
// COVERAGE BOUNDARY, STATED HONESTLY
// ----------------------------------
// This proves things about LITERALS. A program that arrives as a variable (the
// pane's `pi_binary`, which comes over the wire from chiefd) cannot be decided
// statically; that one is enforced at runtime instead, by
// `chiefd_core`/`chiefd` refusing a non-absolute `--pi-binary` and by
// `chief_cli::actuate::launch_catalog::LaunchCatalog::resolve` refusing a
// relative `piBinary` BY NAME before it can become pane argv. Tests, fixtures
// and `build.rs` are out of scope: they mint throwaway processes by the dozen
// and none of them is a program this product ships.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

import { skipSet } from "./tree-walk-lib.mjs";

/** Directories under the repo root that hold code this product SHIPS. */
export const SCAN_ROOTS = ["apps", "packages"];

/** Path segments that are never product source. */
const EXCLUDED_SEGMENTS = skipSet(["build", "test", "tests", "__tests__", "fixtures"]);

const RUST_EXTENSION = ".rs";
const TS_EXTENSIONS = [".ts", ".tsx"];

/**
 * Files that are test code by NAME rather than by directory.
 * `tests.rs` / `*.test.ts` / `*.spec.ts` are test bodies wherever they sit.
 */
function isTestFile(name) {
  return (
    name === "tests.rs" ||
    name === "build.rs" ||
    name.endsWith(".test.ts") ||
    name.endsWith(".test.tsx") ||
    name.endsWith(".spec.ts")
  );
}

/**
 * Every product source file under `root`, as repo-relative POSIX paths.
 * A directory walk — this is the whole derivation.
 */
export function productSourceFiles(repoRoot, roots = SCAN_ROOTS) {
  const found = [];
  const stack = roots.map((root) => join(repoRoot, root));
  while (stack.length > 0) {
    const directory = stack.pop();
    let entries;
    try {
      entries = readdirSync(directory, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (EXCLUDED_SEGMENTS.has(entry.name)) continue;
      const absolute = join(directory, entry.name);
      if (entry.isDirectory()) {
        stack.push(absolute);
        continue;
      }
      if (!entry.isFile()) continue;
      if (isTestFile(entry.name)) continue;
      const isRust = entry.name.endsWith(RUST_EXTENSION);
      const isTypeScript = TS_EXTENSIONS.some((extension) => entry.name.endsWith(extension));
      if (!isRust && !isTypeScript) continue;
      found.push(relative(repoRoot, absolute).split(sep).join("/"));
    }
  }
  return found.sort();
}

/**
 * Rust production source: everything before the crate's first `#[cfg(test)]`.
 *
 * The repo's own in-crate tripwires (`chief_cli::tmux`, `chief_cli::founder`)
 * cut test bodies exactly this way, and for the same reason: test code spawns
 * throwaway processes and none of them is a product name.
 */
export function productionRust(source) {
  return source.split("#[cfg(test)]")[0];
}

/** A program literal is acceptable only when it is an absolute path. */
export function isAbsoluteProgram(literal) {
  return literal.startsWith("/");
}

/**
 * Line number (1-based) of a byte offset. Computed rather than stored, because
 * a register that carried line numbers would rot on the next edit.
 */
function lineOf(text, index) {
  let line = 1;
  for (let at = 0; at < index; at += 1) {
    if (text[at] === "\n") line += 1;
  }
  return line;
}

/**
 * A `//`-comment or a doc comment. Prose that quotes `Command::new("pi")` while
 * explaining this very defect must not read as a spawn site.
 */
function isCommentLine(text, index) {
  const lineStart = text.lastIndexOf("\n", index) + 1;
  return /^\s*(\/\/|\*|#)/.test(text.slice(lineStart, index));
}

/**
 * The detectors. Each is a named rule with one regex whose FIRST capture group
 * is the program literal.
 *
 * `program` covers a binding or struct field whose NAME says it holds a program
 * (`binary`, `pi_binary`, `memory_program`, `exe`, `curl`), assigned a literal
 * through any of the conversion shapes this codebase uses — including the
 * `unwrap_or*` fallback that WAS the defect in `chiefd` and in
 * `main.rs`'s dead `PiPaths`.
 *
 * `env-program-default` is the second net for the same thing: an environment
 * variable whose NAME says it carries a program, defaulted to a literal. It
 * fires even when the binding is named something else entirely.
 */
export const DETECTORS = [
  {
    id: "rust-command-new",
    language: "rust",
    what: 'Command::new("<program>")',
    pattern: /(?:^|[^\w:.])(?:std::process::|tokio::process::)?Command::new\(\s*"([^"\n]*)"\s*\)/g,
  },
  {
    id: "rust-program-literal",
    language: "rust",
    what: "a program-named binding or field assigned a literal",
    pattern:
      /\b[a-z_]*(?:binary|program|executable|exe|curl)\s*[:=]\s*(?:[\w:]*(?:PathBuf::from|String::from|Path::new)\(|Some\(|&)?"([^"\n]*)"/g,
  },
  {
    id: "rust-program-fallback",
    language: "rust",
    what: "a program-named binding defaulted to a literal by unwrap_or*",
    pattern:
      /\b[a-z_]*(?:binary|program|executable|exe|curl)\b[^;]{0,240}?unwrap_or(?:_else)?\(\s*(?:\|_?\|\s*)?(?:[\w:]*(?:PathBuf::from|String::from)\()?"([^"\n]*)"/g,
  },
  {
    id: "rust-env-program-default",
    language: "rust",
    what: "an environment variable naming a program, defaulted to a literal",
    pattern:
      /env::var\(\s*(?:"[A-Z0-9_]*(?:BINARY|BIN|EXE|PI|RUNTIME|PROGRAM|LAUNCHER)"|[A-Z0-9_]*(?:BINARY|BIN|EXE|PI|RUNTIME|PROGRAM|LAUNCHER)(?:_ENV)?)\s*\)[^;]{0,200}?unwrap_or(?:_else)?\(\s*(?:\|_?\|\s*)?(?:[\w:]*(?:PathBuf::from|String::from)\()?"([^"\n]*)"/g,
  },
  {
    id: "ts-spawn-literal",
    language: "typescript",
    what: "spawn/execFile called with a literal program",
    pattern:
      /(?:Bun\.)?(?:spawnSync|spawn|execFileSync|execFile)\(\s*\[?\s*(?:'([^'\n]*)'|"([^"\n]*)")/g,
  },
];

/**
 * Every bare-name program literal in shipped source.
 *
 * Returns `{ file, detector, program, line }` rows, sorted, with no line number
 * used for identity — identity is `(file, detector, program)`, so a row stays
 * valid while code moves inside a file.
 */
export function scanForBareNamePrograms(repoRoot, files = productSourceFiles(repoRoot)) {
  const findings = [];
  for (const file of files) {
    const isRust = file.endsWith(RUST_EXTENSION);
    let raw;
    try {
      raw = readFileSync(join(repoRoot, file), "utf8");
    } catch {
      continue;
    }
    const text = isRust ? productionRust(raw) : raw;
    for (const detector of DETECTORS) {
      if (detector.language === "rust" !== isRust) continue;
      detector.pattern.lastIndex = 0;
      let match;
      while ((match = detector.pattern.exec(text)) !== null) {
        const program = match[1] ?? match[2];
        if (program === undefined || program.length === 0) continue;
        if (isAbsoluteProgram(program)) continue;
        // A program word is a FILENAME. Anything carrying a character a
        // filename cannot have is a placeholder, an interpolation or a
        // diagnostic string ("<gone>", "${bin}", "not set"), never a program a
        // kernel would be asked to exec.
        if (!/^[A-Za-z0-9._+/-]+$/.test(program)) continue;
        if (program.startsWith("-")) continue;
        if (isCommentLine(text, match.index)) continue;
        findings.push({
          file,
          detector: detector.id,
          program,
          line: lineOf(text, match.index),
        });
      }
    }
  }
  return findings.sort(
    (left, right) =>
      left.file.localeCompare(right.file) ||
      left.detector.localeCompare(right.detector) ||
      left.program.localeCompare(right.program) ||
      left.line - right.line
  );
}

/** Identity of a finding or a register row: never the line number. */
export function rowKey(entry) {
  return `${entry.file} [${entry.detector}] ${entry.program}`;
}

/**
 * The residue: programs a shipped process names by a bare name TODAY.
 *
 * NOT a permission that outlives its subject. Each row is a measured fact with
 * a date and a written reason, checked in both directions by
 * `compareFindingsToRegister`:
 *
 *   * a row that matches nothing FAILS -- the site is gone, delete the row;
 *   * a finding with no row FAILS -- a new bare name must be a decision.
 *
 * The single admissible reason is: THIS process resolves the program and THIS
 * process execs it, in one step, and a failure is immediate and named. That is
 * not the defect -- the defect is a name that crosses a process boundary and
 * gets its final answer from an environment nobody measured. Any row that
 * cannot make that statement must be fixed, not registered.
 */
/**
 * One register row. `registeredOn` and `reason` are optional IN THE TYPE and
 * required BY THE CHECK: `registerShapeViolations` exists to reject a row that
 * omits either, so a type that forbade the omission would make that check
 * unwritable — and the check is the thing that has to fire.
 *
 * @typedef {object} RegisterRow
 * @property {string} file
 * @property {string} detector
 * @property {string} program
 * @property {string} [registeredOn]
 * @property {string} [reason]
 */

export const REGISTERED_BARE_NAMES = [
  {
    file: "apps/chiefd/crates/chief-cli/src/actuate/runner.rs",
    detector: "rust-program-literal",
    program: "tmux",
    registeredOn: "2026-08-10",
    reason:
      "SystemTmuxRunner::default's binary. `chief-cli` resolves tmux against its OWN PATH and " +
      "runs it in the same call; a miss is `HostErr::ToolUnavailable { tool: \"tmux\" }` on the " +
      "spot, not a pane that dies somewhere else. tmux is also a host prerequisite the preflight " +
      "probes before anything is minted, and `SystemTmuxRunner::with_binary` is the seam that " +
      "pins a specific build when two must be told apart.",
  },
  {
    file: "apps/chiefd/crates/chief-cli/src/control.rs",
    detector: "rust-program-literal",
    program: "tmux",
    registeredOn: "2026-08-15",
    reason:
      "`ControlTransport`'s default binary, for the persistent control-mode client. Identical " +
      "in kind to the `SystemTmuxRunner` row above, and the same statement holds: this client " +
      "walks its OWN PATH and execs in the same call, and a miss is `HostErr::ToolUnavailable " +
      "{ tool: \"tmux\" }` on the spot. The name crosses no process boundary — `connect` spawns " +
      "the client and reads its opening block, so a failure to resolve is answered before any " +
      "command is sent, and the transport falls back to the spawn runner rather than degrading " +
      "silently. `ControlTransport::with_binary` is the seam that pins a specific build, exactly " +
      "as `SystemTmuxRunner::with_binary` is.",
  },
  {
    file: "apps/chiefd/crates/chief-cli/src/tmux.rs",
    detector: "rust-command-new",
    program: "tmux",
    registeredOn: "2026-08-10",
    reason:
      "The lifecycle module's own tmux invocations (`run` and `attach`). Same one-process " +
      "resolution as the actuator's runner: this client walks its own PATH and execs in the same " +
      "statement, and `attach` replaces this process, so there is no third environment.",
  },
  {
    file: "apps/chiefd/crates/chief-cli/src/preflight.rs",
    detector: "rust-command-new",
    program: "tmux",
    registeredOn: "2026-08-10",
    reason:
      "`RealHost::tmux_reachable` — the probe whose whole job is to answer whether THIS process " +
      "can reach tmux. Resolving it anywhere else would answer a question nobody asked.",
  },
  {
    file: "packages/testing/src/TmuxHostedCompanyDaemon.ts",
    detector: "ts-spawn-literal",
    program: "tmux",
    registeredOn: "2026-08-10",
    reason:
      "The harness's own `tmux -L <socket> kill-server` teardown. Resolved and run by the test " +
      "process itself, against the socket that process created; it names no program for anything " +
      "else to run.",
  },
  {
    file: "apps/chiefd/crates/chief-cli/src/upgrade.rs",
    detector: "rust-command-new",
    program: "curl",
    registeredOn: "2026-08-25",
    reason:
      "`chief upgrade`'s TLS fetch and download of the release tarball and SHA256SUMS from " +
      "github.com. This is the one product path that must speak to a host off the box, and it " +
      "resolves curl against its own PATH and runs it in the same call — a miss is a plain " +
      "'curl could not be started' before anything is changed, never a second process left to " +
      "disagree about where curl is. curl is a prerequisite of the installer that put chief on " +
      "the box, so it is present wherever an upgrade can run.",
  },
  {
    file: "apps/chiefd/crates/chief-cli/src/upgrade.rs",
    detector: "rust-command-new",
    program: "pi",
    registeredOn: "2026-08-25",
    reason:
      "`chief upgrade`'s Pi-floor gate: it reads `pi --version` and, when the box is below the " +
      "target release's floor and the operator agrees, runs `pi update`. Pi is the agent runtime " +
      "every person runs and is resolved against this same process's PATH; the answer is used " +
      "here and nowhere else, so no second resolver can disagree about which Pi this is.",
  },
  {
    file: "apps/chiefd/crates/chief-cli/src/upgrade.rs",
    detector: "rust-command-new",
    program: "tar",
    registeredOn: "2026-08-25",
    reason:
      "`chief upgrade` unpacks the verified release tarball with `tar -xzf` into a staging " +
      "directory. It runs only after the tarball's sha256 matches SHA256SUMS, is resolved " +
      "against this process's own PATH, and its result is consumed in the same call; tar ships " +
      "with both macOS and every Linux this product supports.",
  },
];

/**
 * Both directions at once, as one comparable value so the assertion diff IS the
 * report. Deliberately the same shape as
 * `scripts/test/sql-only-state.test.mjs`'s `compareSitesToAllowlist`.
 */
/**
 * @param {{file: string, detector: string, program: string, line: number}[]} findings
 * @param {RegisterRow[]} register
 */
export function compareFindingsToRegister(findings, register = REGISTERED_BARE_NAMES) {
  const registered = new Set(register.map(rowKey));
  const present = new Set(findings.map(rowKey));
  return {
    unregistered: findings
      .filter((finding) => !registered.has(rowKey(finding)))
      .map((finding) => `${finding.file}:${finding.line} [${finding.detector}] "${finding.program}"`),
    stale: register
      .filter((row) => !present.has(rowKey(row)))
      .map((row) => `${rowKey(row)} — matches nothing today; delete this row`),
  };
}

/** Row-shape violations: a register row that cannot be checked is not a fact. */
/** @param {RegisterRow[]} register */
export function registerShapeViolations(register = REGISTERED_BARE_NAMES) {
  const violations = [];
  const seen = new Set();
  for (const row of register) {
    const key = rowKey(row);
    if (seen.has(key)) violations.push(`${key}: registered twice`);
    seen.add(key);
    if (!row.reason || row.reason.trim().length < 40) {
      violations.push(`${key}: needs a written reason, not a label`);
    }
    if (!/^\d{4}-\d{2}-\d{2}$/.test(row.registeredOn ?? "")) {
      violations.push(`${key}: needs a registration date`);
    }
    if (!DETECTORS.some((detector) => detector.id === row.detector)) {
      violations.push(`${key}: names a detector that does not exist`);
    }
    if (isAbsoluteProgram(row.program)) {
      violations.push(`${key}: an absolute program needs no registration`);
    }
  }
  return violations;
}

/**
 * THE SECOND PROPERTY: one product question, one resolver.
 *
 * A bare name is the LOUD form of the defect. The quiet form is two functions
 * that both answer "where is Pi?" and answer it differently, which is what
 * shipped twice. `99e0a3e69` collapsed three answers on the SPAWN path and left
 * a fourth on the PREFLIGHT path, and the two disagreed about a pinned build in
 * the checkout: on a host with a good checkout and no `pi` on PATH, Founder
 * started and `chief attach` refused.
 *
 * So: each of these facts may be stated in exactly ONE production file. A
 * second file naming one is a second resolver, whether or not it spells a bare
 * name. Derived from the tree, like everything else here — the fact list is the
 * rule, the site list is a scan.
 */
export const SINGLE_SOURCE_FACTS = [
  {
    id: "pi-path-lookup",
    needle: 'candidates_on_path("pi"',
    what: "where Pi is found",
    reading: /candidates_on_path\(\s*"pi"/,
  },
];

/*
 * TOMBSTONES, 2026-08-24: `pi-pin-env` (`TEAM_LAUNCHER_PI`) and
 * `pi-checkout-path` (`node_modules/.bin/pi`) were the two facts this scan
 * protected. Both are deleted from the product — chief runs the Pi the operator
 * installed, resolved on `PATH`, with no pin and no bundled build — so the ONE
 * remaining statement of "where is Pi?" is the `PATH` walk above.
 *
 * The rule did not change and neither did its teeth: this fact must appear in
 * exactly one production file, and a tree stating it NOWHERE is a violation too,
 * because "no violations" from a scan that read nothing is not evidence.
 */

/**
 * Production files that STATE one of the single-source facts. More than one per
 * fact is a second resolver.
 */
export function resolverSites(repoRoot, files = productSourceFiles(repoRoot)) {
  const byFact = new Map(SINGLE_SOURCE_FACTS.map((fact) => [fact.id, []]));
  for (const file of files) {
    if (!file.endsWith(RUST_EXTENSION)) continue;
    let raw;
    try {
      raw = readFileSync(join(repoRoot, file), "utf8");
    } catch {
      continue;
    }
    const text = productionRust(raw);
    for (const fact of SINGLE_SOURCE_FACTS) {
      if (fact.reading.test(text)) byFact.get(fact.id).push(file);
    }
  }
  return byFact;
}

/** One resolver per fact, or a violation naming every file that answers it. */
export function secondResolverViolations(repoRoot, files = productSourceFiles(repoRoot)) {
  const sites = resolverSites(repoRoot, files);
  const violations = [];
  for (const fact of SINGLE_SOURCE_FACTS) {
    const found = sites.get(fact.id) ?? [];
    if (found.length === 0) {
      violations.push(
        `no production file states ${fact.what} ("${fact.needle}") — the scan reads nothing, ` +
          `which is not the same as the fact being gone`
      );
    } else if (found.length > 1) {
      violations.push(
        `${found.length} production files answer "${fact.what}": ${found.join(", ")}. ` +
          `One product question, one resolver — the second answer is the one nobody checks.`
      );
    }
  }
  return violations;
}

/** Existence of `statSync` targets, so a `file` typo cannot register nothing. */
/**
 * @param {string} repoRoot
 * @param {RegisterRow[]} register
 */
export function registerRowsNamingMissingFiles(repoRoot, register = REGISTERED_BARE_NAMES) {
  return register
    .filter((row) => {
      try {
        return !statSync(join(repoRoot, row.file)).isFile();
      } catch {
        return true;
      }
    })
    .map((row) => `${rowKey(row)}: registered path does not exist — delete this row`);
}
