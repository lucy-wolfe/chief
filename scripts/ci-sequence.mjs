// #930: a merger's gate matrix is a PREDICTION of CI, and a prediction must
// be derived from the thing it predicts, never transcribed from a separate
// document. On 2026-08-06 the merger's own driver was built from the
// ENGINEER-BRIEF's description of what a SEAT should run rather than from
// `.github/workflows/ci.yml` itself, and drifted from CI in three ways for
// nine landings before self-correcting: wrong leg order (cargo test before
// the release build existed), a missing `CI` env var (the lenient branch of
// `chiefdBinaryTestGate` ran where the strict one should have), and `CI=1`
// alone without CI's own binary-provisioning steps (a fourth configuration
// nothing runs). This file is the derivation: it reads the ACTUAL job
// definitions out of `ci.yml` — step order, `needs:` job dependencies, and
// `run:`/`uses:` content — as structured data, so any driver (a merger's,
// an engineer's, a future guard's) can assert against the real sequence
// instead of a remembered one.
//
// Regex-based structural extraction, matching this repo's own convention
// (`scripts/test/ci-workspace-state.test.mjs`'s `yamlTopLevelBlock`) rather
// than a full YAML parser: `ci.yml`'s job/step shape is simple and
// consistently indented, and a targeted extractor is auditable in a way a
// general parser's edge cases are not. This is a `git grep`-tier structure,
// not the kind that hides a false positive for THIS file.

/**
 * The text of one top-level job block (2-space indented key under `jobs:`),
 * from its `<jobName>:` line up to (not including) the next job at the same
 * indentation, or end of file.
 */
export function jobBlock(workflowText, jobName) {
  const jobHeaderRe = new RegExp(`^  ${jobName}:\\s*$`, 'm')
  const match = jobHeaderRe.exec(workflowText)
  if (!match) return undefined
  const start = match.index
  const rest = workflowText.slice(start + match[0].length)
  const nextJob = rest.search(/^  [a-zA-Z0-9_-]+:\s*$/m)
  return rest.slice(0, nextJob === -1 ? rest.length : nextJob)
}

/**
 * The `needs: [...]` or `needs:\n  - x\n  - y` array for a job, as an
 * ordered array of job names. Empty array if the job has no `needs:` key
 * (a real fact — not every job depends on another) or the job itself is not
 * found (callers that care about "job exists" should check `jobBlock`
 * first; this function alone cannot distinguish "no needs" from "no job").
 */
export function jobNeeds(workflowText, jobName) {
  const block = jobBlock(workflowText, jobName)
  if (!block) return []
  const inline = /^\s*needs:\s*\[([^\]]*)\]/m.exec(block)
  if (inline) {
    return inline[1]
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean)
  }
  const multiline = /^\s*needs:\s*\n((?:\s+-\s*\S+\n?)+)/m.exec(block)
  if (multiline) {
    return [...multiline[1].matchAll(/-\s*(\S+)/g)].map((m) => m[1])
  }
  return []
}

/**
 * A job's `strategy.matrix.include:` entries, in file order, each as a plain
 * `{ key: value }` map of its own scalar keys with the raw trimmed text on
 * the right of each colon. `[]` when the job has no `include:` list (or does
 * not exist — same convention as `jobNeeds`).
 *
 * A matrix entry is the run set of one matrix leg, so a guard that wants to
 * assert what a leg RUNS has to read the entry rather than grep the job
 * block for a literal line. Matching text inside a job block also matches
 * across entry boundaries: the assertion this replaced was an adjacency of
 * two target names inside one `parallel_targets:` value, which a legitimate
 * new target in between silently broke.
 *
 * The entry indent is taken from the first `- key:` line rather than a
 * hardcoded column, so a reindent of ci.yml does not quietly return an empty
 * list. Deeper `- ` lines (a nested sequence under one of an entry's keys)
 * are skipped rather than read as new entries.
 */
