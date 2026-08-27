/**
 * The organization accent ALLOCATOR: the one place a person's identity colour
 * is derived. It is a pure in-memory derivation over the roster — a curated
 * palette, a hue-rotating wrap once the roster outgrows it, and the
 * identity-stable ordering the allocation is keyed on. Nothing here reads or
 * writes a file.
 *
 * Theme documents do not live in this TypeScript allocator. The Rust
 * create-once worker-home writer consumes this same derived accent and writes
 * Pi-native Light and Dark theme files. It skips the Chief structurally. This
 * allocator still answers for every roster person, including the Chief,
 * because rails and roster identity need the color even when Pi text stays
 * neutral.
 */

function rgb(hexColor: string): number[] {
  return [1, 3, 5].map((index) => Number.parseInt(hexColor.slice(index, index + 2), 16))
}

function hex(values: readonly number[]): string {
  return `#${values.map((value) => value.toString(16).padStart(2, '0')).join('')}`
}

/**
 * High-contrast accents assigned in organization roster order. The roster is
 * append-only for durable identities, so a person's color remains stable
 * across materialization and runtime reconciliation. A compact,
 * Google/Material-derived ten-family palette, each rebalanced to luminance
 * ~0.202. The raw value is the stable identity input; the worker theme derives
 * separate readable Light and Dark foregrounds from it.
 */
const ORGANIZATION_PERSON_ACCENTS = [
  '#e24033', // red
  '#c75e00', // orange
  '#a27400', // amber
  '#2c8e46', // green
  '#00899a', // teal
  '#3c7adf', // blue
  '#6977c5', // indigo
  '#a74ef5', // purple
  '#d83d98', // magenta
  '#c05e68' // rose
] as const

/**
 * Degrees of hue rotation applied per wrap cycle once the roster outgrows the
 * curated palette. 37 is not a divisor of 360, so repeated application walks
 * the wheel instead of landing back on the base hue.
 */
const ACCENT_WRAP_HUE_STEP_DEGREES = 37
const RAW_ACCENT_LUMINANCE = 0.202

/** How many rotations are attempted before the allocator gives up loudly. */
const ACCENT_WRAP_MAX_ATTEMPTS = 360

function rgbToHsl(hexColor: string): [number, number, number] {
  const normalized = rgb(hexColor).map((channel) => channel / 255)
  const r = normalized[0] ?? 0
  const g = normalized[1] ?? 0
  const b = normalized[2] ?? 0
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  const lightness = (max + min) / 2
  if (max === min) return [0, 0, lightness]
  const delta = max - min
  const saturation = lightness > 0.5 ? delta / (2 - max - min) : delta / (max + min)
  let hue: number
  if (max === r) hue = ((g - b) / delta + (g < b ? 6 : 0)) / 6
  else if (max === g) hue = ((b - r) / delta + 2) / 6
  else hue = ((r - g) / delta + 4) / 6
  return [hue * 360, saturation, lightness]
}

function hslToHex(hue: number, saturation: number, lightness: number): string {
  const h = (((hue % 360) + 360) % 360) / 360
  if (saturation === 0) {
    const value = Math.round(lightness * 255)
    return hex([value, value, value])
  }
  const q =
    lightness < 0.5 ? lightness * (1 + saturation) : lightness + saturation - lightness * saturation
  const p = 2 * lightness - q
  const channel = (offset: number): number => {
    let t = h + offset
    if (t < 0) t += 1
    if (t > 1) t -= 1
    if (t < 1 / 6) return p + (q - p) * 6 * t
    if (t < 1 / 2) return q
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6
    return p
  }
  return hex([channel(1 / 3), channel(0), channel(-1 / 3)].map((value) => Math.round(value * 255)))
}

/** Rotate a color's hue before the allocator restores relative luminance. */
function rotateHue(hexColor: string, degrees: number): string {
  const [hue, saturation, lightness] = rgbToHsl(hexColor)
  return hslToHex(hue + degrees, saturation, lightness)
}

