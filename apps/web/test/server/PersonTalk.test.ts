// The talk verbs: what they refuse, and what they report.
//
// The refusal is the interesting half. Under apps/api every one of these
// answered 409 `person-not-running` for EVERY person, because its own roster
// read the wrong field name — so a 409 meant "the host failed to ask", not
// "the roster says no". These tests pin the difference: the answer comes from
// `agentFor`, which is chiefd's roster, and nothing here starts an agent
// because somebody typed at it.
import { beforeEach, describe, expect, it, vi } from 'vitest'

const agentFor = vi.fn()
const hostedSession = vi.fn()

vi.mock('@/server/HostedRoster', () => ({
  agentFor: (...args: unknown[]) => agentFor(...args)
}))
vi.mock('@/server/AgentHost', () => ({
  hostedSession: (...args: unknown[]) => hostedSession(...args)
}))

const { abort, PersonTalkError, say, transcript } = await import('@/server/PersonTalk')

beforeEach(() => {
  agentFor.mockReset()
  hostedSession.mockReset()
})

describe('say', () => {
  it('returns the assistant’s words for a person chiefd wants running', async () => {
    agentFor.mockResolvedValue({
      prompt: async () => ({
        role: 'assistant',
        content: [{ type: 'text', text: 'On it.' }]
      })
    })

    // The MODE is part of the answer, not just the request. A caller reading
    // `{personId, reply}` alone cannot tell a turn that was answered from a
    // message that was queued behind one.
    await expect(say('acme', 'ceo', 'status?')).resolves.toEqual({
      personId: 'ceo',
      mode: 'prompt',
      reply: 'On it.'
    })
  })

  it('keeps tool calls and thinking out of the reply', async () => {
    // Real content, but not what the agent SAID. Concatenating it would put a
    // tool's JSON on screen as if the agent had spoken it.
    agentFor.mockResolvedValue({
      prompt: async () => ({
        role: 'assistant',
        content: [
          { type: 'thinking', thinking: 'let me check the ledger' },
          { type: 'text', text: 'Two open threads.' },
          { type: 'toolCall', name: 'org_roster', input: {} }
        ]
      })
    })

    await expect(say('acme', 'ceo', 'status?')).resolves.toEqual({
      personId: 'ceo',
      mode: 'prompt',
      reply: 'Two open threads.'
    })
  })

  it('steers a running turn through the harness’s own queue', async () => {
    // The composer offers three modes and this honoured one. `steer` and
    // `followUp` were SENT, ignored, and run as ordinary prompts — so an
    // operator correcting an agent mid-turn started a SECOND turn instead, and
    // the harness's queue was never used at all.
    const steer = vi.fn().mockResolvedValue(undefined)
    const prompt = vi.fn()
    agentFor.mockResolvedValue({ prompt, steer })

    await expect(say('acme', 'ceo', 'actually, check the ledger', 'steer')).resolves.toEqual({
      personId: 'ceo',
      mode: 'steer'
    })
    expect(steer).toHaveBeenCalledWith('actually, check the ledger')
    // The proof that it is not a second turn.
    expect(prompt).not.toHaveBeenCalled()
  })

  it('queues a follow-up without inventing a reply for it', async () => {
    // Only `prompt` has an answer to wait for. A queued message reporting
    // `reply: ''` would be indistinguishable from an agent that said nothing.
    const followUp = vi.fn().mockResolvedValue(undefined)
    const prompt = vi.fn()
    agentFor.mockResolvedValue({ prompt, followUp })

    const outcome = await say('acme', 'ceo', 'and then deploy', 'followUp')

    expect(outcome).toEqual({ personId: 'ceo', mode: 'followUp' })
    expect('reply' in outcome).toBe(false)
    expect(followUp).toHaveBeenCalledWith('and then deploy')
    expect(prompt).not.toHaveBeenCalled()
  })

  it('refuses a turn the provider could not complete rather than reporting silence', async () => {
    // `prompt` RESOLVES whatever happened: a turn that died on `Connection
    // error.` comes back with `stopReason: 'error'` and no content, and this
    // used to be returned as `{"reply":""}` with a 200. An operator saw the
    // agent answer nothing, twice, with no way to tell a broken route from a
    // quiet agent — while the reason sat in the transcript.
    agentFor.mockResolvedValue({
      prompt: async () => ({
        role: 'assistant',
        content: [],
        stopReason: 'error',
        errorMessage: 'Connection error.'
      })
    })

    await expect(say('acme', 'ceo', 'status?')).rejects.toMatchObject({
      status: 502,
      code: 'turn-failed'
    })
    // The provider's own words are carried: the most common cause is this
    // server's own TLS trust store, which is invisible from the browser, and
    // naming it is the difference between a five-minute fix and a day.
    await expect(say('acme', 'ceo', 'status?')).rejects.toThrow('Connection error.')
  })

  /** A harness whose one turn fails with the provider's own sentence. */
  function failingWith(errorMessage: string): void {
    agentFor.mockResolvedValue({
      prompt: async () => ({
        role: 'assistant',
        content: [],
        stopReason: 'error',
        errorMessage
      })
    })
  }

  it('names a REJECTED CREDENTIAL rather than calling it a connection error', async () => {
    // THE DEFECT THIS PINS, measured: a company whose provider key had been
    // replaced with a placeholder answered `502 turn-failed: "Connection
    // error."`, which sent a reader to check the network on a host whose
    // egress was answering 200 in 100ms. A refused credential is a durable
    // condition of the company — retrying cannot fix it and nothing will
    // succeed until somebody replaces the key — so it is a 409, the same class
    // of answer as "this person is not running", and never a 502.
    failingWith('AI_APICallError: Unauthorized (401)')

    await expect(say('acme', 'ceo', 'status?')).rejects.toMatchObject({
      status: 409,
      code: 'provider-credential-rejected'
    })
    // The provider's words survive as DETAIL, after this server has said what
    // it thinks happened, rather than as the whole answer.
    await expect(say('acme', 'ceo', 'status?')).rejects.toThrow('Unauthorized (401)')
  })

  it('names a RATE LIMIT, which is the one worth retrying', async () => {
    failingWith('429 Too Many Requests: rate limit exceeded')

    await expect(say('acme', 'ceo', 'status?')).rejects.toMatchObject({
      status: 429,
      code: 'provider-rate-limited'
    })
  })

  it('carries a REJECTED REQUEST’s provider text without asserting which cause it was', async () => {
    // #1011's shape: a provider validates every tool definition's schema and
    // rejects the WHOLE catalog over any one of them, so one malformed tool
    // disarms every tool the person has.
    //
    // This assertion used to be `toThrow('tool catalog')` — it required the
    // message to name that cause as THE known one. That is the same defect the
    // test below names in its own comment ("name a cause an operator can CHECK
    // rather than assert one"), and it shipped: observed live, a session over
    // the context window produced this same branch, and the message told the
    // operator the known cause was a malformed tool schema while quoting the
    // provider saying `maximum context length is 1048576 tokens. However, you
    // requested 1053374`. An operator reading that hunts a schema bug for a
    // session. The provider's own words were the only true part.
    failingWith(
      "400 Invalid schema for function 'org_maintain_session': schema must be a JSON Schema " +
        "of 'type: \"object\"', got 'type: null'"
    )

    await expect(say('acme', 'ceo', 'status?')).rejects.toMatchObject({
      status: 502,
      code: 'provider-rejected-request'
    })
    // The provider's verbatim text survives — it is the diagnosis.
    await expect(say('acme', 'ceo', 'status?')).rejects.toThrow('Invalid schema for function')
  })

  it('does not blame the tool catalog for a REJECTED REQUEST that was a context overrun', async () => {
    // The live case. Same branch, different cause, and the old message stated
    // the wrong one as fact.
    failingWith(
      '400 {"message":"This model\'s maximum context length is 1048576 tokens. However, you ' +
        'requested 1053374 tokens (1053374 in the messages, 0 in the completion).",' +
        '"type":"invalid_request_error"}'
    )

    await expect(say('acme', 'ceo', 'status?')).rejects.toMatchObject({
      status: 502,
      code: 'provider-rejected-request'
    })
    // The provider's own numbers reach the operator...
    await expect(say('acme', 'ceo', 'status?')).rejects.toThrow('maximum context length')
    // ...and the message does not tell them the cause is a malformed tool.
    await expect(say('acme', 'ceo', 'status?')).rejects.not.toThrow(/The known cause/)
  })

  it('keeps an unrecognised failure a transport fault, with the TLS note', async () => {
    // The safe direction for anything unmatched: name a cause an operator can
    // CHECK rather than assert one. This server hosts the harness in its own
    // process, so a TLS-intercepting egress fails every call without the CA
    // bundle chiefd materialized — invisible from the browser, and common.
    failingWith('socket hang up')

    await expect(say('acme', 'ceo', 'status?')).rejects.toMatchObject({
      status: 502,
      code: 'turn-failed'
    })
    await expect(say('acme', 'ceo', 'status?')).rejects.toThrow('TLS trust store')
  })

  it('refuses 409 for somebody chiefd does not want running', async () => {
    agentFor.mockResolvedValue(undefined)

    await expect(say('acme', 'ghost', 'hello')).rejects.toMatchObject({
      status: 409,
      code: 'person-not-running'
    })
  })

  it('refuses an empty turn BEFORE reaching the roster', async () => {
    // Order matters: an empty message is the caller's mistake, and asking
    // chiefd about it first would report "not running" for a person who is
    // running perfectly well.
    await expect(say('acme', 'ceo', '   ')).rejects.toBeInstanceOf(PersonTalkError)
    expect(agentFor).not.toHaveBeenCalled()
  })

  it('lets the harness’s own failure through rather than reporting success', async () => {
    // A fire-and-forget `say` would answer 200 here and leave the operator
    // watching an agent that looks healthy and answers nothing.
    agentFor.mockResolvedValue({
      prompt: async () => {
        throw new Error('provider refused the request')
      }
    })

    await expect(say('acme', 'ceo', 'hello')).rejects.toThrow('provider refused')
  })
})

