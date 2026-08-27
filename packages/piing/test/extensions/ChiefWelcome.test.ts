import { CHIEF_LOGO_LINES } from '@chief/piing/extension-runtime'
import { visibleWidth } from '@earendil-works/pi-tui'
import { isNullish } from '@test/support/Nullish'
import chiefLogo from '@test-assets/chief-logo'
import tribesWelcome, { fitTribesWelcomeLine } from '@test-assets/tribes-welcome'
import { afterEach, expect, test } from 'vitest'

const APPROVED_CHIEF_LOGO_LINES = [
  '                ▗▖',
  '             ▄▄████▄▄',
  '          ▄▟██████████▙▄',
  '       ▗▟█████▛▀  ▀▜█████▙▖',
  '       ▐███▛▀        ▀▜███▛',
  '       ▝▀▀     ▄▟▙▄     ▝▀▘',
  '           ▗▄███████▙▄▖',
  '        ▗▄██████▀▀██████▄▖',
  '        ████▛▀▘    ▝▀███▀▘',
  '        ████',
  '        ████',
  '        ████',
  '        ████▄        ▄▄▖',
  '        ██████▙▄  ▄▟█████▖',
  '         ▝▀▜███████████▀▘',
  '             ▀▜████▛▀',
  '                ▀▘'
] as const

/* eslint-disable lucy/no-process-env */
// This suite drives `CHIEFD_LAUNCH_MODE`, the exact env var the extension
// under test branches on, so direct access is the test subject itself
// (both here and at the one other site below that sets it mid-test).
const originalMode = process.env.CHIEFD_LAUNCH_MODE
afterEach(() => {
  if (isNullish(originalMode)) delete process.env.CHIEFD_LAUNCH_MODE
  else process.env.CHIEFD_LAUNCH_MODE = originalMode
})

test('the Chief mark is the exact approved 27-column by 17-row quadrant-block raster', () => {
  expect(CHIEF_LOGO_LINES).toEqual(APPROVED_CHIEF_LOGO_LINES)
  expect(CHIEF_LOGO_LINES).toHaveLength(17)
  expect(Math.max(...CHIEF_LOGO_LINES.map((line) => visibleWidth(line)))).toBe(27)
  expect(CHIEF_LOGO_LINES[0]?.trim()).toBe('▗▖')
  expect(CHIEF_LOGO_LINES.slice(9, 12).every((line) => line.trim() === '████')).toBe(true)
})

/* eslint-disable @typescript-eslint/no-explicit-any */
// Pi's real `ExtensionAPI`/theme surface is a large third-party interface
// (dozens of required methods this suite never calls); implementing it in
// full for a unit-level event/render harness would bury the assertions in
// unrelated stub methods. These loosely-typed local handler maps mirror the
// pattern the original bun-based test runner's suite already used.
/* eslint-disable @typescript-eslint/no-non-null-assertion */
// Same handler-map seam as above: registered handlers are always present
// by construction in this suite's own fixture, just not provably to TS.
/* eslint-disable @typescript-eslint/consistent-type-assertions */
// Same seam: `{ on(...) {...} } as any` stubs Pi's untyped ExtensionAPI.

test('shows the ChiefD company-creation welcome on every interactive startup', () => {
  const handlers = new Map<string, (event: any, ctx: any) => unknown>()
  let headerFactory: ((tui: unknown, theme: any) => { render(width: number): string[] }) | undefined
  tribesWelcome({
    on(name: string, handler: (event: any, ctx: any) => unknown) {
      handlers.set(name, handler)
    }
  } as any)

  handlers.get('session_start')!(
    { reason: 'startup' },
    {
      hasUI: true,
      ui: {
        setHeader(factory: typeof headerFactory) {
          headerFactory = factory
        }
      }
    }
  )

  const header = headerFactory!({}, { fg: (_color: string, text: string) => text })
  expect(header.render(120)).toEqual([
    '',
    ...CHIEF_LOGO_LINES,
    '',
    '  ChiefD',
    '  What kind of company do you want to create today?',
    ''
  ])
})

test('does not install a header for non-startup or headless sessions', () => {
  const handlers = new Map<string, (event: any, ctx: any) => unknown>()
  let calls = 0
  tribesWelcome({
    on(name: string, handler: (event: any, ctx: any) => unknown) {
      handlers.set(name, handler)
    }
  } as any)
  const ctx = {
    hasUI: true,
    ui: {
      setHeader() {
        calls += 1
      }
    }
  }
  handlers.get('session_start')!({ reason: 'resume' }, ctx)
  handlers.get('session_start')!({ reason: 'startup' }, { ...ctx, hasUI: false })
  expect(calls).toBe(0)
})