function relativeLuminance(hexColor: string): number {
  const [r = 0, g = 0, b = 0] = rgb(hexColor).map((channel) => {
    const value = channel / 255
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * r + 0.7152 * g + 0.0722 * b
}

function colorWithRelativeLuminance(hexColor: string, target: number): string {
  const [hue, saturation] = rgbToHsl(hexColor)
  let low = 0
  let high = 1
  for (let iteration = 0; iteration < 32; iteration += 1) {
    const middle = (low + high) / 2
    if (relativeLuminance(hslToHex(hue, saturation, middle)) < target) low = middle
    else high = middle
  }
  const darker = hslToHex(hue, saturation, low)
  const lighter = hslToHex(hue, saturation, high)
  return Math.abs(relativeLuminance(darker) - target) <=
    Math.abs(relativeLuminance(lighter) - target)
    ? darker
    : lighter
}

/**
 * Every person's identity accent, in roster order — the ONE place accents
 * are allocated, so a caller cannot accidentally reimplement the wrap.
 * Beyond the curated palette, each wrap cycle rotates the base hue by
 * {@link ACCENT_WRAP_HUE_STEP_DEGREES}, and a candidate that still
 * duplicates an already-allocated accent keeps rotating. If uniqueness
 * cannot be reached the allocator THROWS — it never returns a duplicate.
 */
function organizationPersonAccents(peopleOrder: readonly string[]): string[] {
  const allocated: string[] = []
  const taken = new Set<string>()
  for (const [index] of peopleOrder.entries()) {
    const base =
      ORGANIZATION_PERSON_ACCENTS[index % ORGANIZATION_PERSON_ACCENTS.length] ??
      ORGANIZATION_PERSON_ACCENTS[0]
    const cycle = Math.floor(index / ORGANIZATION_PERSON_ACCENTS.length)
    let candidate =
      cycle === 0
        ? base
        : colorWithRelativeLuminance(
            rotateHue(base, cycle * ACCENT_WRAP_HUE_STEP_DEGREES),
            RAW_ACCENT_LUMINANCE
          )
    for (
      let attempt = 1;
      taken.has(candidate) && attempt <= ACCENT_WRAP_MAX_ATTEMPTS;
      attempt += 1
    ) {
      candidate = colorWithRelativeLuminance(
        rotateHue(base, cycle * ACCENT_WRAP_HUE_STEP_DEGREES + attempt),
        RAW_ACCENT_LUMINANCE
      )
    }
    if (taken.has(candidate)) {
      throw new Error(
        `Cannot allocate a distinct organization accent for roster position ${index} ` +
          `('${peopleOrder[index]}'): the palette and its hue rotations are exhausted. ` +
          `Refusing to hand two people the same identity color.`
      )
    }
    taken.add(candidate)
    allocated.push(candidate)
  }
  return allocated
}

export function organizationPersonAccent(peopleOrder: readonly string[], personId: string): string {
  const index = peopleOrder.indexOf(personId)
  if (index < 0)
    throw new Error(`Cannot allocate an organization accent for unknown person '${personId}'`)
  const accent = organizationPersonAccents(peopleOrder)[index]
  if (typeof accent === 'undefined') {
    throw new Error(`Cannot allocate an organization accent for unknown person '${personId}'`)
  }
  return accent
}

/**
 * The identity-stable ordering the accent allocator must be fed: a person's
 * accent is allocated by POSITION in the order given, so this orders by
 * `createdAt` (persisted once at registration, `id` as a deterministic
 * tiebreak) rather than any roster ordering that could re-sort on hire/
 * transfer — a new hire always sorts LAST and takes the next free slot
 * without moving anyone already allocated.
 */
export function identityAccentOrder(people: Record<string, { createdAt: string }>): string[] {
  return Object.keys(people).sort((left, right) => {
    const byCreated = String(people[left]?.createdAt ?? '').localeCompare(
      String(people[right]?.createdAt ?? '')
    )
    return byCreated !== 0 ? byCreated : left.localeCompare(right)
  })
}
