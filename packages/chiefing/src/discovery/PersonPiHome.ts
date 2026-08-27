// Relative specifiers and `node:path` only — this module is reachable from the
// `extension-runtime` closure, which is copied FLAT into every pi-home with no
// `node_modules` and no tsconfig paths mapping.
import { join } from 'node:path'

/**
 * One agent's Pi agent directory inside a company directory:
 * `<dir>/.chief/agent/<personId>`.
 *
 * The Chief is not an agent and must not be passed here. The Chief runs the
 * operator's own Pi in `<dir>` and keeps only its company credential directly
 * under `<dir>/.chief`.
 *
 * # Why this is a function and not a join at each call site
 *
 * It was a join at each call site, twice, and both were a `.chief` short when
 * the company moved into the directory the operator stands in. A pane is
 * stamped with `ORG_LAUNCHER_ORG_DIR = <dir>` EXACTLY
 * (`chiefd-host/src/converge_apply/cycle.rs`), while materialization writes
 * under `config.data_root()` — `<dir>/.chief` — so every reader that joins
 * `people/` straight onto the stamped value looks one level too shallow and
 * finds nothing. Neither reader FAILS on that: `paneTokenManager` treats an
 * unreadable key as "this pane has no credential" and calls token-less, and
 * `currentReloadHardContract` treats an unreadable contract as "no contract".
 * Both are documented benign states, so the whole product degraded in silence —
 * under an enforced gate, every org tool call from every pane is refused with
 * `missing bearer token`.
 *
 * Must stay byte-identical to the Rust derivation
 * (`chiefd-host/src/converge_apply/cycle.rs::launch_entry`, which joins
 * `agent`/`<id>` onto `ActuatorConfig::data_root`), and
 * `PersonPiHomeCrossLanguage.test.ts` is what makes that a check rather than a
 * comment.
 */
export function personPiHome(companyDir: string, personId: string): string {
  return join(companyDir, '.chief', 'agent', personId)
}
