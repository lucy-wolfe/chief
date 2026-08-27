import { isNullish } from '@test/support/Nullish'
import companyStop, {
  chiefBinaryPath,
  type CommandRegistrar,
  CONFIRM_MESSAGE,
  CONFIRM_TITLE,
  DECLINED_MESSAGE,
  HANDED_OFF_MESSAGE,
  NO_COMPANY_REFUSAL,
  NO_UI_REFUSAL,
  ORG_DIR_ENVIRONMENT_NAME,
  runStopCommand,
  type SpawnedLike,
  STOP_COMMAND_NAME,
  type StopContext,
  type StopPlan,
  stopPlan
} from '@test-assets/company-stop'
import { expect, test } from 'vitest'

/**
 * Pi's own interactive commands, read from the installed harness.
 *
 * The list is here rather than derived because the point of the assertion is
 * that OUR name is not one of THEIRS; deriving both sides from the same place
 * would let a rename agree with itself. `/quit` heads the list because it is
 * the name a reader will reach for first and the one Pi never delegates.
 */
const PI_BUILT_IN_COMMANDS = [
  'quit',
  'arminsayshi',
  'changelog',
  'clone',
  'compact',
  'copy',
  'debug',
  'dementedelves',
  'export',
  'fork',
  'hotkeys',
  'import',
  'login',
  'logout',
  'model',
  'name',
  'new',
  'reload',
  'resume',
  'scoped-models',
  'session',
  'settings',
  'share',
  'thinking',
  'tree',
  'trust'
] as const

/** What the extension hands to `registerCommand`. */
interface CommandDefinition {
  readonly description?: string
  readonly handler: (argumentText: string, ctx: StopContext) => Promise<void>
}

/** A recording stand-in for the two Pi surfaces this extension touches. */
function harness(input: {
  readonly environment?: Record<string, string | undefined>
  readonly confirms?: boolean
  readonly hasUI?: boolean
}): {
  readonly commands: ReadonlyMap<string, CommandDefinition>
  readonly command: CommandDefinition
  readonly notices: ReadonlyArray<{ message: string; level?: string }>
  readonly asked: ReadonlyArray<{ title: string; message: string }>
  readonly spawned: readonly StopPlan[]
  readonly unreffed: readonly StopPlan[]
  readonly run: () => Promise<void>
} {
  const commands = new Map<string, CommandDefinition>()
  const notices: Array<{ message: string; level?: string }> = []
  const asked: Array<{ title: string; message: string }> = []
  const spawned: StopPlan[] = []
  const unreffed: StopPlan[] = []

  const pi: CommandRegistrar = {
    registerCommand(name, definition) {
      commands.set(name, definition)
    }
  }

  const environment = input.environment ?? { [ORG_DIR_ENVIRONMENT_NAME]: '/data/acme' }
  const spawn = (plan: StopPlan): SpawnedLike => {
    spawned.push(plan)
    return {
      unref() {
        unreffed.push(plan)
      }
    }
  }

  companyStop(pi, { environment, home: () => '/home/op', spawn })

  const command = commands.get(STOP_COMMAND_NAME)
  if (isNullish(command)) throw new Error(`no ${STOP_COMMAND_NAME} command was registered`)

  const ctx: StopContext = {
    hasUI: input.hasUI ?? true,
    ui: {
      notify(message, level) {
        notices.push({ message, level })
      },
      async confirm(title, message) {
        asked.push({ title, message })
        return input.confirms ?? false
      }
    }
  }

  return {
    commands,
    command,
    notices,
    asked,
    spawned,
    unreffed,
    run: async (): Promise<void> => {
      await runStopCommand(ctx, { environment, home: () => '/home/op', spawn })
    }
  }
}

test('the command is registered as stop, and never as one of Pi built-in names', () => {
  const { commands } = harness({})
  expect([...commands.keys()]).toStrictEqual([STOP_COMMAND_NAME])
  // The whole reason this file exists: an extension named `quit` is never
  // consulted, because Pi's interactive mode returns on the literal name
  // before it asks any extension. A rename to any built-in would ship a
  // command that silently does nothing.
  expect(PI_BUILT_IN_COMMANDS).not.toContain(STOP_COMMAND_NAME)
})

test('the description says the blast radius rather than the mechanism', () => {
  const { command } = harness({})
  expect(command.description).toBeDefined()
  expect(command.description).toContain('whole company')
})

