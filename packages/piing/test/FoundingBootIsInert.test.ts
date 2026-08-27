/**
 * THE FIRST BOOT OF A BRAND-NEW COMPANY DOES NOTHING.
 *
 * The operator, in their own words: "I just boot the founder and then that
 * launched me an application and then what happened, it launched the chief
 * account once the founder finished. The annoying part is it started creating
 * departments and stuff. It should not do anything. The very first time, just
 * start and let the user do anything. Don't have it create stuff in front of
 * you."
 *
 * Two messages reach a newly minted CEO on that boot, and both used to push it
 * toward work it would have to invent:
 *
 * 1. the launcher's fresh-session argument (`chief-cli`'s
 *    `spawn_cmd::fresh_session_message`, tested on the Rust side), and
 * 2. this extension's own work-resume prompt, which asked a first boot to
 *    "take the exact next useful step toward the work you were hired for".
 *
 * On a company seconds old the second sentence is a lie of the same shape as
 * the first: there is no department, no goal, no schedule, no history and no
 * mail, so the only compliant next step is an invented one — and the CEO
 * invented an org chart.
 *
 * The seam under test is `workResumePrompt`, which is where the copy is chosen
 * AND where the founding rule is decided. `workResumeDetails` reports the two
 * facts (first materialization, company headcount) and judges neither, so this
 * suite drives the whole rule with plain data.
 */
import { type PersonRecord, workResumePrompt } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const CEO: PersonRecord = {
  id: 'ceo',
  name: 'Chief',
  title: 'Chief',
  kind: 'executive',
  departmentId: 'executive',
  employmentState: 'active',
  createdAt: '2026-08-18T00:00:00.000Z'
}

/** The instructions a founding boot must never be given, in the words each one
 * is actually written in. */
const WORK_PUSHES = ['next useful step', 'work you were hired for', 'org_roster', 'catch-up check']

describe('the founding boot', () => {
  test('asks the CEO of a seconds-old company to introduce itself and stop', () => {
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      firstBoot: true,
      companyPeopleCount: 1,
      pendingMessageCount: 0,
      protectedSchedules: []
    })
    expect(prompt).toContain('created moments ago')
    expect(prompt).toContain('Introduce yourself')
    expect(prompt).toContain('Create no department, hire nobody')
    // An acknowledgement is the CORRECT output here, so nothing may forbid one.
    expect(prompt).not.toContain('acknowledgement chatter')
    expect(prompt).toContain('Chief (ceo)')
    for (const push of WORK_PUSHES) expect(prompt).not.toContain(push)
  })

  test('is not merely "this person has never run": a hire on a staffed company still gets to work', () => {
    // Same first materialization, one more person in the company, and the mail
    // their manager sent when they asked for them. A new hire has a mandate and
    // work waiting, so the orientation pass is right for them and stays.
    //
    // The mailbox is what carries that here, and it was `0` until the idle
    // ruling of 2026-08-18. It read as "a hire is different from the founding
    // CEO because the company is staffed", and a staffed company is not the
    // question — five sleeping people nobody had asked for anything were also
    // a staffed company, and two of them went and built an org chart. Headcount
    // says the company exists; only the mailbox says anybody wants something.
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      firstBoot: true,
      companyPeopleCount: 2,
      pendingMessageCount: 1,
      protectedSchedules: []
    })
    expect(prompt).toContain('You are online for the first time')
    expect(prompt).toContain('work you were hired for')
    expect(prompt).not.toContain('created moments ago')
  })

  test('is not a lone person who has already run: a genuine resume keeps its recovery copy', () => {
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      companyPeopleCount: 1,
      pendingMessageCount: 2,
      protectedSchedules: []
    })
    expect(prompt).toContain('Work resumed after this Pi session restarted')
    expect(prompt).toContain('2 messages waiting')
    expect(prompt).not.toContain('created moments ago')
  })
})

