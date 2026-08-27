/**
 * **A TURN THAT COMPLETED IS NOT NECESSARILY A TURN THAT WORKED.**
 *
 * The third member of the looks-finished-but-isn't family, after the filtered
 * turn (#1222) and the busy-but-silent compaction (#1230): the model writes
 * `<invoke name="org_send">…</invoke>` as ordinary assistant TEXT, nothing
 * executes, `agent_settled` fires, and the settle countdown parks a person
 * whose work never happened. Every rule reads the turn as completed — because
 * by every signal it is.
 *
 * Measured on a live box: 215 occurrences across 13 people, 61% within
 * six transcript rows of a resume notice.
 *
 * This file pins the CLASSIFIER. The caller's half — one bounded corrective,
 * then the card — is driven in `test/extensions/`.
 */
import { printedToolCall } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

/** Pi's `agent_end` shape, reduced to what the detector reads. */
function ended(content: unknown[]): unknown {
  return { messages: [{ role: 'assistant', content }] }
}

describe('printedToolCall', () => {
  test('names the tool a turn printed instead of calling', () => {
    const found = printedToolCall(
      ended([
        {
          type: 'text',
          text: 'I will message the lead.\n<invoke name="org_send">\n<parameter name="to">lead</parameter>\n</invoke>'
        }
      ])
    )
    expect(found?.toolName).toBe('org_send')
  })

  test('a message that ALSO made a real call is not this defect', () => {
    // THE DIRECTION THAT MATTERS MOST. An agent that quotes the grammar while
    // actually calling the tool has done its work, and correcting it would be
    // telling somebody to redo something they already did.
    expect(
      printedToolCall(
        ended([
          { type: 'text', text: 'Calling it now, like <invoke name="org_send">…</invoke>' },
          { type: 'toolCall', toolName: 'org_send' }
        ])
      )
    ).toBeUndefined()
  })

  test('an ordinary reply is not a detection', () => {
    expect(printedToolCall(ended([{ type: 'text', text: 'Done — I sent it.' }]))).toBeUndefined()
  })

  test('prose that merely mentions invoke is not the grammar', () => {
    // The shape is the anchor, not the word: an opening tag with no closing one
    // is somebody talking ABOUT tool calls.
    expect(
      printedToolCall(ended([{ type: 'text', text: 'you should invoke name="org_send" somehow' }]))
    ).toBeUndefined()
  })

  test('only the LAST assistant message counts', () => {
    // An earlier printed call that was already corrected is history. Treating
    // it as live would re-correct something the model has since fixed.
    const found = printedToolCall({
      messages: [
        {
          role: 'assistant',
          content: [{ type: 'text', text: '<invoke name="org_send"></invoke>' }]
        },
        { role: 'assistant', content: [{ type: 'text', text: 'Sent.' }] }
      ]
    })
    expect(found).toBeUndefined()
  })

  test('a malformed event is undefined rather than a throw', () => {
    expect(printedToolCall(undefined)).toBeUndefined()
    expect(printedToolCall({ messages: 'not an array' })).toBeUndefined()
  })
})
