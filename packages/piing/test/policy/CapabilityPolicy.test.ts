import { describe, expect, it } from 'vitest'

import { BUILTIN_TOOLS } from '@/policy/CapabilityPolicy'

describe('tool-set constants (argv contract — must not drift silently)', () => {
  it('BUILTIN_TOOLS', () => {
    expect(BUILTIN_TOOLS).toEqual(['read', 'bash', 'edit', 'write', 'grep', 'find', 'ls'])
  })
})

/* TOMBSTONE (chief-home-is-cwd §3/§4e): the `isForbiddenLauncherResource`
 * describe. The guard kept chief's own implementation skills and extensions
 * out of a person's materialized home; nothing is materialized and no person
 * selects a resource, so the function and its cases go together. */
