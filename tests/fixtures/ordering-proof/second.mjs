// Mirrors setup-durable-store.ts's shape: a module whose mere evaluation
// is the thing the FIRST module (setup-workspace-build-preflight.ts) must
// run before, so its own worse error never fires first.
import { recordOrderingEvent } from "./order-log.mjs";
recordOrderingEvent("second");
