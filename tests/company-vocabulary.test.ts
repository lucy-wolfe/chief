import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";

const publicGuides = [
  "README.md",
  "docs/WHAT_IS_A_COMPANY.md",
  "docs/ARCHITECTURE.md",
  "docs/ORGANIZATION_ARCHITECTURE.md",
  "packages/piing/skills/organization-launcher/SKILL.md",
] as const;

test("uses company as the sole human-facing root-organization vocabulary", () => {
  expect(existsSync("docs/WHAT_IS_A_COMPANY.md")).toBeTrue();
  expect(existsSync("docs/WHAT_IS_A_TRIBE.md")).toBeFalse();

  for (const path of publicGuides) {
    const text = readFileSync(path, "utf8");
    expect(text).not.toMatch(/\bbun(?: run)? tribe\b/);
    expect(text).not.toContain("WHAT_IS_A_TRIBE");
  }

  const readme = readFileSync("README.md", "utf8");
  expect(readme).toContain("bun run company -- boot <company>");
  expect(readme).toContain("Internal expert organization control plane");
});
