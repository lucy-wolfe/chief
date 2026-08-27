/**
 * reactive-scan — the TypeScript sibling of chiefd's `clippy.toml`
 * `disallowed-methods`: Mandate 1 (reactive-only) as a machine-checkable gate.
 *
 * WHY THIS EXISTS
 * ---------------
 * #827 (E8-S5) deletes the last SSE poll floors and the last two agent-thread
 * parks. Deleting the current offenders is not enough on its own — nothing
 * stops the NEXT engineer from reaching for `setInterval` the next time a
 * consumer "just wants to be safe". This scan makes that a build failure
 * instead of a silent regression: every recurring timer, self-rescheduling
 * wait, or thread-blocking primitive in the scanned source must appear in
 * `REACTIVE_ALLOWLIST` (scripts/reactive-allowlist.ts) with a reason naming
 * one of five classes (deadline / render-clock / external-protocol /
 * os-liveness / bounded-retry). An unlisted hit fails the gate.
 *
 * WHAT THIS GATE DOES (surface only — it never edits anything)
 * --------------------------------------------------------------------
 * 1. Scans `apps/<name>/src`, `packages/<name>/src`, and `packages/piing/extensions`
 *    for six primitives: `setInterval`, a self-rescheduling `setTimeout`
 *    (a setTimeout call whose callback body itself calls setTimeout —
 *    the "poll in disguise" shape #827 converts scheduleIdleResume out of),
 *    `Atomics.wait`, `Bun.sleepSync`, `spawnSync`, and a bare `sleep(` call
 *    (the legacy CLI's async-sleep helper).
 * 2. `scripts/*` and test files (`*.test.ts`, `test/**`, `tests/**`) are
 *    deliberately OUT OF SCAN SCOPE — CI's own pacing (retry loops, poll
 *    waits in test harnesses) is not the product's reactive surface, and
 *    scanning it would eventually tempt someone to "fix" CI timing by
 *    weakening this gate instead. This is intentional, not an oversight.
 * 3. Every hit must match a `REACTIVE_ALLOWLIST` entry by (file, primitive,
 *    exact trimmed source text) — #966/#967. An unmatched hit is UNTRIAGED —
 *    a brand-new poll landing unnoticed, OR a real site silently orphaned by
 *    a file move or a same-file sibling it never actually covered — and
 *    fails the gate.
 * 4. An allowlist entry pointing at a (file, primitive, match) triple with no
 *    matching hit is STALE — the code moved or was deleted and the register
 *    was not updated — and also fails the gate (same bidirectional
 *    discipline as scripts/test/sql-only-state.test.mjs and
 *    orphanable-spawner-lib.mjs): deleting an allowlist entry without
 *    deleting the code, or deleting the code without deleting the entry,
 *    both fail.
 * 5. Matching is BAG (multiset) semantics, not set: two real sites sharing
 *    byte-identical text in the same file each require their own allowlist
 *    entry (duplicate rows are legitimate and expected), the same discipline
 *    the deleted `apps/cli/test/BlockingAllowlist.ts` (#883) used — a set-based "has this
 *    text been blessed at all" check would let one entry silently cover an
 *    unreviewed duplicate.
 *
 * Exit non-zero (gate failure) when:
 *   - a scanned primitive hit has no matching allowlist entry (untriaged), or
 *   - an allowlist entry's reason does not name one of the five allowed
 *     classes, or
 *   - an allowlist entry points at a (file, primitive, match) triple with no
 *     matching hit in the current tree (stale).
 *
 * Usage:  bun run scripts/reactive-scan.ts [--json]
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

// `import.meta.dir` is a Bun-only extension -- it is undefined when this
// module is imported into a vitest run (packages/testing/test/ReactiveScan.test.ts),
// which executes under vitest's own (Node-compatible) module loader rather
// than `bun scripts/reactive-scan.ts` directly. `import.meta.url` is the
// portable equivalent both runtimes support.
const REPO_ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

/** Only the product's own reactive surface — CI pacing and scripts/tests are
 *  explicitly excluded (see module doc comment, point 2). */
