/**
 * chief-logo extension — renders the canonical Chief mark as the Pi header on
 * every session start. It stays self-contained: no harness alias, auth, or
 * startup side effect beyond the themed header.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import { truncateToWidth } from "@earendil-works/pi-tui"
import { CHIEF_LOGO_LINES } from "@chief/piing/extension-runtime"

function fitHeaderLine(line: string, width: number): string {
  const renderWidth = Number.isFinite(width) ? Math.max(0, Math.floor(width)) : 0
  return truncateToWidth(line, renderWidth, "…")
}

export default function chiefLogo(pi: ExtensionAPI): void {
  pi.on("session_start", async (event, ctx) => {
    if (event.reason !== "startup") return
    if (!ctx.hasUI) return
    ctx.ui.setHeader((_tui, theme) => ({
      render(width: number): string[] {
        const logo = CHIEF_LOGO_LINES.map((line) => theme.fg("text", line))
        const subtitle = `  ${theme.fg("muted", "welcome to tribes capital")}`
        return ["", ...logo, "", subtitle, ""].map((line) => fitHeaderLine(line, width))
      },
      invalidate() {}
    }))
  })
}
