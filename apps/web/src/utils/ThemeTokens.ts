import type { ChiefThemeToken } from '@/types/Theme'

/**
 * The concrete hex value behind every `--chief-*` custom property declared
 * in `src/app/globals.css`. `contrastingInk` (WCAG math, `utils/Contrast.ts`)
 * needs a real hex, not a CSS custom property string, and CSS variables
 * aren't synchronously readable from JS without a DOM-measurement effect —
 * so this is the ONE place that mirrors globals.css's literal values. Every
 * component reads a hex from here rather than re-declaring its own, so the
 * unavoidable JS-side duplication stays in a single, obviously-paired file
 * instead of scattered across the kit (the acceptance grep for
 * `apps/web/src/components` bans a second hex literal per token — this
 * module is where that one literal lives).
 */
export const CHIEF_THEME_TOKEN_HEX: Readonly<Record<ChiefThemeToken, string>> = {
  '--chief-status-bg': '#1a1a1a',
  '--chief-tab-inactive-bg': '#37373e',
  '--chief-index-inactive-bg': '#5a5a63',
  '--chief-index-active-bg': '#9daef8',
  '--chief-status-left-bg': '#d8d0c5',
  '--chief-pane-border': '#3a3a4a',
  '--chief-neutral-accent': '#8a8aaa'
}
