import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { truncateToWidth } from "@earendil-works/pi-tui";
import { CHIEF_LOGO_LINES } from "@chief/piing/extension-runtime";

// #784 materializes this sanctioned package import as a sibling file before
// Pi loads the extension, so the welcome and standalone logo share one source.

/** Every extension-owned render line must fit Pi's supplied terminal width.
 * Pi treats an over-wide custom header as a fatal renderer exception, which
 * would otherwise terminate the CEO's real runtime pane on a narrow terminal. */
export function fitTribesWelcomeLine(line: string, width: number): string {
  const renderWidth = Number.isFinite(width) ? Math.max(0, Math.floor(width)) : 0;
  return truncateToWidth(line, renderWidth, "…");
}

export function installTribesWelcome(ctx: ExtensionContext) {
  // The logo/question belongs exclusively to the pre-company launcher. A
  // company CEO must start as the normal Pi identity, never as ChiefD.
  if (process.env.CHIEFD_LAUNCH_MODE === "company") return;
  const founder = process.env.CHIEFD_LAUNCH_MODE === "founder";
  ctx.ui.setHeader((_tui, theme) => ({
    render(width: number): string[] {
      return [
        "",
        ...CHIEF_LOGO_LINES.map((line) => theme.fg("text", line)),
        "",
        `  ${theme.fg("muted", founder ? "Founder" : "ChiefD")}`,
        `  ${theme.fg("muted", "What kind of company do you want to create today?")}`,
        "",
      ].map((line) => fitTribesWelcomeLine(line, width));
    },
    invalidate() {},
  }));
}

export default function tribesWelcome(pi: ExtensionAPI) {
  pi.on("session_start", (event, ctx) => {
    if (event.reason !== "startup" || !ctx.hasUI) return;
    installTribesWelcome(ctx);
  });
}