test('a confirmed stop spawns chief stop in the company directory and unrefs it', async () => {
  const harnessed = harness({ confirms: true })
  await harnessed.run()

  expect(harnessed.spawned).toStrictEqual([
    { binary: '/home/op/.chief/bin/chief', argv: ['stop'], cwd: '/data/acme' }
  ])
  // Unref is not decoration. `chief stop` kills the tmux session this pane
  // lives in partway through its own sequence; a child still attached to this
  // process would die with us and leave the daemon running.
  expect(harnessed.unreffed).toHaveLength(1)
  expect(harnessed.notices.map((notice) => notice.message)).toContain(HANDED_OFF_MESSAGE)
})

test('the operator is asked before anything is spawned', async () => {
  const harnessed = harness({ confirms: true })
  await harnessed.run()
  expect(harnessed.asked).toStrictEqual([{ title: CONFIRM_TITLE, message: CONFIRM_MESSAGE }])
})

test('declining stops nothing and says so', async () => {
  const harnessed = harness({ confirms: false })
  await harnessed.run()

  expect(harnessed.spawned).toStrictEqual([])
  expect(harnessed.notices).toStrictEqual([{ message: DECLINED_MESSAGE, level: 'info' }])
})

test('the confirmation names everyone, because it stops everyone', () => {
  expect(CONFIRM_MESSAGE).toContain('Every person')
  // A person deciding this needs to know the durable state survives, or the
  // honest answer to "is this safe" is unavailable at the moment they must
  // answer it.
  expect(CONFIRM_MESSAGE).toContain('Nothing durable is lost')
})

test('a pane with no company stamp refuses instead of guessing one', async () => {
  const harnessed = harness({ environment: {}, confirms: true })
  await harnessed.run()

  expect(harnessed.spawned).toStrictEqual([])
  // Not even asked: there is no company to name in the question.
  expect(harnessed.asked).toStrictEqual([])
  expect(harnessed.notices).toStrictEqual([{ message: NO_COMPANY_REFUSAL, level: 'error' }])
})

test('a blank company stamp is treated as no stamp', async () => {
  const harnessed = harness({
    environment: { [ORG_DIR_ENVIRONMENT_NAME]: '   ' },
    confirms: true
  })
  await harnessed.run()
  expect(harnessed.spawned).toStrictEqual([])
  expect(harnessed.notices).toStrictEqual([{ message: NO_COMPANY_REFUSAL, level: 'error' }])
})

test('a session with no dialog refuses rather than stopping unasked', async () => {
  const harnessed = harness({ hasUI: false, confirms: true })
  await harnessed.run()

  expect(harnessed.spawned).toStrictEqual([])
  expect(harnessed.notices).toStrictEqual([{ message: NO_UI_REFUSAL, level: 'error' }])
})

test('the registered handler runs the same body, so registration is not a second path', async () => {
  const commands = new Map<string, CommandDefinition>()
  const pi: CommandRegistrar = {
    registerCommand(name, definition) {
      commands.set(name, definition)
    }
  }
  const spawned: StopPlan[] = []
  companyStop(pi, {
    environment: { [ORG_DIR_ENVIRONMENT_NAME]: '/data/acme' },
    home: () => '/home/op',
    spawn: (plan) => {
      spawned.push(plan)
      return { unref() {} }
    }
  })
  const command = commands.get(STOP_COMMAND_NAME)
  if (isNullish(command)) throw new Error('no command was registered')

  const notices: string[] = []
  const ctx: StopContext = {
    hasUI: true,
    ui: {
      notify(message) {
        notices.push(message)
      },
      confirm: async () => true
    }
  }
  // The handler's `ctx` is Pi's own, which is structurally a `StopContext`.
  await command.handler('anything at all', ctx)

  expect(spawned).toStrictEqual([
    { binary: '/home/op/.chief/bin/chief', argv: ['stop'], cwd: '/data/acme' }
  ])
  expect(notices).toContain(HANDED_OFF_MESSAGE)
})

test('stopPlan returns the refusal text rather than throwing', () => {
  expect(stopPlan({}, '/home/op')).toBe(NO_COMPANY_REFUSAL)
  expect(stopPlan({ [ORG_DIR_ENVIRONMENT_NAME]: '/data/acme' }, '/home/op')).toStrictEqual({
    binary: '/home/op/.chief/bin/chief',
    argv: ['stop'],
    cwd: '/data/acme'
  })
})

