// Shared side-effect recorder for the sequential-dynamic-import ordering
// proof (#962). A file, not an in-memory array: the harness spawns a fresh
// process per scenario, so process-local state cannot carry the record
// back to the assertion.
import { appendFileSync } from "node:fs";

const LOG_PATH = process.env.ORDERING_PROOF_LOG_PATH;

export function recordOrderingEvent(name) {
  if (!LOG_PATH) throw new Error("ORDERING_PROOF_LOG_PATH not set");
  appendFileSync(LOG_PATH, `${name}\n`);
}