const SCAN_GLOBS = ["apps", "packages"];
const EXCLUDE_SEGMENTS = [
  "node_modules",
  ".claude/worktrees",
  "target",
  ".git",
  "dist",
  "build",
  "scripts",
  "test",
  "tests",
];
const SCAN_EXTENSIONS = [".ts", ".tsx"];

/** This detector's own files embed the primitive names as strings/comments;
 *  they are not scan sites and must not scan themselves. */
const EXCLUDE_BASENAMES = new Set(["reactive-scan.ts", "reactive-allowlist.ts"]);

/** Directory segments that ARE in scan scope even though they sit under an
 *  otherwise-excluded top level: packages/piing/extensions is the product's
 *  copied-extension surface (piing framework, not test/script tooling). */
function inScanScope(relPath: string): boolean {
  const segments = relPath.split("/");
  if (!SCAN_GLOBS.includes(segments[0])) return false;
  // `apps/<name>/src/**` or `packages/<name>/src/**` or
  // `packages/piing/extensions/**` are the only in-scope trees.
  if (segments[0] === "apps" && segments[2] === "src") return true;
  if (segments[0] === "packages" && segments[1] === "piing" && segments[2] === "extensions") return true;
  if (segments[0] === "packages" && segments[2] === "src") return true;
  return false;
}

interface PrimitiveMarker {
  primitive: ReactivePrimitive;
  pattern: RegExp;
  what: string;
}

type ReactivePrimitive =
  | "setInterval"
  | "setTimeout-self-rescheduling"
  | "Atomics.wait"
  | "Bun.sleepSync"
  | "spawnSync"
  | "sleep";

