/**
 * SSE-E (#265) acceptance criterion 2's grep gate: "no 600ms (or any
 * sub-second) docstore poll loop remains anywhere in the repo
 * (grep-verifiable); the 60s floor exists and is suppressed while healthy."
 *
 * LIVE AND ENFORCED as of `main`@`da4caa4` (#262/#263/#264 all landed,
 * `#262`'s intercom poll and `#264`'s outbound-channel drain both converted
 * away from a sub-second interval): this was `describe.skip`'d while it was
 * correctly false (see the git history on this file/`DECISIONS.md` for that
 * period's reasoning — writing it hard-failing before the clients converted
 * would have committed a known-red test to `main`), then flipped live by
 * deleting the `.skip` the moment both `KNOWN_PENDING_SITES` entries then
 * listed were confirmed gone (one of the two has since been deleted outright
 * along with the extension it named, leaving the intercom entry below) — exactly the "one-line flip" this file was built to
 * make possible, and it was verified GREEN on the very same rebase, not
 * forced green.
 *
 * Two independent checks, mirroring `repo-binary-source.test.ts`'s
 * `git ls-files` + read-and-assert grep-gate style:
 *
 * 1. Each currently-known offending literal, named explicitly, must be
 *    ABSENT from its file. Named rather than swept generically because
 *    intercom's actual `setInterval` call site uses a variable
 *    (`pollIntervalMs`), not a literal `600` — a purely generic "grep for
 *    `setInterval(fn, <number>)`" sweep cannot see it; only checking for the
 *    literal `600` default assignment itself can.
 * 2. A generic repo-wide sweep for any OTHER `setInterval(fn, <literal
 *    number>)` call under 1000ms, catching new/unknown offenders a named
 *    check can't anticipate — with a small, explicit exemption list for
 *    sub-second intervals that are NOT docstore polling (a bounded
 *    process-identity sampler, currently) so this gate stays honest about
 *    what it can and can't see, same limitation `repo-binary-source.test.ts` accepts for its
 *    own grep-based check.
 */

import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

/** Verified present as of this pass (2026-07-23) — see the module doc for
 * why each is named explicitly rather than caught by the generic sweep. */
const KNOWN_PENDING_SITES = [
  {
    ticket: "#262 (SSE-C2, intercom)",
    file: "packages/piing/extensions/organization-intercom.ts",
    mustNotContain: "pollIntervalMs === undefined ? 600",
  },
] as const;

/** Sub-second `setInterval` calls that are NOT docstore/mailbox polling and
 * must never trip the generic sweep below. Exempted by (file, a snippet of
 * the exact call), so a genuinely NEW sub-second poll added later next to
 * an exempt one still gets caught. */
const EXEMPT_NON_POLL_INTERVALS = [
  {
    // #695's bounded in-flight child-process identity sampler: it reads only
    // the harness's temporary pid files, retains exact owned argv tuples, and
    // is stopped before the cleanup safety net runs. It never polls docstore
    // or mailbox state.
    file: "tests/e2e/harness/chiefd-process-cleanup.ts",
    pattern: /setInterval\(sample, 10\)/,
  },
] as const;

function trackedTsFiles(): string[] {
  const listed = spawnSync("git", ["ls-files"], { encoding: "utf8" });
  expect(listed.status).toBe(0);
  return listed.stdout.split("\n").filter((f) => f.length > 0 && f.endsWith(".ts") && !f.endsWith(".test.ts"));
}

function isExemptNonPollInterval(file: string, snippet: string): boolean {
  return EXEMPT_NON_POLL_INTERVALS.some((entry) => entry.file === file && entry.pattern.test(snippet));
}

function subSecondIntervalOffenders(file: string, text: string): string[] {
  const offenders: string[] = [];
  const intervalCallPattern = /setInterval\([^,]*,\s*(\d+)\s*\)/g;
  for (const match of text.matchAll(intervalCallPattern)) {
    const ms = Number(match[1]);
    if (!Number.isFinite(ms) || ms >= 1000) continue;
    const snippet = match[0];
    if (!isExemptNonPollInterval(file, snippet)) offenders.push(`${file}: ${snippet}`);
  }
  return offenders;
}

describe("SSE-E (#265) AC-2: no sub-second docstore/mailbox poll loop remains anywhere", () => {
  test("none of the currently-known offending literals remain in their file", () => {
    const stillPresent: string[] = [];
    for (const site of KNOWN_PENDING_SITES) {
      const text = readFileSync(site.file, "utf8");
      if (text.includes(site.mustNotContain)) {
        stillPresent.push(`${site.file} (${site.ticket}): still contains "${site.mustNotContain}"`);
      }
    }
    expect(stillPresent, `convert these before this gate can pass:\n${stillPresent.join("\n")}`).toEqual([]);
  });

  test("no OTHER sub-second setInterval literal appears anywhere in tracked source", () => {
    const files = trackedTsFiles();
    expect(files.length).toBeGreaterThan(100); // the scan must actually be scanning the repo

    const offenders: string[] = [];
    for (const file of files) {
      let text: string;
      try {
        text = readFileSync(file, "utf8");
      } catch {
        continue;
      }
      offenders.push(...subSecondIntervalOffenders(file, text));
    }
    expect(
      offenders,
      `sub-second setInterval outside EXEMPT_NON_POLL_INTERVALS — either convert it to SSE ` +
        `or add an exemption entry if it's genuinely not docstore-related:\n` +
        offenders.join("\n"),
    ).toEqual([]);
  });

  test("the exact bounded ChiefD process-identity sampler exemption is live", () => {
    const file = "tests/e2e/harness/chiefd-process-cleanup.ts";
    const source = readFileSync(file, "utf8");
    expect(isExemptNonPollInterval(file, "setInterval(sample, 10)")).toBe(true);
    expect(subSecondIntervalOffenders(file, source)).toEqual([]);
  });

  test("a nearby new sub-second interval in the sampler file is still detected", () => {
    const file = "tests/e2e/harness/chiefd-process-cleanup.ts";
    const source = readFileSync(file, "utf8");
    const nearby = source.replace("setInterval(sample, 10)", "setInterval(sample, 11)");
    expect(nearby).not.toBe(source);
    expect(subSecondIntervalOffenders(file, nearby)).toEqual([
      `${file}: setInterval(sample, 11)`,
    ]);
  });
});
