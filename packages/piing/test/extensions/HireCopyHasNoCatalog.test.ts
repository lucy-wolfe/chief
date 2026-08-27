import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

describe('org_hire model-facing copy', () => {
  test('does not tell a caller to select deleted per-person resources', () => {
    const source = readFileSync(
      fileURLToPath(new URL('../../extensions/organization-intercom.ts', import.meta.url)),
      'utf8'
    )
    const hire = source.slice(
      source.indexOf('name: "org_hire"'),
      source.indexOf('name: "org_hire"') + 2500
    )

    expect(hire).toContain('a hire does not select skills, extensions, or packages')
    expect(hire).not.toContain('installed resource catalog')
    expect(hire).not.toContain('select only exact ids')
  })
})
