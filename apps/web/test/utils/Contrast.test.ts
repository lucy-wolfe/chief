import { describe, expect, it } from 'vitest'

import {
  CHIP_INK_DARK,
  CHIP_INK_LIGHT,
  contrastingInk,
  contrastRatio,
  relativeLuminance
} from '@/utils/Contrast'

// The tmux styling constants this module ports (org-tmux.ts:616-638).
const CHIEF_THEME_TOKENS = {
  '--chief-status-bg': '#1a1a1a',
  '--chief-tab-inactive-bg': '#37373e',
  '--chief-index-inactive-bg': '#5a5a63',
  '--chief-index-active-bg': '#9daef8',
  '--chief-status-left-bg': '#d8d0c5',
  '--chief-pane-border': '#3a3a4a',
  '--chief-neutral-accent': '#8a8aaa'
} as const

describe('contrastingInk', () => {
  it('picks white on the deep-purple incident color (theme.ts doc comment)', () => {
    expect(contrastingInk('#5b1fa8')).toBe(CHIP_INK_LIGHT)
  })

  it('picks black on the pale status-left chip color', () => {
    expect(contrastingInk('#d8d0c5')).toBe(CHIP_INK_DARK)
  })

  it('meets normal-text contrast (>= 4.5:1) for every chief theme chip background', () => {
    for (const [token, background] of Object.entries(CHIEF_THEME_TOKENS)) {
      const ink = contrastingInk(background)
      const ratio = contrastRatio(background, ink)
      expect(
        ratio,
        `${token} (${background}) resolved ink ${ink} at ratio ${ratio}`
      ).toBeGreaterThanOrEqual(4.5)
    }
  })
})

describe('relativeLuminance / contrastRatio (port proof against theme.ts)', () => {
  it('matches the original luminance for pure white and pure black', () => {
    expect(relativeLuminance('#ffffff')).toBeCloseTo(1, 5)
    expect(relativeLuminance('#000000')).toBeCloseTo(0, 5)
  })

  it('matches the original contrast ratio between pure white and pure black (21:1)', () => {
    expect(contrastRatio('#ffffff', '#000000')).toBeCloseTo(21, 5)
  })

  it('is order-independent', () => {
    expect(contrastRatio('#5b1fa8', '#ffffff')).toBeCloseTo(contrastRatio('#ffffff', '#5b1fa8'), 10)
  })
})
