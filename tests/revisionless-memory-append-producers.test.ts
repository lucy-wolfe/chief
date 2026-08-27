import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

function typescriptFunctionSource(source: string, name: string): string {
  const match = new RegExp(`(?:export\\s+)?(?:async\\s+)?function\\s+${name}\\s*\\(`).exec(source);
  const start = match?.index ?? -1;
  if (start < 0) throw new Error(`missing function ${name}`);
  const parenStart = source.indexOf("(", start);
  let parenDepth = 0;
  let afterParams = -1;
  for (let index = parenStart; index < source.length; index += 1) {
    if (source[index] === "(") parenDepth += 1;
    else if (source[index] === ")") {
      parenDepth -= 1;
      if (parenDepth === 0) {
        afterParams = index + 1;
        break;
      }
    }
  }
  // A returned object type (`): { ... } {`) has a brace before the body.
  // Prefer that type-to-body boundary; functions without an object return type
  // fall back to their first post-parameter brace.
  const typeBoundary = /\}\s*\{/.exec(source.slice(afterParams));
  const bodyStart = typeBoundary
    ? afterParams + typeBoundary.index + typeBoundary[0].lastIndexOf("{")
    : source.indexOf("{", afterParams);
  if (bodyStart < 0) throw new Error(`missing body for ${name}`);
  let depth = 0;
  for (let index = bodyStart; index < source.length; index += 1) {
    const character = source[index];
    if (character === "{") depth += 1;
    if (character === "}") depth -= 1;
    if (depth === 0) return source.slice(start, index + 1);
  }
  throw new Error(`unclosed body for ${name}`);
}

function rustFunctionSource(source: string, name: string): string {
  const start = source.indexOf(`async fn ${name}(`);
  if (start < 0) throw new Error(`missing Rust function ${name}`);
  const bodyStart = source.indexOf("{", start);
  if (bodyStart < 0) throw new Error(`missing body for ${name}`);
  let depth = 0;
  for (let index = bodyStart; index < source.length; index += 1) {
    const character = source[index];
    if (character === "{") depth += 1;
    if (character === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(start, index + 1);
    }
  }
  throw new Error(`unclosed body for ${name}`);
}

describe("revisionless memory append producers", () => {
  test("live TypeScript and worker appenders call the atomic command without a document CAS", () => {
    const root = join(import.meta.dir, "..");
    const memoryExtension = readFileSync(join(root, "packages", "piing", "extensions", "organization-memory.ts"), "utf8");
    const intercomExtension = readFileSync(join(root, "packages", "piing", "extensions", "organization-intercom.ts"), "utf8");
    const worker = readFileSync(join(root, "apps", "chiefd", "crates", "chiefd", "src", "memory_worker.rs"), "utf8");

    for (const source of [
      typescriptFunctionSource(memoryExtension, "recordMemoryRecord"),
      typescriptFunctionSource(intercomExtension, "appendMemoryRecord"),
    ]) {
      expect(source).toContain('"/v1/org/memory/append"');
      expect(source).not.toContain("/v1/org/memory/publish");
      expect(source).not.toContain("expectedSeq");
      expect(source).not.toContain("mutateDurableDocument");
    }

    const workerAppend = rustFunctionSource(worker, "append_memory_record");
    expect(workerAppend).toContain(".memory_append(");
    expect(workerAppend).not.toContain(".memory_read(");
    expect(workerAppend).not.toContain(".memory_publish(");
    expect(workerAppend).not.toContain("append_record(");
  });
});
