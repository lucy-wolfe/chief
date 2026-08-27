// Mirrors setup-workspace-build-preflight.ts's shape: records that it ran,
// then optionally throws SYNCHRONOUSLY during module evaluation (the same
// point the real preflight's Bun.resolveSync check throws at) when
// ORDERING_PROOF_FIRST_THROWS is set -- the failure-path scenario.
import { recordOrderingEvent } from "./order-log.mjs";
recordOrderingEvent("first");
if (process.env.ORDERING_PROOF_FIRST_THROWS) {
  throw new Error("first.mjs: simulated preflight failure");
}