const SIMPLE_MARKERS: PrimitiveMarker[] = [
  { primitive: "setInterval", pattern: /\bsetInterval\s*\(/, what: "a recurring timer" },
  { primitive: "Atomics.wait", pattern: /\bAtomics\.wait\s*\(/, what: "a thread-blocking wait" },
  { primitive: "Bun.sleepSync", pattern: /\bBun\.sleepSync\s*\(/, what: "a thread-blocking sleep" },
  { primitive: "spawnSync", pattern: /\bspawnSync\s*\(/, what: "a thread-blocking child-process spawn" },
  // A `sleep(` call, bare OR as a `.sleep(` seam method — the legacy CLI's
  // dominant shape is an injectable seam (`options.sleep ?? defaultSleep`,
  // called as `seams.sleep(...)`), not a bare identifier, so `.sleep(` must
  // match too or every real site in org-tmux.ts/chiefd-process.ts would be
  // invisible to this scan. `\b` still
  // excludes `sleepSync`/`oversleep`-style identifiers (no word boundary
  // before "sleep" inside them) without needing a dot-exclusion.
  { primitive: "sleep", pattern: /\bsleep\s*\(/, what: "an async sleep" },
];

interface FoundSite {
  file: string;
  line: number;
  primitive: ReactivePrimitive;
  what: string;
  text: string;
}

function isCommentLine(trimmed: string): boolean {
  return /^(\/\/|\*|\/\*|#)/.test(trimmed);
}

function walk(dir: string, out: string[]): void {
  let entries: string[];
  try {
    entries = readdirSync(dir);
  } catch {
    return;
  }
  for (const name of entries) {
    const full = join(dir, name);
    const rel = relative(REPO_ROOT, full);
    if (EXCLUDE_SEGMENTS.some((seg) => rel.split("/").includes(seg))) continue;
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    if (st.isDirectory()) walk(full, out);
    else if (SCAN_EXTENSIONS.some((ext) => name.endsWith(ext)) && !EXCLUDE_BASENAMES.has(name) && !name.endsWith(".test.ts")) {
      out.push(full);
    }
  }
}

/**
 * Find the substring of `content` spanning the full argument list of a call
 * whose opening `(` is at `openParenIndex` — i.e. everything between that
 * `(` and its matching `)` — using a paren-depth walk. Scoping by PARENS
 * (the call's own argument boundary) rather than by braces is deliberate:
 * `setTimeout(() => void doThing(id), delay)` has a concise-body callback
 * with NO braces at all, so a brace-first search would skip past this whole
 * call and latch onto the next unrelated `{ ... }` block in the file —
 * exactly the kind of cross-contamination a fixed-line-window heuristic also
 * risked in a file this large (organization-intercom.ts is 14k+ lines,
 * deeply nested, with many unrelated `setTimeout` calls near each other).
 * Bounding by the call's own parens is correct for both callback shapes.
 *
 * Deliberately heuristic (naive paren counting, not a real parser): a paren
 * inside a string/template literal/regex can throw the count off. Given this
 * scans a small, deliberately-scoped surface (extensions + apps/*\/src +
 * packages/*\/src) reviewed by a human allowlist either way, that residual
 * risk is accepted the same way it is in the sibling
 * `orphanable-spawner-lib.mjs`.
 */
function extractCallArguments(content: string, openParenIndex: number): string | undefined {
  if (content[openParenIndex] !== "(") return undefined;
  let depth = 0;
  for (let i = openParenIndex; i < content.length; i += 1) {
    const ch = content[i];
    if (ch === "(") depth += 1;
    else if (ch === ")") {
      depth -= 1;
      if (depth === 0) return content.slice(openParenIndex + 1, i);
    }
  }
  return undefined;
}

/**
 * Detect a self-rescheduling setTimeout: a `setTimeout(` call whose own
 * argument list — scoped by paren-depth, see {@link extractCallArguments},
 * not a line-count window or a brace search — itself contains another
 * `setTimeout(` call. This is the "poll in disguise" shape: a one-shot timer
 * that re-arms itself from inside its own callback, functionally a
 * `setInterval` wearing a disguise (the exact shape #827 step 7 converts
 * `scheduleIdleResume` out of, keeping only one bounded fallback attempt
 * that does NOT re-arm itself).
 */
/**
 * Does `setTimeout`'s first argument name a function that re-arms?
 *
 * Only a BARE IDENTIFIER counts: an inline arrow or function expression is
 * already covered by the nested-call test, and anything else (a member
 * expression, a call) is not a local definition this scan can resolve
 * honestly. Both declaration forms are matched — `function rearm(` and
 * `const rearm = ` — because a scan that knew only one of them would report
 * the other as clean.
 */
function namedCallbackRearms(content: string, callArgs: string): boolean {
  const firstArgument = callArgs.split(",")[0]?.trim() ?? "";
  if (!/^[A-Za-z_$][\w$]*$/.test(firstArgument)) return false;
  const declaration = new RegExp(
    `(?:function\\s+${firstArgument}\\s*\\(|(?:const|let|var)\\s+${firstArgument}\\s*=)`,
  );
  const at = content.search(declaration);
  if (at === -1) return false;
  // The declaration's own body, bounded by its brace balance — never a fixed
  // window, which would either miss a long function or swallow the next one.
  const bodyStart = content.indexOf("{", at);
  if (bodyStart === -1) return false;
  let depth = 0;
  for (let i = bodyStart; i < content.length; i += 1) {
    if (content[i] === "{") depth += 1;
    else if (content[i] === "}") {
      depth -= 1;
      if (depth === 0) {
        const body = content
          .slice(bodyStart, i + 1)
          .split("\n")
          .filter((line) => !isCommentLine(line.trim()))
          .join("\n");
        return /\bsetTimeout\s*\(/.test(body);
      }
    }
  }
  return false;
}

function findSelfReschedulingSetTimeout(content: string, file: string): FoundSite[] {
  const sites: FoundSite[] = [];
  const lineStarts: number[] = [0];
  for (let i = 0; i < content.length; i += 1) if (content[i] === "\n") lineStarts.push(i + 1);
  const lineOf = (charIndex: number): number => {
    let lo = 0;
    let hi = lineStarts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (lineStarts[mid]! <= charIndex) lo = mid; else hi = mid - 1;
    }
    return lo;
  };
  const setTimeoutCall = /\bsetTimeout\s*(\()/g;
  let match: RegExpExecArray | null;
  while ((match = setTimeoutCall.exec(content))) {
    const lineIndex = lineOf(match.index);
    const lineText = content.slice(lineStarts[lineIndex]!, content.indexOf("\n", match.index) === -1 ? content.length : content.indexOf("\n", match.index));
    if (isCommentLine(lineText.trim())) continue;
    const openParenIndex = match.index + match[0].length - 1;
    const callArgs = extractCallArguments(content, openParenIndex);
    if (callArgs === undefined) continue;
    // A nested setTimeout that appears only inside a `//` line comment or a
    // block comment within the arguments is documentation, not a
    // reschedule — strip comment lines before testing.
    const argsWithoutComments = callArgs
      .split("\n")
      .filter((l) => !isCommentLine(l.trim()))
      .join("\n");
    // R11: follow a NAMED callback one hop.
    //
    // `setTimeout(rearm, delay)` where `rearm` itself calls `setTimeout` is
    // the same poll-in-disguise as an inline nested one, and it was invisible
    // to a detector that only looked inside the argument list. One hop, in the
    // same file, is deliberately the whole depth: it catches the shape that
    // actually occurs without turning a text scan into a call-graph analysis
    // it cannot be trusted to do.
    const rearmsViaNamedCallback =
      !/\bsetTimeout\s*\(/.test(argsWithoutComments) &&
      namedCallbackRearms(content, argsWithoutComments);
    if (/\bsetTimeout\s*\(/.test(argsWithoutComments) || rearmsViaNamedCallback) {
      sites.push({
        file,
        line: lineIndex + 1,
        primitive: "setTimeout-self-rescheduling",
        what: "a setTimeout whose own callback re-arms another setTimeout — a poll in disguise",
        text: lineText.trim(),
      });
    }
  }
  return sites;
}

/** Enumerate every reactive-primitive hit under the in-scope scan roots. */
export function findReactiveSites(root = REPO_ROOT): FoundSite[] {
  const files: string[] = [];
  for (const d of SCAN_GLOBS) walk(join(root, d), files);
  const sites: FoundSite[] = [];
  for (const file of files) {
    const rel = relative(root, file);
    if (!inScanScope(rel)) continue;
    let content: string;
    try {
      content = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    const lines = content.split("\n");
    for (let i = 0; i < lines.length; i += 1) {
      const raw = lines[i];
      const trimmed = raw.trim();
      if (isCommentLine(trimmed)) continue;
      const line = raw.replace(/\/\/.*$/, "");
      for (const marker of SIMPLE_MARKERS) {
        if (!marker.pattern.test(line)) continue;
        sites.push({ file: rel, line: i + 1, primitive: marker.primitive, what: marker.what, text: trimmed });
      }
    }
    sites.push(...findSelfReschedulingSetTimeout(content, rel));
  }
  return sites;
}

const ALLOWED_CLASSES = ["deadline", "render-clock", "external-protocol", "os-liveness", "bounded-retry"];

import { REACTIVE_ALLOWLIST, type ReactiveAllowance } from "./reactive-allowlist";

export interface ReactiveScanResult {
  sites: FoundSite[];
  untriaged: FoundSite[];
  staleAllowlist: ReactiveAllowance[];
  unclassified: ReactiveAllowance[];
  ok: boolean;
}

/** #966/#967: (file, primitive, exact trimmed text) — the same content
 *  anchor `BlockingAllowlist.ts` and `sql-only-state.test.mjs` already use. */
function siteKey(entry: { file: string; primitive: string; match: string }): string {
  return `${entry.file}\x00${entry.primitive}\x00${entry.match}`;
}

/** Bag (multiset) comparison between real sites and allowlist entries, keyed
 *  by content rather than line number or bare (file, primitive) — see the
 *  module doc comment, points 3-5. Bag semantics (not set) so two real sites
 *  sharing byte-identical text in the same file each require their own
 *  allowlist entry; a set-based "has this text been blessed at all" check
 *  would let one entry silently cover an unreviewed duplicate. */
function compareSitesToAllowlist(
  sites: readonly FoundSite[],
  allowlist: readonly ReactiveAllowance[],
): { untriaged: FoundSite[]; stale: ReactiveAllowance[] } {
  const allowlistCounts = new Map<string, number>();
  for (const entry of allowlist) {
    const key = siteKey({ file: entry.file, primitive: entry.primitive, match: entry.match });
    allowlistCounts.set(key, (allowlistCounts.get(key) ?? 0) + 1);
  }
  const siteCounts = new Map<string, number>();
  for (const site of sites) {
    const key = siteKey({ file: site.file, primitive: site.primitive, match: site.text });
    siteCounts.set(key, (siteCounts.get(key) ?? 0) + 1);
  }

  const untriaged: FoundSite[] = [];
  const seenUntriaged = new Map<string, number>();
  for (const site of sites) {
    const key = siteKey({ file: site.file, primitive: site.primitive, match: site.text });
    const alreadyBlessed = seenUntriaged.get(key) ?? 0;
    const budget = allowlistCounts.get(key) ?? 0;
    if (alreadyBlessed >= budget) untriaged.push(site);
    seenUntriaged.set(key, alreadyBlessed + 1);
  }

  const stale: ReactiveAllowance[] = [];
  const seenStale = new Map<string, number>();
  for (const entry of allowlist) {
    const key = siteKey({ file: entry.file, primitive: entry.primitive, match: entry.match });
    const alreadyMatched = seenStale.get(key) ?? 0;
    const available = siteCounts.get(key) ?? 0;
    if (alreadyMatched >= available) stale.push(entry);
    seenStale.set(key, alreadyMatched + 1);
  }

  return { untriaged, stale };
}

export function scan(
  root = REPO_ROOT,
  opts: { allowlist?: readonly ReactiveAllowance[] } = {},
): ReactiveScanResult {
  const allowlist = opts.allowlist ?? REACTIVE_ALLOWLIST;
  const sites = findReactiveSites(root);

  const { untriaged, stale: staleAllowlist } = compareSitesToAllowlist(sites, allowlist);

  // An entry whose reason names none of the five allowed classes is a
  // register violation even if the site itself is real — "the register
  // ships closed" means every reason is legible as one of the five, not
  // just present.
  const unclassified = allowlist.filter(
    (e) => !ALLOWED_CLASSES.some((cls) => e.reason.toLowerCase().includes(cls)),
  );

  const ok = untriaged.length === 0 && staleAllowlist.length === 0 && unclassified.length === 0;

  return { sites, untriaged, staleAllowlist, unclassified, ok };
}

function main(): void {
  const asJson = process.argv.includes("--json");
  const result = scan(REPO_ROOT);

  if (asJson) {
    console.log(JSON.stringify(result, null, 2));
    process.exit(result.ok ? 0 : 1);
  }

  console.log(`reactive-scan: ${result.sites.length} reactive-primitive site(s) found, ${REACTIVE_ALLOWLIST.length} allowlist entries\n`);

  if (result.untriaged.length > 0) {
    console.log(`UNTRIAGED (${result.untriaged.length}) — a reactive primitive with no allowlist entry. FAIL:`);
    for (const s of result.untriaged) console.log(`  x ${s.file}:${s.line}  [${s.primitive}]  ${s.text}\n      ${s.what}`);
    console.log("");
  }

  if (result.staleAllowlist.length > 0) {
    console.log(`STALE ALLOWLIST (${result.staleAllowlist.length}) — entry points at a site that no longer exists. FAIL:`);
    for (const e of result.staleAllowlist) console.log(`  x ${e.file}  [${e.primitive}]  ${e.match}`);
    console.log("");
  }

  if (result.unclassified.length > 0) {
    console.log(`UNCLASSIFIED (${result.unclassified.length}) — reason names none of the five allowed classes. FAIL:`);
    for (const e of result.unclassified) console.log(`  x ${e.file}  [${e.primitive}]  reason: ${e.reason.slice(0, 80)}...`);
    console.log("");
  }

  console.log(result.ok ? "PASS" : "FAIL");
  process.exit(result.ok ? 0 : 1);
}

if (import.meta.main) main();
