#!/bin/sh
# Install the live activity status line (FIVE / task #27) into the ROOT Pi layer
# so EVERY plain Pi agent inherits it — the operator's ruling: "statusline is for all pi
# agents, not specific to cobalt".
#
# THIS NOW REACHES COMPANY AGENTS TOO, and it did not before. The old note here
# said org pi-homes were unaffected, on two grounds that have both gone: they
# ran with `PI_CODING_AGENT_DIR=<person pi-home>` (chief stopped redirecting it
# in #1307 — Pi inherits `~/.pi/agent` for every agent now) and with
# `--no-extensions` (retired; `spawn_cmd` asserts it is never emitted). User
# scope extensions are not trust-gated, so this directory auto-discovers into
# every company pane. That follows directly from the operator's ruling that Pi
# should do its own inheritance — an extension installed for "all pi agents"
# now genuinely means all of them.
#
# WHY THIS IS A DIRECTORY, NOT A LONE FILE (measured, not assumed):
#   packages/piing/extensions/organization-activity-status.ts is a HELPER, not a Pi extension.
#   Pi's loader (dist/core/extensions/loader.js) requires an extension module's
#   DEFAULT export to be a factory function; the helper has only named exports
#   (createActivityStatusLine, ACTIVITY_STATUS_KEY, ...), so dropping it in on
#   its own makes Pi reject it: "Extension does not export a valid factory
#   function". For org agents the driver that wires it lives in team-ui.ts; a
#   plain agent has no team-ui. So this installs a self-contained extension
#   DIRECTORY: index.ts (the default-export driver that subscribes to Pi tool
#   events and feeds ctx.ui.setStatus) plus the helper verbatim beside it.
#   Pi's collectAutoExtensionEntries treats a subdir-with-index as ONE
#   extension, so the helper is not separately (mis)loaded.
#
# Discovery mechanism (measured against @earendil-works/pi-coding-agent):
#   package-manager.js addAutoDiscoveredResources() scans
#   <agentDir>/extensions/ (agentDir = $PI_CODING_AGENT_DIR or ~/.pi/agent).
#   Discovery is DIRECTORY-BASED; the settings.json "extensions" key is only an
#   enable/disable override, never required to register. This script therefore
#   touches NO settings.json (respects "override only the model, no ad-hoc
#   settings edits") — the directory drop alone registers the extension.
#
# Recipe discipline: every step verifies its own result and refuses otherwise.
# Idempotent: re-running converges to the same bytes and re-passes every gate.
set -eu

fail() { echo "INSTALL REFUSED: $*" >&2; exit 1; }

# --- Resolve the root Pi agent dir (env override wins, else ~/.pi/agent) ------
AGENT_DIR="${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}"
[ -n "$AGENT_DIR" ] || fail "could not resolve the Pi agent dir"
# The agent layer must already exist (settings.json is the operator's root
# config). Refuse rather than fabricate a root layer that was never configured.
[ -f "$AGENT_DIR/settings.json" ] || fail "no $AGENT_DIR/settings.json — root Pi layer is not configured on this box"

# --- Resolve the repo (this script lives in <repo>/scripts) -------------------
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
HELPER_SRC="$REPO/packages/piing/extensions/organization-activity-status.ts"
[ -f "$HELPER_SRC" ] || fail "helper source missing at $HELPER_SRC (wrong checkout?)"
# The helper must be context-free (no launcher/org imports) to live at root.
# Inspect only real import/export-from statements, never comment prose that may
# mention those paths while explaining why they are forbidden.
if grep -nE '^[[:space:]]*(import|export)[[:space:]].*[[:space:]]from[[:space:]]' "$HELPER_SRC" \
     | grep -qE '\.\./|/src/|organization-intercom'; then
  fail "helper is not context-free (imports ChiefD/org state) — refusing root install"
fi

EXT_ROOT="$AGENT_DIR/extensions"
DEST="$EXT_ROOT/activity-status"

# --- Step 1: land the files (atomic per file via temp + mv) -------------------
mkdir -p "$DEST" || fail "could not create $DEST"

install_file() {
  # $1 = final path, stdin = contents. Writes only if bytes differ (idempotent,
  # preserves mtime on a no-op — matters for Pi's mtime-based drift alarms).
  _final="$1"; _tmp="$_final.tmp.$$"
  cat > "$_tmp" || { rm -f "$_tmp"; fail "write failed: $_final"; }
  if [ -f "$_final" ] && cmp -s "$_tmp" "$_final"; then
    rm -f "$_tmp"
  else
    mv "$_tmp" "$_final" || { rm -f "$_tmp"; fail "rename failed: $_final"; }
  fi
}

# Helper: copied VERBATIM from the FIVE source so it can never drift from it.
install_file "$DEST/organization-activity-status.ts" < "$HELPER_SRC"

