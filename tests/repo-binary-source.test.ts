import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

/**
 * No tracked SOURCE file may contain a NUL byte.
 *
 * Twice in one session, two different agents embedded a RAW 0x00 byte in a
 * template literal as a field separator (`${a}\x00${b}` typed as the actual
 * control character instead of the four-character escape). The runtime string
 * is identical either way — but the raw byte makes git and grep classify the
 * FILE as binary, which silently exempts it from every grep-based check in
 * the repository INCLUDING the conflict-marker gate (which skips
 * binary-looking files by NUL scan, per this repo's own measurement trap
 * that byte-oriented tools go silent on them rather than erroring).
 *
 * A source file that tooling silently skips is a hole in every assurance
 * built on that tooling: the second occurrence (org-memory-worker.ts) sat
 * invisible through multiple repo-wide audits and merge hazard-greps. The
 * catalogue entry written after the first occurrence did not prevent the
 * second — only a detector does. Fix pattern: write the ESCAPE (`\x00`),
 * never the raw byte; same string, and the file stays text.
 */
const SOURCE_SUFFIXES = [".ts", ".tsx", ".rs", ".md", ".sh", ".json", ".toml", ".yml", ".yaml"];

test("no tracked source file is grep-invisible (contains a raw NUL byte)", () => {
  const listed = spawnSync("git", ["ls-files"], { encoding: "utf8" });
  expect(listed.status).toBe(0);
  const files = listed.stdout.split("\n").filter(
    (f) => f.length > 0 && SOURCE_SUFFIXES.some((s) => f.endsWith(s)),
  );
  expect(files.length).toBeGreaterThan(500); // the scan must actually be scanning the repo
  const offenders: string[] = [];
  for (const file of files) {
    let bytes: Buffer;
    try {
      bytes = readFileSync(file);
    } catch {
      continue; // deleted-but-listed transients are not this gate's business
    }
    const at = bytes.indexOf(0);
    if (at !== -1) offenders.push(`${file} (first NUL at byte ${at})`);
  }
  expect(
    offenders,
    `raw NUL byte(s) in tracked source — the file is binary to git/grep and silently exempt ` +
      `from every grep-based gate. Replace the raw byte with the \\x00 ESCAPE (identical ` +
      `runtime string):\n${offenders.join("\n")}`,
  ).toEqual([]);
});
