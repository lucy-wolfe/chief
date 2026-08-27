// The one reading of a `say` body.
//
// # The defect this file exists to make impossible
//
// The browser sent `{"message": "...", "mode": "prompt"}` and this route read
// `body.text`. Both halves were tested — the client against its own schema, the
// route against its own handler — and both were green, so the mismatch was
// invisible until an operator typed into the composer and every single message
// came back `422 empty-message`. That is the same shape as the missing `/api`
// prefix: two correct halves and nothing checking the seam.
//
// The wire word is `text`, because that is what it is called everywhere it ends
// up — `PersonTalk.say(…, text)`, `AgentHarness.prompt(text)`, and the
// transcript entry. So the assertion that matters most below is the NEGATIVE
// one: a body carrying the old spelling is refused, not quietly accepted.
import { beforeEach, describe, expect, it, vi } from 'vitest'

// The parser reaches `PersonTalkError` through `PersonTalk`, which reaches the
// agent runtime. Neither the roster nor the harness has any part in reading a
// body, so both are stubbed away — this keeps the test about the parser and
// keeps Pi's provider modules out of a suite that parses JSON.
vi.mock('@/server/HostedRoster', () => ({ agentFor: vi.fn() }))
vi.mock('@/server/AgentHost', () => ({ hostedSession: vi.fn() }))

const { PersonTalkError } = await import('@/server/PersonTalk')
const { sayRequest } = await import('@/server/SayRequest')

/** The refusal a caller sees, without a try/catch in every test. */
function refusalOf(body: unknown): unknown {
  try {
    sayRequest(body)
  } catch (error) {
    return error
  }
  return undefined
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe('sayRequest', () => {
  it('reads a turn as { text, mode }', () => {
    expect(sayRequest({ text: 'status?', mode: 'steer' })).toEqual({
      text: 'status?',
      mode: 'steer'
    })
  })

  it('defaults an absent mode to prompt', () => {
    // `prompt` is the composer's default and the only mode a caller that has
    // never heard of the harness queues would want.
    expect(sayRequest({ text: 'status?' })).toEqual({ text: 'status?', mode: 'prompt' })
  })

  it('REFUSES the old `message` spelling instead of accepting either', () => {
    // Accepting both would be a compatibility layer over a seam that is now
    // correct on both sides — and it would let the browser drift back to the
    // spelling that produced `422 empty-message` for every operator message,
    // with nothing to notice.
    expect(refusalOf({ message: 'status?', mode: 'prompt' })).toMatchObject({
      status: 422,
      code: 'invalid-request'
    })
  })

  it('names the field it is missing', () => {
    // "invalid request" tells an operator nothing. The field name is the whole
    // actionable content of this refusal.
    expect(refusalOf({})).toMatchObject({
      message: '"text" is required and must be a string'
    })
  })

  it('refuses a non-string text rather than coercing it', () => {
    // `String(42)` would send an agent the number as a turn, and `String(null)`
    // would send it the word "null".
    expect(refusalOf({ text: 42 })).toBeInstanceOf(PersonTalkError)
    expect(refusalOf({ text: null })).toBeInstanceOf(PersonTalkError)
  })

  it('refuses a mode it does not know rather than defaulting it', () => {
    // A typo silently becoming `prompt` would start a NEW turn where the
    // operator meant to correct the running one — the single most damaging way
    // this could be wrong, because it looks like it worked.
    expect(refusalOf({ text: 'wait', mode: 'steeer' })).toMatchObject({
      status: 422,
      code: 'invalid-request',
      message: '"mode" must be one of: prompt, steer, followUp'
    })
  })

  it('refuses a body that is not a JSON object at all', () => {
    // The route reads the body INSIDE `routeResult`, so each of these becomes a
    // 422 with an envelope the client can read rather than an unmapped 500.
    expect(refusalOf(undefined)).toBeInstanceOf(PersonTalkError)
    expect(refusalOf('status?')).toBeInstanceOf(PersonTalkError)
    expect(refusalOf(['status?'])).toBeInstanceOf(PersonTalkError)
    expect(refusalOf(null)).toBeInstanceOf(PersonTalkError)
  })

  it('does NOT judge an empty turn — that is the roster’s side of the seam', () => {
    // Blank text is refused by `PersonTalk.say` BEFORE it reaches the roster,
    // so refusing it here as well would put one rule in two places. This parser
    // decides shape; the verb decides substance.
    expect(sayRequest({ text: '   ' })).toEqual({ text: '   ', mode: 'prompt' })
  })
})
