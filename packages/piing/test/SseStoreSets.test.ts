import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

// E4-S8 (#794) Contract: "SSE subscriptions keep their exact store sets ...
// and the onReorg -> full-resync wiring is preserved verbatim." Verified
// (issue text): intercom `[sseMailboxStore, ...ORGANIZATION_SSE_MAINTENANCE_STORES]`
// where `ORGANIZATION_SSE_MAINTENANCE_STORES = ["session-maintenance"]`;
// team-ui `["supervision", "activity", "mailbox/${identity.role}"]` -- the
// memory-review and learned-skills stores went with those features. A third
// subscriber, the outbound messaging-channel extension, was covered here
// until that channel was deleted outright; its store set went with it,
// leaving these two.
//
// A behavioral (constructed-subscription) assertion would need each
// extension's full install closure (tmux/chiefd/Pi context) live -- these
// two files' own installer test suites already exercise that. This is the
// standing regression on the literal store-set SHAPE and the onReorg wiring
// surviving the E4-S8 transport swap, source-verified the same
// strip-then-match way `ExtensionTransportFences.test.ts` does.

const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url))

function read(relativePath: string): string {
  return readFileSync(join(PACKAGE_ROOT, relativePath), 'utf8')
}

describe('SseStoreSets (E4-S8): exact store sets and onReorg -> full-resync wiring survive the transport swap', () => {
  test('team-ui.ts subscribes the three footer stores with onReorg driving a full refresh', () => {
    const source = read('extensions/team-ui.ts')
    expect(source).toContain('stores: ["supervision", "activity", `mailbox/${identity.role}`],')
    expect(source).toContain('onReorg: () => { refreshAndRender(); },')
    expect(source).toContain('from "@chief/chiefing/extension-runtime";')
  })

  test('organization-intercom.ts subscribes [sseMailboxStore, ...ORGANIZATION_SSE_MAINTENANCE_STORES]', () => {
    const source = read('extensions/organization-intercom.ts')
    expect(source).toContain('stores: [sseMailboxStore, ...ORGANIZATION_SSE_MAINTENANCE_STORES],')
    expect(source).toContain(
      'export const ORGANIZATION_SSE_MAINTENANCE_STORES = ["session-maintenance", "supervision"] as const;'
    )
    expect(source).toContain('from "@chief/chiefing/extension-runtime";')
  })

  test('neither of the two still imports the deleted ./org-sse-watcher sibling', () => {
    for (const file of ['extensions/team-ui.ts', 'extensions/organization-intercom.ts']) {
      const source = read(file)
      expect(source).not.toMatch(/from\s+["']\.\/org-sse-watcher["']/)
    }
  })

  test('org-sse-rollout.ts (the poll-only kill switch) is deleted (#827), not converted to chiefing', () => {
    // E4-S8's own scope note said "this story does not touch it" -- #827 is
    // the story that does: D0 (no configurable poll-only mode) deletes the
    // whole module rather than converting it, since there is no floor left
    // anywhere in the tree for a kill switch to gate. This asserts the
    // deletion, not a conversion -- `sseEnabled`/`ORG_SSE_POLL_FLOOR_MS`/
    // `ORG_SSE_DISABLED` have no replacement anywhere in packages/piing.
    expect(() => read('extensions/org-sse-rollout.ts')).toThrow()
    for (const file of ['extensions/team-ui.ts', 'extensions/organization-intercom.ts']) {
      const source = read(file)
      // Import/usage, not bare mention: team-ui.ts's own doc comment
      // legitimately explains FOOTER_STALE_AFTER_MS's history by naming the
      // deleted constant in backticks -- that is documentation, not a
      // surviving import or reference, so these checks target the import
      // statement and actual code usage rather than any string occurrence.
      expect(source).not.toMatch(/from\s+["']\.\/org-sse-rollout["']/)
      expect(source).not.toContain('sseEnabled(')
      expect(source).not.toMatch(/\{[^}]*ORG_SSE_POLL_FLOOR_MS[^}]*\}\s*from/)
      expect(source).not.toContain('ORG_SSE_POLL_FLOOR_MS =')
      expect(source).not.toMatch(/\{[^}]*ORG_SSE_DISABLED[^}]*\}\s*from/)
      expect(source).not.toContain('ORG_SSE_DISABLED =')
    }
  })
})
