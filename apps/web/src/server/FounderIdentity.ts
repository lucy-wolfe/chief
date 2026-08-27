/**
 * Who the Founder is — read from the ONE place the Founder is defined.
 *
 * # There is one Founder, and therefore one copy of its instructions
 *
 * The Founder's persona is a Pi skill: `packages/piing/skills/founder/
 * SKILL.md`. The tmux Founder loads it through `pi --skill`
 * (`chief-cli/src/founder_pi.rs`). This server has no skill loader — it builds
 * an `AgentHarness` directly and hands it a `systemPrompt` string — so it reads
 * the same file itself, at runtime.
 *
 * An earlier version of this module RESTATED the skill body as a TypeScript
 * constant and pinned it to the file with a test. That was wrong and is
 * deleted: a second copy of a prompt is the same defect class as a second copy
 * of a constant, and a pinning test only tells you the copies have diverged
 * after somebody has already edited one. There is no second copy now, so there
 * is nothing to diverge.
 *
 * # Why the file is reachable, and why that is not a monorepo assumption
 *
 * `@chief/piing` ships `skills` in its `files` array and now EXPORTS that tree
 * (`"./skills/*"`), so the skill is addressable as a package subpath rather
 * than as a directory somebody happens to know the shape of. The path is
 * resolved THROUGH the package rather than composed from this app's own
 * directory, so it is correct in the workspace, in a published install, and in
 * a standalone build — wherever Node can resolve `@chief/piing`, this finds
 * its skills.
 *
 * Getting there cost two live failures, both worth recording because both are
 * invisible to a unit test:
 *
 *  - resolving a NEIGHBOURING module and stepping up two directories failed on
 *    a build host with "Package subpath './extensions/founder-launch' is not
 *    defined by exports" — that entry declares only the `import` condition and
 *    `createRequire().resolve` asks for `require`. Exporting the skills tree is
 *    the honest fix, and it removed the directory arithmetic with it;
 *  - and then package resolution failed under `next dev` REGARDLESS of anchor,
 *    with "Cannot find module '@chief/piing/skills/founder/SKILL.md'"
 *    from two anchors that both resolve it correctly under plain Node. The
 *    cause is not Node: `@chief/piing` is in `transpilePackages`, so the
 *    bundler owns requires of it and its resolver does not consult the exports
 *    map for a non-JS subpath. Verified by resolving the identical specifier
 *    with `node -e` from the identical anchor and getting the real path back.
 *
 * So there are TWO strategies, tried in order, and they are different KINDS of
 * answer rather than two goes at one:
 *
 *  1. ask the package, through its exports map. Correct anywhere Node's own
 *     resolver is what runs — Vitest, a plain Node server, a published install
 *     — and it is tried first because it is the answer that does not care
 *     where anything is;
 *  2. read it beside this module, through `new URL(..., import.meta.url)`.
 *     This is the one path composed by hand and it is confined to this file.
 *     It works under the bundler because `import.meta.url` IS this source
 *     file's URL there (the failure above proved that: it reported this exact
 *     path). It assumes apps/web sits in the checkout — which this app already
 *     assumes structurally, since `ExtensionTools` imports piing extension
 *     SOURCE and `next.config.ts` sets `outputFileTracingRoot` to the repo
 *     root, and which chiefd requires independently: it refuses
 *     `launcher-root-unusable` without that same checkout.
 *
 * Both land on the same file. If they ever disagree, the first one wins and
 * the second is dead weight rather than a second definition.
 *
 * # A skill this server cannot read is a REFUSAL, never a default
 *
 * If the file is missing or empty, `founderSystemPrompt` throws. Falling back
 * to some built-in sentence would produce an agent that answers as Founder,
 * launches real companies, and is not the Founder anybody wrote — the exact
 * "looks healthy, is not the person chiefd staffed" failure `AgentIdentity`'s
 * header describes, with a durable side effect attached.
 */