export function matrixIncludes(workflowText, jobName) {
  const block = jobBlock(workflowText, jobName)
  if (!block) return []
  const header = /^( *)include: *$/m.exec(block)
  if (!header) return []
  const headerIndent = header[1].length
  const body = block.slice(header.index + header[0].length)

  const entries = []
  let entryIndent
  let current
  for (const line of body.split('\n')) {
    const trimmed = line.trim()
    if (trimmed.length === 0 || trimmed.startsWith('#')) continue
    const indent = line.length - line.replace(/^ */, '').length
    if (indent <= headerIndent) break
    const entryStart = /^ *- +([A-Za-z0-9_-]+): *(.*)$/.exec(line)
    if (entryStart) {
      if (entryIndent === undefined) entryIndent = indent
      if (indent !== entryIndent) continue
      current = { [entryStart[1]]: entryStart[2].trim() }
      entries.push(current)
      continue
    }
    const pair = /^ *([A-Za-z0-9_-]+): *(.*)$/.exec(line)
    if (pair && current && indent === entryIndent + 2) {
      current[pair[1]] = pair[2].trim()
    }
  }
  return entries
}

/**
 * Every step in a job's `steps:` list, in the ORDER they appear (the whole
 * point of this file — GitHub Actions runs steps top-to-bottom within a
 * job, so order here IS execution order). Each step is
 * `{ name, run, uses }` — `name` is the step's own `name:` field (or
 * `undefined` for an unnamed step), `run` is its `run:` command text
 * (single-line or the joined text of a `run: |` block), `uses` is its
 * `uses:` action reference. A step has `run` XOR `uses` in practice, never
 * both, but both are read independently rather than assumed exclusive.
 */
export function jobSteps(workflowText, jobName) {
  const block = jobBlock(workflowText, jobName)
  if (!block) return []
  const stepsHeaderRe = /^\s*steps:\s*$/m
  const stepsMatch = stepsHeaderRe.exec(block)
  if (!stepsMatch) return []
  const stepsText = block.slice(stepsMatch.index + stepsMatch[0].length)

  const steps = []
  // Each step starts with a line matching `      - name: ...` or
  // `      - uses: ...` at the steps list's own indentation (6 spaces is
  // this file's convention throughout ci.yml; matched structurally via the
  // dash rather than a hardcoded column count so a reindent does not
  // silently break this).
  const stepStartRe = /^(\s*)- (name|uses):/gm
  const starts = [...stepsText.matchAll(stepStartRe)]
  for (let i = 0; i < starts.length; i += 1) {
    const indent = starts[i][1]
    const start = starts[i].index
    const end = i + 1 < starts.length ? starts[i + 1].index : stepsText.length
    const stepText = stepsText.slice(start, end)
    const name = /^\s*- name:\s*(.+)$/m.exec(stepText)?.[1]?.trim()
    const uses = /^\s*uses:\s*(.+)$/m.exec(stepText)?.[1]?.trim()
    // `run:` is either a single-line command or a `run: |`/`run: >` block;
    // for a block, take every subsequent line indented deeper than the
    // step's own indent, up to the next step-level key or the next step.
    let run
    const inlineRun = new RegExp(`^${indent}  run:\\s*(.+)$`, 'm').exec(stepText)
    if (inlineRun && !inlineRun[1].trim().startsWith('|') && !inlineRun[1].trim().startsWith('>')) {
      run = inlineRun[1].trim()
    } else {
      const blockRunRe = new RegExp(`^${indent}  run:\\s*[|>][-+]?\\s*\\n((?:${indent}    .*\\n?)+)`, 'm')
      const blockMatch = blockRunRe.exec(stepText)
      if (blockMatch) run = blockMatch[1]
    }
    steps.push({ name, run, uses })
  }
  return steps
}

/**
 * The index of the FIRST step whose `name` or `run`/`uses` text contains
 * `needle` (case-sensitive substring match — deliberately not a regex, so a
 * caller matching a step by its literal name string cannot accidentally
 * miss it to a regex metacharacter). `-1` if no step matches, mirroring
 * `Array.prototype.indexOf`'s own "not found" convention rather than
 * throwing, since "this step does not exist yet" is itself a real,
 * checkable fact for a caller asserting an ordering.
 */
export function stepIndex(steps, needle) {
  return steps.findIndex(
    (step) => step.name?.includes(needle) || step.run?.includes(needle) || step.uses?.includes(needle)
  )
}
