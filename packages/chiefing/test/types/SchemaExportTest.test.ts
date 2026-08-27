import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const schemaPath = join(here, '..', '..', 'generated', 'chiefd-request-schemas.json')

interface SchemaExportFile {
  $comment: string
  ops: Record<string, unknown>
}

function isSchemaExportFile(value: unknown): value is SchemaExportFile {
  return (
    !!value &&
    typeof value === 'object' &&
    '$comment' in value &&
    typeof value.$comment === 'string' &&
    'ops' in value &&
    typeof value.ops === 'object' &&
    !!value.ops
  )
}

function readSchemaExport(): SchemaExportFile {
  const parsed: unknown = JSON.parse(readFileSync(schemaPath, 'utf8'))
  if (!isSchemaExportFile(parsed)) {
    throw new Error('generated/chiefd-request-schemas.json does not have the {$comment, ops} shape')
  }
  return parsed
}

describe('generated/chiefd-request-schemas.json', () => {
  it('parses as JSON', () => {
    expect(() => JSON.parse(readFileSync(schemaPath, 'utf8'))).not.toThrow()
  })

  it('has the top-level {$comment, ops} shape', () => {
    const parsed = readSchemaExport()
    expect(typeof parsed.$comment).toBe('string')
    expect(typeof parsed.ops).toBe('object')
  })

  // The floor moved 70 -> 69 in #751-P4, when `activity.reflect` stopped being
  // a registered wire operation (releasing a transition has no payload and no
  // agent-facing verb, so chiefd reaches it internally). It moved 69 -> 68
  // when the `@koltmcbride/pi-loop` addon was deleted and `loops.stop` — the
  // executive-only verb that rewrote pi-loop's own session loops file — went
  // with it; durable reminders replace the mechanism entirely. It moved
  // 68 -> 66 when the outbound messaging-channel subsystem was deleted
  // outright and its two wire verbs (poll and send) went with it; see the
  // CHANGELOG entry for that removal. It moved 66 -> 55 when the task,
  // memory, learned-skills and acknowledgement subsystems were deleted
  // outright, taking their ops with them, and then 55 -> 50 once the export
  // was REGENERATED: 55 had been measured against a HAND-EDITED copy that
  // still declared `assignment.assign` and four other deleted ops, so the
  // count described a wire surface that no longer existed. 50 is what the
  // derive actually emits. This is a
  // TRUNCATION guard, not an inventory: it exists so a half-written or
  // clipped export fails loudly. Lower it only alongside a deliberate,
  // reviewed removal of an operation — never to make a red go away.
  // 50 -> 48 on 2026-08-13: `org.loan` and `org.return` were deleted with the
  // loan concept by operator ruling. That is exactly the "deliberate, reviewed
  // removal of an operation" the paragraph above allows for, and not a red
  // being made to go away — the export really does emit two fewer ops.
  // 48 -> 45 on 2026-08-16: provider/model management is deleted outright
  //, taking the model-change and
  // provider-models ops with it. Same allowance, same evidence: the
  // REGENERATED export really does emit three fewer ops.
  // Lowered 45 -> 44 when the daemon-side CEO boot was deleted: `company.ceo`
  // left the wire with the route it named. The floor tracks a surface that
  // genuinely shrank; an op disappearing WITHOUT this number moving is still
  // the truncated-copy failure this test exists to catch.
  // 44 -> 43 on 2026-08-24: `maint.complete_native` left the wire registry with
  // the `/v1/org/session-maintenance/complete-native` route, deleted with
  // `org_maintain_session` by operator ruling. Same allowance, same evidence as
  // the four lowerings above: the export really does emit one fewer op.
  it('has at least 43 ops — a truncated copy fails loudly', () => {
    const parsed = readSchemaExport()
    expect(Object.keys(parsed.ops).length).toBeGreaterThanOrEqual(43)
  })

  // #835 (E9-S3): the three tests above prove the FILE's outer shape (it
  // parses, has {$comment, ops}, and roughly the right op count) but never
  // inspect any individual operation's schema — a file with 72 ops that
  // are all `{}` would still pass every assertion above. These prove real
  // per-operation depth exists, spot-checked against operations this
  // package's own resource clients actually call — a truncated or
  // corrupted individual entry (not just a truncated file) fails loudly.
  it('a real operation this package calls (org.hire) has a well-formed JSON-schema object with required fields', () => {
    const parsed = readSchemaExport()
    const hire = parsed.ops['org.hire']
    expect(hire).toBeDefined()
    expect(hire).toMatchObject({ type: 'object' })
    if (!hire || typeof hire !== 'object') throw new Error('expected an object schema')
    expect('properties' in hire).toBe(true)
    expect('required' in hire).toBe(true)
  })

  it('every op schema is a non-empty JSON-schema object, not a stub {} entry', () => {
    // A truncated/corrupted export run producing well-formed-but-empty
    // entries would still satisfy "parses as JSON" and "has ≥50 ops" —
    // this is the check that would catch that specific failure mode.
    const parsed = readSchemaExport()
    const emptyOps = Object.entries(parsed.ops)
      .filter(
        ([, schema]) => schema && typeof schema === 'object' && Object.keys(schema).length === 0
      )
      .map(([name]) => name)
    expect(emptyOps).toEqual([])
  })

  it("op names are dotted namespace.verb identifiers (e.g. company.boot), matching this package's own route-family naming", () => {
    const parsed = readSchemaExport()
    const malformed = Object.keys(parsed.ops).filter((name) => !/^[a-z]+(\.[a-z_]+)+$/.test(name))
    expect(malformed).toEqual([])
  })
})
