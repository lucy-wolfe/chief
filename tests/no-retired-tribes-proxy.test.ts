import { expect, test } from "bun:test";

test("the retired Tribes proxy is absent from every tracked repository artifact", () => {
  const retiredProvider = "tribes-" + "llm-proxy";
  const result = Bun.spawnSync(["git", "grep", "-in", retiredProvider], { cwd: import.meta.dir + "/..", stdout: "pipe", stderr: "pipe" });
  expect(result.exitCode).toBe(1);
});
