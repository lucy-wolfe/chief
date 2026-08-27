/**
 * THE MANAGER DELEGATES. IT DOES NOT DO THE WORK.
 *
 * The operator's report, repeated over months: *"you send an issue to a
 * department and then the manager is doing all the work and not even waking up
 * his subordinates. The manager is a delegator, that's the idea."*
 *
 * The mechanism was never friction. `org_send` IS the wake — a message to a
 * settled person grants their launch intent and brings their pane up
 * (`converge_apply/cycle.rs`: "A message to a settled person is the whole
 * mechanism that brings them back") — so delegating costs exactly one call and
 * needs nobody to be up first. The manager holds `org_send` and `org_roster`
 * like everybody else.
 *
 * The mechanism was that the one surface a manager reads AT THE MOMENT WORK
 * ARRIVES said the opposite. `messageContext` rendered a single byte-identical
 * guidance for every recipient of every kind — "Reply only with a needed
 * result, precise blocker, or necessary question" — which is a WORKER
 * instruction, delivered in the current turn, while the delegation duty sat
 * thousands of tokens back in an `AGENTS.md` read once at boot.
 *
 * This repo already wrote the doctrine, for the last defect of the same shape
 * (`ReportsToNamesTheParent.test.ts`): *"A skill that explains it beautifully
 * while the argument beside the cursor says nothing is a fix that never
 * fires."*
 *
 * So this pins the copy WHERE THE MANAGER DECIDES: the delivered envelope, and
 * `org_send`'s own description. Both branches are asserted, because a fix that
 * gave every recipient the manager text would be the same defect mirrored.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { CapturedTool, ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import {
  mailboxBatchContextForTest,
  messageContextForTest
} from '@test-assets/organization-intercom'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const MANAGER_SKILL = readFileSync(
  fileURLToPath(new URL('../skills/manager/SKILL.md', import.meta.url)),
  'utf8'
)
const WORKER_SKILL = readFileSync(
  fileURLToPath(new URL('../skills/worker/SKILL.md', import.meta.url)),
  'utf8'
)

const ENVELOPE = {
  schemaVersion: 1 as const,
  id: 'msg-1',
  organization: 'northstar',
  fromPersonId: 'vera',
  to: 'ada',
  recipients: ['ada'],
  body: 'The checkout page 500s for logged-out users. Please get it fixed today.',
  urgency: 'normal' as const,
  createdAt: '2024-01-15T09:00:00.000Z'
}

const BATCH = {
  schemaVersion: 1 as const,
  batchId: 'batch-1',
  envelopes: [ENVELOPE, ENVELOPE]
}

let capture: ToolRegistrationCapture

beforeAll(async () => {
  capture = await captureRegisteredTools()
}, 30_000)

afterAll(async () => {
  await capture?.stop()
})

function tool(name: string): CapturedTool {
  const found = capture.tools.find((candidate) => candidate.name === name)
  if (isNullish(found)) throw new Error(`the install registered no '${name}'`)
  return found
}

describe('the delivered envelope tells a manager to route the work', () => {
  const delivered = (): string => messageContextForTest(ENVELOPE, 'ada', 'manager')

  it('states the duty as a flat instruction, in the turn the work arrives', () => {
    expect(delivered()).toContain('YOU ARE A MANAGER, SO THIS IS WORK TO ROUTE AND NOT WORK TO DO')
  })

  it('names the four things delegation actually needs', () => {
    const text = delivered()
    expect(text).toContain('ONE owner')
    expect(text).toContain('the expected output')
    expect(text).toContain('the evidence required')
    expect(text).toContain('the deadline')
  })

  it('names the wake, because "my team is asleep" is what makes doing it yourself feel forced', () => {
    const text = delivered()
    expect(text).toContain('The send IS the wake')
    expect(text).toContain('org_send starts a person who is not running')
    expect(text).toContain('nobody has to be up first')
  })

  it('names the benched exception and its exact remedy', () => {
    expect(delivered()).toContain('org_recall them and send again')
  })

  it('leaves no dead end: hire, create, or escalate', () => {
    const text = delivered()
    expect(text).toContain('hire somebody with org_hire')
    expect(text).toContain('create the department that should own it')
    expect(text).toContain('escalate to whoever asked you')
  })

  it('forbids the specific actions a manager reaches for instead', () => {
    expect(delivered()).toContain(
      'Do not open the editor, run the command, or produce the result yourself'
    )
  })

  it('closes the loop upward', () => {
    expect(delivered()).toContain('reply to the sender saying who owns it')
  })

  it('does NOT hand a manager the worker instruction that caused this', () => {
    expect(delivered()).not.toContain('Reply only with a needed result')
    expect(delivered()).not.toContain('You do this work yourself')
  })
})

describe('the delivered envelope tells a worker to do the work', () => {
  const delivered = (): string => messageContextForTest(ENVELOPE, 'milo', 'worker')

  it('says the worker executes, and says it plainly', () => {
    const text = delivered()
    expect(text).toContain('You do this work yourself')
    expect(text).toContain('own the assigned output, verify it')
  })

  it('forbids a worker delegating its own work', () => {
    expect(delivered()).toContain('Do not hand it to somebody else')
  })

  it('is not given the manager text — the same defect mirrored', () => {
    const text = delivered()
    expect(text).not.toContain('YOU ARE A MANAGER')
    expect(text).not.toContain('WORK TO ROUTE')
  })
})

describe('an unresolvable role claims nothing', () => {
  /**
   * A cold manifest cache plus an unreachable docstore yields `unknown`. Mail
   * must still be delivered — the 19-hour-outage class is worse than a missing
   * sentence — but the reader must not be TOLD it is a worker when nobody
   * knows. Guessing here would be the "unreadable becomes empty" collapse this
   * repo refuses everywhere else.
   */
  it('delivers the shared guidance and asserts no role', () => {
    const text = messageContextForTest(ENVELOPE, 'ada', 'unknown')
    expect(text).toContain('This is an organization peer message')
    expect(text).not.toContain('YOU ARE A MANAGER')
    expect(text).not.toContain('You do this work yourself')
  })
})

