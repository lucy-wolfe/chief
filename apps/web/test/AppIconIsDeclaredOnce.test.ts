/**
 * The browser is offered exactly one app icon, and the file behind it exists.
 *
 * # The defect this exists to make impossible
 *
 * This app shipped with NO icon of any kind. Every page load asked for
 * `/favicon.ico` and took a 404, and nothing in the suite noticed — because
 * an icon is not an `/api` route, so `ClientPathsAreServed` cannot see it, and
 * no component renders it, so no render test can either. A missing favicon and
 * a working one are IDENTICAL in the code unless something reads for it.
 *
 * # What this can prove, and what it deliberately does not claim
 *
 * The icon is served by Next's `app/icon.*` metadata-file convention: the file
 * sitting at `src/app/icon.svg` is what makes Next serve `/icon.svg` and emit
 * the matching `<link rel="icon">` into every page's head. Vitest does not run
 * Next — the same argument `RouteRuntimeReach` makes for reading the import
 * graph instead of executing routes applies here — so this test does NOT
 * assert an HTTP status. It asserts the PRECONDITIONS the convention reads,
 * every one of which is a way the 404 comes back:
 *
 *   - the file is absent, or moved out of the one directory Next reads;
 *   - it is not actually an SVG, so the route serves something a browser will
 *     not paint;
 *   - a second declaration appears (`metadata.icons`, or a hand-written
 *     `<link rel="icon">`), and the two disagree about the path.
 *
 * That last one is the interesting failure. A hand-written tag pointing at a
 * path the convention does not serve reproduces the original bug EXACTLY while
 * the icon file sits in the tree looking fine — so "there is one declaration,
 * and it is the framework's" is the invariant, not "a file exists".
 *
 * # The README half
 *
 * The README used to render THIS FILE by relative path, and the assertion was
 * simply "the README's one `<img>` names `apps/web/src/app/icon.svg`". That
 * stopped being possible at the open-source launch, for a reason worth stating
 * rather than working around: a README hero has to be theme-aware. GitHub
 * renders the same page on a white and a near-black background, and a single
 * black-square-with-white-marks file is a black smear on one of them. So the
 * README now uses a `<picture>` with a light and a dark variant under
 * `docs/assets/`.
 *
 * That is genuinely two more copies of the mark, which is exactly the drift
 * this file exists to stop — so the invariant is restated rather than dropped.
 * It is no longer "one file"; it is **one MARK**: every variant must carry
 * byte-identical geometry (the `<path d="…">` data and the rounded-square
 * `rx`), and may differ only in fill colour. A variant redrawn by hand, or an
 * icon edited without its logos, fails here naming which file drifted.
 *
 * The app side is unchanged and unrelaxed: Next still serves ONE icon, from
 * its own convention, declared nowhere else.
 */
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = join(here, '..', 'src', 'app')
const repoRoot = join(here, '..', '..', '..')

/**
 * The icon files Next's metadata-file convention reads, derived from the app
 * directory rather than named.
 *
 * Named would defeat the point: the failure being guarded against is the file
 * not being where the convention looks, and a test that reads the path it
 * expects to find would simply move with it.
 */
function conventionIcons(): string[] {
  return readdirSync(appRoot)
    .filter((entry) => /^(icon|apple-icon|favicon)\b.*\.(svg|png|jpg|jpeg|ico)$/.test(entry))
    .sort()
}

/**
 * One source file with its comments removed.
 *
 * The scan below looks for the literal shapes of a second icon declaration,
 * and `layout.tsx` carries a comment explaining why it does NOT write one —
 * which quotes both shapes verbatim. Matching prose would make the guard fire
 * on the very file that documents the decision, so comments are stripped and
 * only code is read.
 */
function code(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1')
}

/** Every `.ts`/`.tsx` file under `src`, for the second-declaration scan. */
function sourceFiles(dir: string): string[] {
  const found: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) found.push(...sourceFiles(full))
    else if (/\.tsx?$/.test(entry.name)) found.push(full)
  }
  return found
}

/**
 * The parts of an SVG that ARE the mark: its path data and the corner radius
 * of the rounded square. Colour is deliberately excluded — that is the one
 * thing a theme variant is allowed to change.
 */
