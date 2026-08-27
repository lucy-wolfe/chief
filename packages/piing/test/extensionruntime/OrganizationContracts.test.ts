import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import {
  ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
  ORGANIZATION_MANAGER_TOOL_NAMES,
  ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES,
  ORGANIZATION_SUBTREE_TOOL_NAMES,
  organizationForegroundResponsivenessContract
} from '@/extensionruntime/index'

const EXTENSIONS_ROOT = fileURLToPath(new URL('../../extensions', import.meta.url))

describe('promoted organization extension-runtime contracts', () => {
  it('pins the runtime tool inventories', () => {
    expect(ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES).toEqual([
      'org_send',
      'org_roster',
      'org_create_reminder',
      'org_list_reminders',
      'org_stop_reminder'
    ])
    // ROLE-gated: EMPTY, and pinned empty on purpose. Every tool that was here
    // is fenced server-side now, so nothing in this product is granted or
    // withheld by what a person IS. A name reappearing here is a role gate
    // coming back, and this assertion is what says so.
    expect(ORGANIZATION_MANAGER_TOOL_NAMES).toEqual([])
    // SCOPE-gated: the handler checks `departmentIsInScope`, which is empty for
    // a leaf, so each of these refuses today and succeeds the moment that leaf
    // heads a unit of its own. Pinned as its own inventory rather than folded
    // into the count above, because the split IS the product change: the two
    // lists differ in what the handler enforces, and a name in the wrong one is
    // either an unreachable mandate or an over-grant.
    expect(ORGANIZATION_SUBTREE_TOOL_NAMES).toEqual([
      'org_launch_department',
      'org_stop_department',
      'org_remove_department',
      'org_launch_contract',
      'org_stop_contract',
      'org_remove_contract',
      'org_add_department',
      'org_pause_department',
      'org_resume_department',
      'org_resume_departments',
      'org_hire',
      'org_bench',
      'org_recall',
      'org_start_person',
      'org_stop_person',
      'org_transfer',
      'org_offboard',
      'org_reparent_department',
      'org_move_department_members',
      'org_appoint_department_head',
      'org_lifecycle_status'
    ])
    // And the split lost nobody: the two lists together are exactly the
    // twenty-one names that used to sit in one (thirty before the goal
    // feature took its five manager tools, twenty-five before the loan
    // concept took `org_loan` and `org_return`, twenty-three before
    // `org_set_thinking` went with the rest of the model business,
    // twenty-two before `org_maintain_session` went with the REST of it —
    // the operator's 2026-08-24 ruling removed the tool and all three of its
    // actions). A tool that fell out of both would be a tool nobody can reach
    // and nothing here would otherwise notice.
    expect([...ORGANIZATION_MANAGER_TOOL_NAMES, ...ORGANIZATION_SUBTREE_TOOL_NAMES].length).toBe(21)
    // The CEO's operator-facing verbs, and the only list whose subject is the
    // COMPANY. `org_stand_down`/`org_resume` joined the escalation after a live
    // company was told to stop all work and could not: the CEO obeyed, parked
    // six people, and every one of them was back forty-five seconds later. No
    // number of `org_stop_person` calls expresses "the company stops working",
    // so the verb that does has to exist and has to be reachable from the one
    // person the operator talks to.
    expect(ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES).toEqual([
      'org_escalate_to_operator',
      'org_stand_down',
      'org_resume'
    ])
  })

  it('preserves the four-minute foreground responsiveness guidance', () => {
    expect(organizationForegroundResponsivenessContract()).toContain(
      'managed Bash receives a 4-minute maximum'
    )
    expect(organizationForegroundResponsivenessContract()).toContain(
      'Arm a durable reminder with `org_create_reminder` for future work'
    )
  })

  it('keeps package extensions bound to the shared authority', () => {
    const expectedImports = new Map([
      ['organization-runtime-policy.ts', ['organizationForegroundResponsivenessContract']],
      [
        'organization-intercom.ts',
        [
          'ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES',
          'ORGANIZATION_MANAGER_TOOL_NAMES',
          'ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES'
        ]
      ]
    ])

    for (const [file, symbols] of expectedImports) {
      const source = readFileSync(`${EXTENSIONS_ROOT}/${file}`, 'utf8')
      expect(source).toContain('from "@chief/piing/extension-runtime"')
      for (const symbol of symbols) {
        expect(source).toContain(symbol)
        const declarationPatterns = [
          new RegExp(`^\\s*(?:export\\s+)?const\\s+${symbol}\\s*=`, 'm'),
          new RegExp(`^\\s*(?:export\\s+)?function\\s+${symbol}\\s*\\(`, 'm'),
          new RegExp(`^\\s*(?:export\\s+)?interface\\s+${symbol}\\s*\\{`, 'm'),
          new RegExp(`^\\s*(?:export\\s+)?type\\s+${symbol}\\s*=`, 'm')
        ]
        for (const pattern of declarationPatterns) expect(source).not.toMatch(pattern)
      }
    }
  })

  it('keeps the extension registration subsets equal to the promoted active inventory', () => {
    const source = readFileSync(`${EXTENSIONS_ROOT}/organization-intercom.ts`, 'utf8')
    const namesIn = (declaration: string): string[] => {
      const block = source.match(
        new RegExp(`export const ${declaration} = \\[([\\s\\S]*?)\\] as const`)
      )
      expect(block).not.toBeNull()
      return [...(block?.[1] ?? '').matchAll(/"([^"]+)"/g)].map((match) => match[1])
    }

    expect(namesIn('ORGANIZATION_BASELINE_TOOL_NAMES')).toEqual(
      ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES
    )
  })
})