/**
 * NO ASSIGNED WORK MEANS IDLE, NOT GO AND FIND SOME.
 *
 * Operator ruling, 2026-08-18, in their own words: "what is assigned work? you
 * mean no message or goals? that's fine. Just let them idle until the 2min
 * passes. never force kill them."
 *
 * The founding arm above only catches the very first boot of a brand-new
 * company. The same hole reopens on every later one, and the operator watched
 * it: a company was created and staffed with five sleeping people, and the only
 * instruction ever given about any of them was "leave every one of them
 * asleep". No project, no goal, no repository, no task was ever mentioned.
 *
 * Two `Wake Up` clicks then produced, unbidden, an Engineering department, a
 * head hired into it, a third person recalled, and six messages about "critical
 * chiefd blockers" — about half a dollar of spend in two minutes. The reasoning
 * was impeccable given the prompt: this pane was told to continue the next real
 * piece of work and forbidden from acknowledging, there was no work, and the
 * one thing it could see was the chief SOURCE TREE at the launcher root, which
 * is mounted there only so Pi can be resolved.
 *
 * So assigned work is a MESSAGE WAITING or a schedule this person owns, and
 * nothing else. Company goals are not the other half of it — they do not exist:
 * #1047 dropped `manager_goals`, `delegated_goals`, `goal_watches` and
 * `goal_intents` outright, and `hasOpenOrganizationWork` in the same extension
 * already states the consequence, "with goals deleted, the mailbox IS the work
 * queue". Nothing DISCOVERABLE counts, which is the property that survives a
 * rewording: a repository a person can see is plumbing they were not given.
 */
describe('a boot with no assigned work', () => {
  test('is told to come up, say so, and do nothing — with the acknowledgement ban lifted', () => {
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      firstBoot: true,
      companyPeopleCount: 5,
      pendingMessageCount: 0,
      protectedSchedules: []
    })
    expect(prompt).toContain('nothing is assigned to you')
    expect(prompt).toContain('up and available')
    expect(prompt).toContain('Create no department, hire nobody')
    // The sentence that answers the reasoning which adopted the launcher's own
    // checkout. It is what makes the rule survive a prompt rewrite.
    expect(prompt).toContain(
      'source tree or anything else you can see on disk is NOT work anybody gave you'
    )
    // An acknowledgement is the CORRECT output, so nothing may forbid one.
    expect(prompt).not.toContain('acknowledgement chatter')
    for (const push of WORK_PUSHES) expect(prompt).not.toContain(push)
    // Not the founding copy: this company is staffed and is not seconds old.
    expect(prompt).not.toContain('created moments ago')
  })

  test('is not only a first boot: a RESUME with an empty mailbox idles too', () => {
    // The `Wake Up` click the operator made lands here once the person has run
    // once. The old recovery pass sent it through org_roster and a catch-up
    // check before step 4 ever offered "stop after a concise status".
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      companyPeopleCount: 5,
      pendingMessageCount: 0,
      protectedSchedules: []
    })
    expect(prompt).toContain('nothing is assigned to you')
    expect(prompt).toContain('again after this Pi session restarted')
    for (const push of WORK_PUSHES) expect(prompt).not.toContain(push)
  })

  test('A PERSON WITH WORK IS UNCHANGED: mail waiting gets the recovery pass and keeps the ban', () => {
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      companyPeopleCount: 5,
      pendingMessageCount: 1,
      protectedSchedules: []
    })
    expect(prompt).toContain('Work resumed after this Pi session restarted')
    expect(prompt).toContain('1 message waiting; read it before anything else.')
    expect(prompt).toContain('Call org_roster')
    expect(prompt).toContain('Do not send readiness or acknowledgement chatter')
    expect(prompt).not.toContain('nothing is assigned to you')
  })

  test('a schedule this person owns is assigned work too, even with an empty mailbox', () => {
    // A durable recurring responsibility is a claim on this person's attention
    // that nobody has to restate. Step 3 exists for exactly that, and an idle
    // arm that swallowed it would drop a check the person owns.
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      companyPeopleCount: 5,
      pendingMessageCount: 0,
      protectedSchedules: ['daily-close 09:00']
    })
    expect(prompt).toContain('Protected ChiefD schedule: daily-close 09:00')
    expect(prompt).not.toContain('nothing is assigned to you')
  })

  test('a first boot WITH mail is a new hire and still gets to work', () => {
    const prompt = workResumePrompt(CEO, {
      personId: 'ceo',
      firstBoot: true,
      companyPeopleCount: 5,
      pendingMessageCount: 2,
      protectedSchedules: []
    })
    expect(prompt).toContain('You are online for the first time')
    expect(prompt).toContain('work you were hired for')
    expect(prompt).toContain('2 messages waiting')
    expect(prompt).not.toContain('nothing is assigned to you')
  })
})
