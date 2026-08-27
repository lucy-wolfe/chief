// The exact control-flow shape tests/setup-conditional-preload.ts uses:
// sequential `await import(...)` in source order. This file exists so the
// ordering proof exercises the REAL mechanism (dynamic import awaited in
// sequence), not a description of it.
await import("./first.mjs");
await import("./second.mjs");
