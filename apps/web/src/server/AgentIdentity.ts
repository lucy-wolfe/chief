/**
 * Who a hosted agent is.
 *
 * # The gap this closes
 *
 * chiefd's launch profile carries `displayName` — "<company> · <title>" — and
 * materializes an `AGENTS.md` into the person's own workspace carrying their
 * mandate, their department, and how the company works. The harness was
 * constructed with neither, so a hosted agent ran with Pi's default system
 * prompt and no idea it was anybody. Asked what company it ran, the CEO of
 * `webproof-labs` answered: "I don't run any company — I'm Claude, an AI
 * assistant created by Anthropic, to be helpful, harmless, and honest."
 *
 * That is not a cosmetic gap. `AGENTS.md` is where the company's own
 * instructions live; an agent without it is not the person chiefd staffed.
 *
 * # Two facts, and nothing invented
 *
 * The prompt is chiefd's `displayName` plus the workspace's own `AGENTS.md`,
 * and nothing else.
 *
 * Pi's `buildSystemPrompt` — the one taking `{cwd, selectedTools,
 * contextFiles}` — would be the obvious thing to reuse, and it cannot be
 * reached: it lives in `@earendil-works/pi-coding-agent`'s
 * `dist/core/system-prompt.js`, and that package's `exports` map publishes
 * only `.` (whose `index.d.ts` does not re-export it) and `./rpc-entry`.
 * `@earendil-works/pi-agent-core` does export a `system-prompt` module, which
 * is a different file containing only `formatSkillsForSystemPrompt`. Both were
 * checked; neither reaches it.
 *
 * It is also not needed. That builder exists to describe TOOLS to the model,
 * and a tool's name, description and schema already travel with it on every
 * request. Restating them here would be a second copy of Pi's tool guidance,
 * drifting from the day it was written.
 *
 * So this composes exactly what nothing else can supply: which person this is,
 * and the document the company wrote for them.
 */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

/** The file chiefd materializes into a person's workspace. */
const CONTEXT_FILE = 'AGENTS.md'

/**
 * The person's own context file, when chiefd has materialized one.
 *
 * Absent is ordinary rather than fatal: a person materialized by an older
 * build has no `AGENTS.md`, and refusing to host them over it would take a
 * working agent off the air for a missing document.
 */
async function contextFiles(cwd: string): Promise<{ path: string; content: string }[]> {
  const path = join(cwd, CONTEXT_FILE)
  try {
    return [{ path, content: await readFile(path, 'utf8') }]
  } catch {
    return []
  }
}

/**
 * The system prompt for one person.
 *
 * `displayName` is stated as an identity line rather than left implicit in the
 * context file: `AGENTS.md` describes the company and the role, and this says
 * WHICH person reading it is. The two together are what makes an answer to
 * "who are you" correct.
 */
export async function systemPromptFor(options: {
  cwd: string
  displayName: string
}): Promise<string> {
  const identity =
    `You are ${options.displayName}. That is your identity in this company: ` +
    'answer as that person, not as a general-purpose assistant. Your working ' +
    `directory is ${options.cwd}.`
  const context = await contextFiles(options.cwd)
  if (context.length === 0) return identity
  return context
    .map(
      (file) =>
        `The following is ${CONTEXT_FILE} from your workspace (${file.path}). ` +
        `Treat its instructions as your own.\n\n${file.content}`
    )
    .reduce((prompt, section) => `${prompt}\n\n${section}`, identity)
}