test('does not install launcher welcome chrome in a company CEO pane', () => {
  process.env.CHIEFD_LAUNCH_MODE = 'company'
  const handlers = new Map<string, (event: any, ctx: any) => unknown>()
  let calls = 0
  tribesWelcome({
    on(name: string, handler: (event: any, ctx: any) => unknown) {
      handlers.set(name, handler)
    }
  } as any)
  handlers.get('session_start')!(
    { reason: 'startup' },
    {
      hasUI: true,
      ui: {
        setHeader() {
          calls += 1
        }
      }
    }
  )
  expect(calls).toBe(0)
})

test('clips the 27-column mark and welcome copy to a narrower terminal', () => {
  const handlers = new Map<string, (event: any, ctx: any) => unknown>()
  let headerFactory: ((tui: unknown, theme: any) => { render(width: number): string[] }) | undefined
  tribesWelcome({
    on(name: string, handler: (event: any, ctx: any) => unknown) {
      handlers.set(name, handler)
    }
  } as any)
  handlers.get('session_start')!(
    { reason: 'startup' },
    {
      hasUI: true,
      ui: {
        setHeader(factory: typeof headerFactory) {
          headerFactory = factory
        }
      }
    }
  )

  const width = 20
  const lines = headerFactory!({}, { fg: (_color: string, text: string) => text }).render(width)
  expect(lines.every((line) => visibleWidth(line) <= width)).toBe(true)
  expect(lines.slice(1, 1 + CHIEF_LOGO_LINES.length)).toEqual(
    CHIEF_LOGO_LINES.map((line) => fitTribesWelcomeLine(line, width))
  )
  expect(lines).toContain(
    fitTribesWelcomeLine('  What kind of company do you want to create today?', width)
  )
})

// The launcher extension (`tribes-welcome`) and the materialized Chief header
// extension (`chief-logo`) cannot import one another, so pre-#784 they each
// carried an independently-copied logo constant that could drift. #784
// replaced both copies with one canonical import from
// `@chief/piing/extension-runtime`; this drives BOTH extensions' real render
// paths and proves their rendered logo lines stay byte-identical to each
// other and to the canonical source, rather than merely importing the same
// reference twice.
test('both welcome paths render the canonical mark through active Light and Dark text foregrounds', () => {
  const welcomeHandlers = new Map<string, (event: any, ctx: any) => unknown>()
  let welcomeHeaderFactory:
    ((tui: unknown, theme: any) => { render(width: number): string[] }) | undefined
  tribesWelcome({
    on(name: string, handler: (event: any, ctx: any) => unknown) {
      welcomeHandlers.set(name, handler)
    }
  } as any)
  welcomeHandlers.get('session_start')!(
    { reason: 'startup' },
    {
      hasUI: true,
      ui: {
        setHeader(factory: typeof welcomeHeaderFactory) {
          welcomeHeaderFactory = factory
        }
      }
    }
  )
  const logoHandlers = new Map<string, (event: any, ctx: any) => unknown>()
  let logoHeaderFactory:
    ((tui: unknown, theme: any) => { render(width: number): string[] }) | undefined
  chiefLogo({
    on(name: string, handler: (event: any, ctx: any) => unknown) {
      logoHandlers.set(name, handler)
    }
  } as any)
  logoHandlers.get('session_start')!(
    { reason: 'startup' },
    {
      hasUI: true,
      ui: {
        setHeader(factory: typeof logoHeaderFactory) {
          logoHeaderFactory = factory
        }
      }
    }
  )
  for (const mode of ['light', 'dark'] as const) {
    const foreground = (color: string, text: string): string => `${mode}:${color}:${text}`
    const expected = CHIEF_LOGO_LINES.map((line) => `${mode}:text:${line}`)
    const welcomeLogo = welcomeHeaderFactory!({}, { fg: foreground })
      .render(120)
      .slice(1, 1 + CHIEF_LOGO_LINES.length)
    const standaloneHeader = logoHeaderFactory!({}, { fg: foreground }).render(120)
    const standaloneLogo = standaloneHeader.slice(1, 1 + CHIEF_LOGO_LINES.length)

    expect(welcomeLogo).toEqual(expected)
    expect(standaloneLogo).toEqual(expected)
    expect(standaloneHeader[CHIEF_LOGO_LINES.length + 2]).toBe(
      `  ${mode}:muted:welcome to tribes capital`
    )
  }
})
/* eslint-enable @typescript-eslint/no-explicit-any */
/* eslint-enable @typescript-eslint/no-non-null-assertion */
/* eslint-enable @typescript-eslint/consistent-type-assertions */
/* eslint-enable lucy/no-process-env */
