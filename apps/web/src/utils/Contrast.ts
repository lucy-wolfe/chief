/**
 * Ported, byte-for-byte, from `src/foundation/theme.ts:5-56` (chief's tmux
 * launcher — pre-E1-move location; `apps/cli/src/legacy/foundation/theme.ts`
 * after `E4-S1-cli-package-move`). Pure WCAG contrast math, no I/O.
 */

function rgb(hex: string): number[] {
  return [1, 3, 5].map((index) => Number.parseInt(hex.slice(index, index + 2), 16))
}

function linearChannel(channel: number): number {
  const value = channel / 255
  return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4
}

/** WCAG relative luminance of a #rrggbb color. */
export function relativeLuminance(hexColor: string): number {
  const [r, g, b] = rgb(hexColor)
  return (
    0.2126 * linearChannel(r ?? 0) + 0.7152 * linearChannel(g ?? 0) + 0.0722 * linearChannel(b ?? 0)
  )
}

/** WCAG contrast ratio between two #rrggbb colors, order-independent. */
export function contrastRatio(a: string, b: string): number {
  const first = relativeLuminance(a)
  const second = relativeLuminance(b)
  return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05)
}

/** The light ink a title chip uses when its background is dark. */
export const CHIP_INK_LIGHT = '#ffffff'
/** The dark ink a title chip uses when its background is light.
 *
 * Pure black is intentional: tmux's pane-title chip is a large, solid
 * background and must meet normal-text contrast without changing a person's
 * raw identity accent. A softer near-black left the light/dark-safe accent
 * band below 4.5:1 even though black clears it comfortably. */
export const CHIP_INK_DARK = '#000000'

/**
 * Pick a title chip's foreground FROM ITS OWN BACKGROUND, by WCAG contrast
 * ratio (never a naive brightness average, and never a hardcoded colour).
 *
 * A chip whose background is a per-person accent cannot carry a fixed ink: a
 * deep accent (the reported `#5b1fa8` purple) drew the dark ink on a dark
 * field and was unreadable, while a pale accent needs exactly that dark ink.
 * The rule is the same one in both directions — take whichever of the two inks
 * has the higher ratio against the background — so the fix cannot regress the
 * light-accent half the way "just use white" would.
 */
export function contrastingInk(background: string): string {
  return contrastRatio(background, CHIP_INK_LIGHT) >= contrastRatio(background, CHIP_INK_DARK)
    ? CHIP_INK_LIGHT
    : CHIP_INK_DARK
}