function geometry(source: string): { paths: string[]; rx: string } {
  return {
    paths: [...source.matchAll(/\sd="([^"]+)"/g)].map((match) => match[1].trim()),
    rx: /<rect[^>]*\srx="([^"]+)"/.exec(source)?.[1] ?? ''
  }
}

/**
 * Every `fill="…"` in document order. Two variants must use the same colours
 * in a different order.
 */
function fills(source: string): string[] {
  return [...source.matchAll(/\sfill="([^"]+)"/g)].map((match) => match[1])
}

describe('the app declares exactly one icon, by the framework convention', () => {
  it('has an icon where Next reads one', () => {
    // `icon.svg` specifically: an `apple-icon` may join it later, but an app
    // with no `icon.*` at all is the 404 this exists for.
    expect(conventionIcons()).toContain('icon.svg')
  })

  it('serves a real SVG, not a placeholder or an empty file', () => {
    const svg = readFileSync(join(appRoot, 'icon.svg'), 'utf8')

    expect(svg).toContain('<svg')
    expect(svg).toContain('</svg>')
    // A `viewBox` is what makes the mark scale to every size a browser asks
    // for. Without one an SVG favicon renders at whatever intrinsic size it
    // happens to carry, which for a tab icon is the difference between a mark
    // and a smudge.
    expect(svg).toMatch(/viewBox="[^"]+"/)
  })

  it('declares the icon NOWHERE else — the framework owns the path', () => {
    // Two declarations is how the original 404 comes back with the file
    // present: a hand-written tag or a `metadata.icons` entry naming a path
    // the convention does not serve looks completely fine in review.
    const offenders = sourceFiles(join(here, '..', 'src'))
      .map((file) => [relative(repoRoot, file), code(readFileSync(file, 'utf8'))] as const)
      .filter(
        ([, source]) =>
          /\bicons\s*:/.test(source) || /rel=["'{`]?\s*(?:"|')?(?:shortcut )?icon/.test(source)
      )
      .map(([path]) => path)

    expect(offenders).toEqual([])
  })

  it('renders a theme-aware pair, and both variants exist', () => {
    const readme = readFileSync(join(repoRoot, 'README.md'), 'utf8')
    // Both halves of a `<picture>`: the `<source srcset>` for dark and the
    // `<img src>` fallback for light. Reading only `<img>` would have let a
    // dark variant drift unread, which is the half nobody looks at.
    const references = [
      ...[...readme.matchAll(/<source[^>]*\bsrcset="([^"]+\.svg)"/g)].map((match) => match[1]),
      ...[...readme.matchAll(/<img[^>]*\bsrc="([^"]+\.svg)"/g)].map((match) => match[1])
    ].sort()

    // Exactly two, and exactly these two. A third SVG in the README would mean
    // a third copy of the mark, which is the drift this layout exists to stop.
    expect(references).toEqual(['docs/assets/logo-dark.svg', 'docs/assets/logo-light.svg'])
    for (const reference of references) {
      expect(existsSync(join(repoRoot, reference))).toBe(true)
    }
  })

  it("is ONE mark: every variant carries the icon's geometry, and differs only in colour", () => {
    const icon = readFileSync(join(appRoot, 'icon.svg'), 'utf8')
    const expected = geometry(icon)
    // Non-vacuity: a normaliser that extracted nothing would make every
    // comparison below trivially true, which is the shape of a guard that has
    // quietly stopped guarding.
    expect(expected.paths.length).toBeGreaterThan(0)
    expect(expected.rx).not.toBe('')

    for (const variant of ['docs/assets/logo-light.svg', 'docs/assets/logo-dark.svg']) {
      const source = readFileSync(join(repoRoot, variant), 'utf8')
      expect(geometry(source), `${variant} has drifted from apps/web/src/app/icon.svg`).toEqual(
        expected
      )
    }

    // And the two variants are inverses of each other rather than two copies of
    // the same picture — the whole reason there are two.
    const light = fills(readFileSync(join(repoRoot, 'docs/assets/logo-light.svg'), 'utf8'))
    const dark = fills(readFileSync(join(repoRoot, 'docs/assets/logo-dark.svg'), 'utf8'))
    expect(light).not.toEqual(dark)
    expect(new Set(light)).toEqual(new Set(dark))
  })
})