test('the binary is the installed one, with no PATH lookup behind it', () => {
  // A PATH fallback could find a DIFFERENT chief and stop a DIFFERENT company,
  // which is the worst outcome this command has.
  expect(chiefBinaryPath('/home/op')).toBe('/home/op/.chief/bin/chief')
  expect(chiefBinaryPath('/root')).toBe('/root/.chief/bin/chief')
})

/**
 * **A `/stop` THAT DID NOT LAUNCH MUST SAY SO.**
 *
 * The teardown itself is correct — traced end to end and observed completing in
 * 200ms. What could fail invisibly was the DELIVERY: a detached spawn with
 * `stdio: "ignore"` produces no log line, no event and no notification when the
 * binary is missing, the permission is denied, or the child dies at birth. An
 * invoked `/stop` that did nothing was indistinguishable from one that was
 * never invoked.
 *
 * The operator hit exactly that: four deploy attempts in an afternoon, ending
 * with them reaching for `pkill -f "chief"` — a command that would match any
 * process with "chief" anywhere in its argv, second companies included. That
 * blast radius is the measure of how badly the silence failed them.
 *
 * Same defect class as the fence key, the ownership `socketName`, and the click
 * that logged a navigation it never made: code that works but never reports
 * when it doesn't.
 */
test('a stop whose spawn throws tells the operator, and never claims it was handed off', async () => {
  const notices: Array<{ message: string; level?: string }> = []
  const ctx: StopContext = {
    hasUI: true,
    ui: {
      notify(message, level) {
        notices.push({ message, level })
      },
      confirm: async () => true
    }
  }

  await runStopCommand(ctx, {
    environment: { [ORG_DIR_ENVIRONMENT_NAME]: '/data/acme' },
    home: () => '/home/op',
    spawn: () => {
      throw new Error('spawn chief ENOENT')
    }
  })

  expect(notices.map((notice) => notice.level)).toStrictEqual(['error'])
  expect(notices[0]?.message).toContain('FAILED TO LAUNCH')
  // tmux's — or the OS's — own words, not a generic sentence.
  expect(notices[0]?.message).toContain('spawn chief ENOENT')
  // AND NEVER THE SUCCESS LINE. This is the assertion that is red on the old
  // code: it notified "Stopping the company" BEFORE spawning, so a spawn that
  // threw left the operator told a stop was under way that never started.
  expect(notices.some((notice) => notice.message === HANDED_OFF_MESSAGE)).toBe(false)
})

test('a stop that launches and then dies early tells the operator', async () => {
  const notices: Array<{ message: string; level?: string }> = []
  const handlers = new Map<string, (payload: unknown) => void>()
  const ctx: StopContext = {
    hasUI: true,
    ui: {
      notify(message, level) {
        notices.push({ message, level })
      },
      confirm: async () => true
    }
  }

  await runStopCommand(ctx, {
    environment: { [ORG_DIR_ENVIRONMENT_NAME]: '/data/acme' },
    home: () => '/home/op',
    spawn: () => ({
      unref() {},
      on(event, handler) {
        handlers.set(event, handler)
      }
    })
  })
  handlers.get('exit')?.(3)

  expect(notices.some((notice) => notice.message === HANDED_OFF_MESSAGE)).toBe(true)
  const failure = notices.find((notice) => notice.level === 'error')
  expect(failure?.message).toContain('status 3')
  expect(failure?.message).toContain('may still be running')
})

test('a clean exit says nothing, because the teardown kills the pane that would hear it', async () => {
  const notices: Array<{ message: string; level?: string }> = []
  const handlers = new Map<string, (payload: unknown) => void>()
  const ctx: StopContext = {
    hasUI: true,
    ui: {
      notify(message, level) {
        notices.push({ message, level })
      },
      confirm: async () => true
    }
  }

  await runStopCommand(ctx, {
    environment: { [ORG_DIR_ENVIRONMENT_NAME]: '/data/acme' },
    home: () => '/home/op',
    spawn: () => ({
      unref() {},
      on(event, handler) {
        handlers.set(event, handler)
      }
    })
  })
  handlers.get('exit')?.(0)

  // NON-VACUITY in the other direction: a receipt that reported success as
  // loudly as failure would be noise on the one path that always happens.
  expect(notices).toStrictEqual([{ message: HANDED_OFF_MESSAGE, level: 'info' }])
})