describe('the batch triage prompt splits the same way', () => {
  it('tells a manager to route every item, not to work through them', () => {
    const text = mailboxBatchContextForTest(BATCH, 'ada', 'manager')
    expect(text).toContain('ROUTE it to an owner with org_send')
    expect(text).toContain('Do not work through the checklist yourself')
  })

  it('leaves a worker the acting instruction', () => {
    const text = mailboxBatchContextForTest(BATCH, 'milo', 'worker')
    expect(text).toContain('act on it where needed')
    expect(text).not.toContain('Do not work through the checklist yourself')
  })
})

describe("org_send's own description states that the send is the wake", () => {
  /**
   * Read off the SERIALIZED tool description — the string the provider hands
   * the model — because reading it any other way would prove something the
   * model never sees. Same instrument as `ReportsToNamesTheParent`.
   */
  it('says so at the call site, not only in a skill', () => {
    const description = tool('org_send').description ?? ''
    expect(description).toContain('THE SEND IS THE WAKE')
    expect(description).toContain('you never have to start a person before delegating to them')
    expect(description).toContain('org_recall them, then send again')
  })
})

describe('the manager skill states the negative and the worker skill states the conversion', () => {
  it('the manager skill opens with the flat rule', () => {
    expect(MANAGER_SKILL).toContain('**You are a manager. You do not do the work.**')
  })

  it("the manager skill's frontmatter description leads with the duty", () => {
    // Pi reads the description first, and for many models it is the only part
    // read before the skill is chosen.
    const frontmatter = MANAGER_SKILL.split('---')[1] ?? ''
    expect(frontmatter).toContain('you DELEGATE and you do not do the work yourself')
  })

  it('the manager skill no longer carries the escape hatch that made the rule unenforceable', () => {
    // "Do specialist work only when no responsible specialist can own it and
    // delay would be harmful." A manager whose reports are all asleep reads
    // that as satisfied every single time.
    expect(MANAGER_SKILL).not.toContain('Do specialist work only when no responsible specialist')
  })

  it('the manager skill answers every excuse rather than only forbidding', () => {
    for (const excuse of [
      'It is faster if I just do it',
      'Nobody on my team knows this',
      'It is too small to delegate',
      'I need to understand it first',
      'There is nobody up'
    ]) {
      expect(MANAGER_SKILL, `the skill must answer: ${excuse}`).toContain(excuse)
    }
  })

  it('the manager skill names the wake as the reason the excuse is false', () => {
    expect(MANAGER_SKILL).toContain('`org_send` IS the wake')
    expect(MANAGER_SKILL).toContain('A message to a person who is not running starts them')
  })

  it('the worker skill says the worker does the work and does not delegate', () => {
    expect(WORKER_SKILL).toContain('**You do the work.**')
    expect(WORKER_SKILL).toContain('You are not a manager')
    expect(WORKER_SKILL).toContain('Do not hand your own assigned work to somebody else')
  })

  it('the worker skill says a worker may collaborate with peers', () => {
    expect(WORKER_SKILL).toContain('## Collaborating with peers')
    expect(WORKER_SKILL).toContain('`org_send`')
  })

  it('the worker skill names the conversion mechanism, not a vague pointer', () => {
    // Operator, verbatim: "if we ever say, hey [name], I want you to head a
    // department, it knows how to convert to the department and then it gets
    // the management skill".
    expect(WORKER_SKILL).toContain('## If you are asked to head a department')
    expect(WORKER_SKILL).toContain('existingHeadPersonId')
    expect(WORKER_SKILL).toContain('org_add_department')
  })

  it('the worker skill says what a conversion does to its own skill set', () => {
    expect(WORKER_SKILL).toContain(
      'this skill is\nUNINSTALLED from your home and the `manager` skill is INSTALLED in its place'
    )
    expect(WORKER_SKILL).toContain('You stop doing the work yourself')
  })

  it('the manager skill names the reverse conversion', () => {
    expect(MANAGER_SKILL).toContain('## If you stop being a manager')
    expect(MANAGER_SKILL).toContain('installs `worker`')
  })
})