describe('abort', () => {
  it('reports the queued messages it threw away', async () => {
    // An operator who steered three messages and then stopped needs to know
    // those three are gone rather than pending.
    agentFor.mockResolvedValue({
      abort: async () => ({ clearedSteer: [{}, {}, {}], clearedFollowUp: [{}] })
    })

    await expect(abort('acme', 'ceo')).resolves.toEqual({
      clearedSteer: 3,
      clearedFollowUp: 1
    })
  })

  it('is not an error on an idle agent', async () => {
    // Pressing stop twice, or stopping a turn that has just finished, is not a
    // mistake — and a refusal there trains an operator to ignore refusals.
    agentFor.mockResolvedValue({
      abort: async () => ({ clearedSteer: [], clearedFollowUp: [] })
    })

    await expect(abort('acme', 'ceo')).resolves.toEqual({
      clearedSteer: 0,
      clearedFollowUp: 0
    })
  })

  it('refuses 409 for somebody chiefd does not want running', async () => {
    agentFor.mockResolvedValue(undefined)

    await expect(abort('acme', 'ghost')).rejects.toMatchObject({ code: 'person-not-running' })
  })
})

describe('transcript', () => {
  it('reads the session the harness itself writes to', async () => {
    agentFor.mockResolvedValue({})
    hostedSession.mockReturnValue({
      getBranch: async () => [
        { id: 'e1', type: 'message', message: { role: 'user', content: 'status?' } },
        {
          id: 'e2',
          type: 'message',
          message: { role: 'assistant', content: [{ type: 'text', text: 'Two open threads.' }] }
        }
      ]
    })

    // Shaped as a session ENTRY — `{type: 'message', message: {role, content}}`
    // — because that is what the browser's `rowsFromTranscript` reads. It used
    // to be `{id, role, text}`, a flat shape of this module's own invention, so
    // the fold matched no case and skipped every entry: the pane rendered an
    // empty conversation for an agent with a full transcript. The same defect
    // as the person stream's private frame names, one read further along.
    await expect(transcript('acme', 'ceo')).resolves.toEqual({
      personId: 'ceo',
      entries: [
        {
          type: 'message',
          id: 'e1',
          message: { role: 'user', content: [{ type: 'text', text: 'status?' }] }
        },
        {
          type: 'message',
          id: 'e2',
          message: { role: 'assistant', content: [{ type: 'text', text: 'Two open threads.' }] }
        }
      ]
    })
  })

  it('shows only what a reader can read', async () => {
    // A session tree also carries model changes, tool activity, compactions and
    // labels. Rendering those as conversation would show an operator machinery
    // as if it were speech.
    agentFor.mockResolvedValue({})
    hostedSession.mockReturnValue({
      getBranch: async () => [
        { id: 'e1', type: 'modelChange', provider: 'openrouter', modelId: 'm1' },
        { id: 'e2', type: 'compaction', summary: 'earlier turns' },
        { id: 'e3', type: 'message', message: { role: 'system', content: 'you are…' } },
        { id: 'e4', type: 'message', message: { role: 'user', content: 'hi' } }
      ]
    })

    const result = await transcript('acme', 'ceo')

    expect(result.entries).toEqual([
      {
        type: 'message',
        id: 'e4',
        message: { role: 'user', content: [{ type: 'text', text: 'hi' }] }
      }
    ])
  })

  it('refuses 409 rather than answering an empty conversation', async () => {
    // An empty array would read as "this person has said nothing", which is a
    // different fact from "this person is not running".
    agentFor.mockResolvedValue(undefined)

    await expect(transcript('acme', 'ghost')).rejects.toMatchObject({
      status: 409,
      code: 'person-not-running'
    })
  })
})