import { accessSync, constants, readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

/** The skill whose body IS the Founder's instructions. */
const FOUNDER_SKILL = 'founder'

/** The skill file, as a `@chief/piing` package subpath. */
const FOUNDER_SKILL_SUBPATH = `@chief/piing/skills/${FOUNDER_SKILL}/SKILL.md`

/** The same file, relative to THIS module's source location. See strategy 2 in
 * the module header. `apps/web/src/server` -> repo root is four levels. */
const FOUNDER_SKILL_RELATIVE = `../../../../packages/piing/skills/${FOUNDER_SKILL}/SKILL.md`

/**
 * Strip a skill file's YAML front matter, leaving the instructions.
 *
 * Pi reads `name`/`description` out of that header for its skill listing; they
 * are metadata about the skill, not instruction to the model, and passing them
 * through would put `name: founder` in a system prompt.
 */
function skillBody(source: string): string {
  const match = /^---\n[\s\S]*?\n---\n([\s\S]*)$/.exec(source)
  return (match?.[1] ?? source).trim()
}

/** Strategy 1: ask the package. `createRequire` resolves relative to a FILE,
 * so the anchor is a filename in the app root rather than the directory. */
function throughPackage(): string {
  return createRequire(pathToFileURL(join(process.cwd(), 'noop.js')).href).resolve(
    FOUNDER_SKILL_SUBPATH
  )
}

/** Strategy 2: read it beside this module. See the module header. */
function besideThisModule(): string {
  const path = fileURLToPath(new URL(FOUNDER_SKILL_RELATIVE, import.meta.url))
  // Probed rather than returned blind: an unreadable path here must fall out
  // as "no strategy worked", naming both, instead of as an ENOENT that reads
  // like the file was deleted.
  accessSync(path, constants.R_OK)
  return path
}

/** Where `SKILL.md` is. */
function skillPath(): string {
  const failures: string[] = []
  for (const strategy of [throughPackage, besideThisModule]) {
    try {
      return strategy()
    } catch (error) {
      failures.push(`${strategy.name}: ${error instanceof Error ? error.message : String(error)}`)
    }
  }
  throw new Error(failures.join('; '))
}

/** Read once. The file cannot change under a running server without a restart,
 * and re-reading it per turn would put a disk read in the hot path of every
 * message for a value that is constant for the life of the process. */
let cached: string | undefined

/**
 * The founder skill's instructions, as written.
 *
 * @throws when the skill cannot be read or is empty — see the module header.
 */
export function founderSkillBody(): string {
  if (typeof cached === 'string') return cached
  let path: string
  try {
    path = skillPath()
  } catch (error) {
    throw new Error(
      `Founder Mode cannot locate the "${FOUNDER_SKILL}" skill: @chief/piing did not resolve ` +
        `by any strategy (${error instanceof Error ? error.message : String(error)}). The ` +
        "Founder's instructions have exactly one definition and this server will not invent a " +
        'second one.',
      { cause: error }
    )
  }
  let body: string
  try {
    body = skillBody(readFileSync(path, 'utf8'))
  } catch (error) {
    throw new Error(
      `Founder Mode cannot read its own instructions at ${path}: ` +
        `${error instanceof Error ? error.message : String(error)}. Refusing rather than running ` +
        'a Founder that is not the one anybody wrote.',
      { cause: error }
    )
  }
  if (body === '') {
    throw new Error(
      `Founder Mode read its instructions at ${path} and they are empty. Refusing rather than ` +
        'running a Founder with no instructions, which would create companies on nothing.'
    )
  }
  cached = body
  return body
}

/**
 * The one thing this surface adds, and only because it is TRUE HERE AND FALSE
 * THERE.
 *
 * The tmux Founder is granted the seven coding tools and told by its skill not
 * to use them, because it runs as a Pi child in a checkout. This one runs
 * inside the web server process and is granted exactly one tool, so there is
 * nothing to refrain from — and a model left to infer why a shell it never had
 * is missing will offer it anyway.
 *
 * It is deliberately about the SURFACE and never about the task: nothing here
 * tells the Founder what to do, which is the skill's job and the skill's alone.
 */
const HOSTED_NOTE =
  'You are running inside the Chief web app rather than a terminal, and ' +
  '`chiefd_launch_company` is the only tool you have. You have no shell, no ' +
  'file access and no company-management tools, so do not offer them.'

/** The Founder's system prompt. */
export function founderSystemPrompt(): string {
  return `${founderSkillBody()}\n\n${HOSTED_NOTE}`
}

/** Drop the cached skill text. For tests, which need to observe a real read
 * rather than a value some earlier test warmed. */
export function resetFounderIdentity(): void {
  cached = undefined
}