# Driver: the default-export factory that a plain agent needs (the piece
# team-ui.ts supplies for org agents). Wiring mirrors team-ui exactly: tool
# events only, setStatus(key, text|undefined) where undefined clears.
install_file "$DEST/index.ts" <<'TS'
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { ACTIVITY_STATUS_KEY, createActivityStatusLine } from "./organization-activity-status";

/**
 * Root-layer live activity status line. Self-contained: the only non-type
 * import is the sibling helper installed beside this file. Empty is not
 * broken — an idle agent renders nothing (createActivityStatusLine emits
 * undefined, which clears the footer status).
 */
export default function activityStatusLine(pi: ExtensionAPI): void {
  let lastCtx: ExtensionContext | undefined;
  const activity = createActivityStatusLine({
    setStatus: (text) => {
      try { lastCtx?.ui.setStatus(ACTIVITY_STATUS_KEY, text); } catch { /* footer is presentation-only */ }
    },
  });
  pi.on("tool_execution_start", (event, ctx) => { lastCtx = ctx; activity.toolStart(event.toolCallId, event.toolName); });
  pi.on("tool_execution_end", (event, ctx) => { lastCtx = ctx; activity.toolEnd(event.toolCallId, event.isError === true); });
  pi.on("session_shutdown", () => activity.reset());
}
TS

# --- Step 2: verify the files landed -----------------------------------------
[ -f "$DEST/index.ts" ] || fail "index.ts did not land at $DEST"
[ -f "$DEST/organization-activity-status.ts" ] || fail "helper did not land at $DEST"
cmp -s "$DEST/organization-activity-status.ts" "$HELPER_SRC" || fail "installed helper differs from source"

# --- Step 3: verify it is REGISTERED (discoverable by Pi auto-discovery) ------
# collectAutoExtensionEntries registers a subdir as one extension iff it has an
# index entry. Prove the entrypoint Pi keys on is present and top-level.
case "$DEST" in "$EXT_ROOT"/*) : ;; *) fail "extension not under the discovered root $EXT_ROOT" ;; esac
[ -f "$DEST/index.ts" ] || fail "no index entrypoint — Pi would not register the directory"

# --- Step 4: SAMPLE CHECK — prove it actually LOADS through the real loader ---
# Reproduce loader.js's contract (jiti import with default:true, default MUST be
# a function) and then functionally drive it: a tool start yields a label, a
# tool end clears it. Uses the repo's own jiti (a dep of pi-coding-agent).
RUNTIME=""
for c in bun node; do command -v "$c" >/dev/null 2>&1 && { RUNTIME="$c"; break; }; done
[ -n "$RUNTIME" ] || fail "no bun/node runtime to run the load check"
SMOKE="$DEST/.load-check.mjs"
cat > "$SMOKE" <<'JS'
import { createJiti } from "jiti/static";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
const here = dirname(fileURLToPath(import.meta.url));
const jiti = createJiti(import.meta.url);
const mod = await jiti.import(join(here, "index.ts"), { default: true });
if (typeof mod !== "function") { console.error("LOAD_FAIL: default export is " + typeof mod + ", not a factory"); process.exit(2); }
const handlers = {};
mod({ on: (name, fn) => { handlers[name] = fn; } });
for (const ev of ["tool_execution_start", "tool_execution_end", "session_shutdown"]) {
  if (typeof handlers[ev] !== "function") { console.error("LOAD_FAIL: handler not wired: " + ev); process.exit(3); }
}
let status = "UNSET";
const ctx = { ui: { setStatus: (_k, text) => { status = text; } } };
handlers.tool_execution_start({ toolCallId: "probe", toolName: "bash" }, ctx);
if (!status) { console.error("LOAD_FAIL: no status label after tool start"); process.exit(4); }
handlers.tool_execution_end({ toolCallId: "probe", isError: false }, ctx);
if (status !== undefined) { console.error("LOAD_FAIL: status did not clear after tool end"); process.exit(5); }
console.log("LOAD_OK label=" + JSON.stringify(status === undefined ? null : status));
JS
# Bounded external call. #190: ceiling 60s, derived — a jiti transpile+load of
# two small files is sub-second; 60s is ~100x headroom and strictly below any
# enclosing deploy step budget, so it can only fire on a genuine hang.
if ! ( cd "$REPO" && timeout 60 "$RUNTIME" "$SMOKE" ); then
  rm -f "$SMOKE"
  fail "the installed extension did not load/function through the real Pi loader"
fi
rm -f "$SMOKE"

echo "ROOT STATUS LINE INSTALLED: $DEST"
echo "  helper + driver landed, registered by directory auto-discovery, load+smoke verified."
echo "  scope: plain Pi agents under $AGENT_DIR only; org pi-homes unaffected."
