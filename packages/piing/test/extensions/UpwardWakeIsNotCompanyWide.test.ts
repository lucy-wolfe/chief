// EVERY UPWARD REPLY TO THE CEO WAS AUTHORIZATION-REFUSED ON THE WAKE PATH.
//
// The pane of every non-executive person printed this after messaging the CEO:
//
//   ✅ Message sent · @ceo
//   The recipient's immediate wake-up hit an issue; it will retry
//   automatically on the next reconcile.
//
// and `bus/events.jsonl` carried the reason eight times over, once per
// non-executive person:
//
//   {"event":"message-wake-deferred","to":"ceo","error":"org row refused:
//    caller-out-of-company-scope: caller 'nova-sterling' does not head
//    'executive', so it may not launch the runtime for the whole company;
//    this write reaches every person in it"}
//
// The send's reactive wake posted `/v1/org/runtime/launch`, and that route's
// subject is the COMPANY: `require_company_wide_authority` asks the ordinary
// subtree question about the ROOT department, so only the person who heads the
// root passes. One person waking the one person they messaged is not a
// company-wide act, and it must not be spelled as one.
//
// Narrowing that route's fence to the named recipients would NOT have fixed
// it, and this file exists partly to record that: `launch_runtime` calls
// `org_ops::start_person` for each requested id, which asks
// `actor_out_of_scope` about the department that person lives in. A
// subordinate does not manage the CEO, so the refusal would simply have moved
// one layer down and come back as `actor-out-of-scope`.
//
// The delivery already IS the recipient-scoped write. `/v1/org/mailbox/delta`
// is judged per entry as "consumption of your own mailbox or a delivery from
// you", and chiefd's converge cycle grants launch intent to exactly the
// recipients of pending rows. So the wake rides on the delivery, and the send
// path makes no runtime write at all.
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const SOURCE = readFileSync(
  fileURLToPath(new URL('../../extensions/organization-intercom.ts', import.meta.url)),
  'utf8'
)

/** Comments name deleted routes as history on purpose — including the ones
 * this file exists to keep deleted — so absence is judged against code. */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
}

/** The `org_send` registration body, from its name to the next registration. */
function orgSendBody(): string {
  const start = SOURCE.indexOf('name: "org_send"')
  expect(start, 'org_send must still be registered').toBeGreaterThan(-1)
  const rest = SOURCE.slice(start)
  const end = rest.indexOf('toolRegistrar.registerTool({')
  expect(end, 'org_send must not be the last registration in the file').toBeGreaterThan(-1)
  return stripComments(rest.slice(0, end))
}

describe('the upward wake is scoped to the recipient', () => {
  it('org_send makes no company-wide runtime write', () => {
    const body = orgSendBody()
    // `reconcileRuntime` posts `/v1/org/runtime/launch`, whose fence is the
    // root department. Calling it from a send is what refused every
    // subordinate, and no shape of argument narrows it — the route's subject
    // is the company whatever the body names.
    expect(body).not.toMatch(/\breconcileRuntime\s*\(/)
    expect(body).not.toContain('/v1/org/runtime/launch')
  })

  it('the refusal it produced has no emitter left', () => {
    // `message-wake-deferred` was the artifact of a wake the sender was never
    // allowed to attempt. There is no wake attempt to defer any more, so an
    // emitter left behind could only report a failure of something that no
    // longer happens.
    expect(stripComments(SOURCE)).not.toContain('message-wake-deferred')
    expect(SOURCE).not.toContain("The recipient's immediate wake-up hit an issue")
    // The same defect, one tool over: the provider-health escalation messages
    // a person's DIRECT MANAGER, and woke them the same company-wide way.
    expect(stripComments(SOURCE)).not.toContain('provider-failure-alert-wake-deferred')
  })

  it('the delivery route the wake now rides on is still the one org_send posts', () => {
    // The positive half. If the send ever stopped writing through
    // `/v1/org/mailbox/delta`, the wake would silently stop happening and
    // every assertion above would still pass.
    expect(SOURCE).toContain('"/v1/org/mailbox/delta"')
  })
})
