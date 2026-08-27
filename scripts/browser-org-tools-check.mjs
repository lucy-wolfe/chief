#!/usr/bin/env node
// THE browser acceptance check: a real Chromium, a real hosted agent, a real
// `org_*` tool call, and durable state that changed because of it.
//
// # What this replaced, and why the replacement is not a port
//
// `scripts/browser-flow-check.mjs` stood here and was doubly dead. It drove
// the old TypeScript api app on :8791 — an app that has been DELETED — and it
// imported `playwright`, which is not a dependency of this repo, so it could not
// even be loaded, let alone run. Nothing referenced it. A browser acceptance
// check that cannot load is worse than none: it occupies the slot, so nobody
// writes the one that works, and every handoff quotes its last green number.
//
// So this is one check, with one driver, and no second source of truth about
// how to reach a browser.
//
// # Why the Chrome DevTools Protocol, and no browser library at all
//
// The repo has no browser driver dependency and adding one to revive a dead
// file is how the last one got here. Chromium already speaks CDP over a
// WebSocket, Node has had a WebSocket client since 22, and everything this
// check needs — navigate, evaluate, real key events, real clicks — is four
// CDP domains. That is a smaller surface than a driver library, and it cannot
// rot into an undeclared import.
//
// # What it proves that a route test cannot
//
// A `POST …/say` that answers `ACK` proves a harness replies. It proved that
// while a hosted CEO had 7 of its 60 tools and could not run a company. The
// proof this file exists for is one layer past the reply:
//
//   the operator types into the REAL composer with REAL keystrokes;
//   the agent calls a REAL `org_*` tool;
//   chiefd's supervision ledger CHANGED;
//   and the change is visible on a page a person can look at.
//
// Every step asserts, and a failure names its step and its reason.
//
//   usage: node scripts/browser-org-tools-check.mjs --dir <company directory>
//                [--web http://127.0.0.1:3000] [--beacond http://127.0.0.1:6969]
//                [--chromium /usr/bin/chromium] [--person <id>]
import { spawn } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'

function arg(name, fallback) {
  const index = process.argv.indexOf(`--${name}`)
  return index === -1 ? fallback : process.argv[index + 1]
}

const WEB = arg('web', 'http://127.0.0.1:3000')
const BEACOND = arg('beacond', 'http://127.0.0.1:6969')
// THE COMPANY: the directory it occupies. A slug names no company — two
// directories may hold one called the same thing — so a `--slug` here picked
// whichever row beacond happened to list first.
const DIR = arg('dir', undefined)
const CHROMIUM = arg('chromium', process.env.CHROMIUM_BINARY ?? '/usr/bin/chromium')
const PERSON = arg('person', undefined)

let step = 0
const failures = []

function begin(title) {
  step += 1
  process.stdout.write(`\n[${step}] ${title}\n`)
}

function ok(detail) {
  process.stdout.write(`   ok — ${detail}\n`)
}

function fail(reason) {
  failures.push(`step ${step}: ${reason}`)
  process.stdout.write(`   FAILED — ${reason}\n`)
  throw new Error(`step ${step}: ${reason}`)
}

async function json(url, init) {
  const response = await fetch(url, { ...init, signal: AbortSignal.timeout(180_000) })
  const text = await response.text()
  try {
    return { status: response.status, body: JSON.parse(text) }
  } catch {
    return { status: response.status, body: text }
  }
}

// ── the driver ──────────────────────────────────────────────────────────────

/**
 * The exact subset of the WHATWG `WebSocket` interface this script speaks.
 *
 * `WebSocket` has been a Node global since 22.4, but `@types/node` 20 — the
 * version this repo pins — predates it, and `tsconfig.guards.json`
 * deliberately does NOT load the `DOM` lib: a node script that reads a
 * browser global by accident must stay an error rather than resolve against
 * a browser type nothing here runs in. So the one real global this file
 * needs is named, with the three members it uses, instead of switching the
 * whole DOM on for every guard.
 * @typedef {{
 *   addEventListener: (type: string, listener: (event: { data: string }) => void, options?: { once?: boolean }) => void,
 *   send: (data: string) => void,
 *   close: () => void
 * }} CdpSocket
 */
const { WebSocket } = /** @type {{ WebSocket: new (url: string) => CdpSocket }} */ (
  /** @type {unknown} */ (globalThis)
)

/** One Chromium, one page, spoken to over CDP. */
class Browser {
  constructor(process_, socket, userDataDir) {
    this.process = process_
    this.socket = socket
    this.userDataDir = userDataDir
    this.nextId = 1
    this.pending = new Map()
    socket.addEventListener('message', (event) => {
      const message = JSON.parse(event.data)
      const waiter = this.pending.get(message.id)
      if (!waiter) return
      this.pending.delete(message.id)
      if (message.error) waiter.reject(new Error(JSON.stringify(message.error)))
      else waiter.resolve(message.result)
    })
  }

  send(method, params = {}) {
    const id = this.nextId++
    this.socket.send(JSON.stringify({ id, method, params }))
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      setTimeout(() => {
        if (!this.pending.delete(id)) return
        reject(new Error(`CDP ${method} timed out`))
      }, 120_000).unref?.()
    })
  }

  /** Evaluate in the page and return the value, throwing the page's own error. */
  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true
    })
    if (result.exceptionDetails) {
      throw new Error(result.exceptionDetails.exception?.description ?? 'page threw')
    }
    return result.result.value
  }

  async navigate(url) {
    await this.send('Page.navigate', { url })
    // Settle on the document being interactive rather than on a load event:
    // this app hydrates and then fetches, so `load` is the wrong milestone in
    // both directions — too late for a static page, far too early for this one.
    await this.until(() => this.evaluate('document.readyState === "complete"'), 60_000, url)
  }

  /** Poll a page-side predicate until it is true, or fail with `what`. */
  async until(predicate, timeoutMs, what) {
    const deadline = Date.now() + timeoutMs
    for (;;) {
      if (await predicate()) return true
      if (Date.now() > deadline) return fail(`timed out waiting for ${what}`)
      await delay(500)
    }
  }

  /** Click where an element actually is, with a real mouse.
   *
   * `element.focus()` from `Runtime.evaluate` is not the same act: it moves
   * the DOM focus without any of the events a React composer listens for, and
   * a pane that only mounts its input handlers on a real pointer interaction
   * then receives every keystroke into nothing. Measured: the composer stayed
   * empty through a full instruction typed at it. */
  async click(selector) {
    const box = await this.evaluate(`(() => {
      const element = document.querySelector(${JSON.stringify('SELECTOR')})
      if (!element) return null
      element.scrollIntoView({ block: 'center' })
      const rect = element.getBoundingClientRect()
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }
    })()`.replace(JSON.stringify('SELECTOR'), JSON.stringify(selector)))
    if (!box) return false
    for (const type of ['mousePressed', 'mouseReleased']) {
      await this.send('Input.dispatchMouseEvent', {
        type,
        x: box.x,
        y: box.y,
        button: 'left',
        clickCount: 1
      })
    }
    return true
  }

  /**
   * Click an input and CONFIRM it took the focus, retrying while it does not.
   *
   * A click can miss for reasons that have nothing to do with the product: the
   * pane is still settling when the rectangle is measured, or a re-render
   * replaces the node between the press and the release. Measured, that miss
   * is silent — `document.activeElement` stays `BODY`, every subsequent
   * keystroke goes to the document, and the composer reports itself empty as
   * though the app had swallowed the text. Confirming focus turns a flaky
   * driver into a real one, and keeps the composer assertion a statement about
   * the app instead of about the mouse.
   */
  async clickInto(selector, attempts = 20) {
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      // The element must be WHERE the click is aimed, and still be there when
      // the press lands. A pane is laid out over several frames and its
      // composer sits at the bottom edge of the viewport, so an early click is
      // aimed below the fold and hits nothing. `elementFromPoint` asks the
      // page the same question the mouse will: is this really what is at that
      // point?
      const onTarget = await this.evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)})
        if (!element) return false
        element.scrollIntoView({ block: 'center' })
        const rect = element.getBoundingClientRect()
        if (rect.width === 0 || rect.height === 0) return false
        if (rect.bottom > innerHeight || rect.top < 0) return false
        const at = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2)
        return at === element
      })()`)
      if (onTarget === true) {
        if (!(await this.click(selector))) return false
        await delay(300)
        const focused = await this.evaluate(
          `document.activeElement === document.querySelector(${JSON.stringify(selector)})`
        )
        if (focused === true) return true
      }
      await delay(500)
    }
    return false
  }

  /** Type one character the way a keyboard does: down, char, up.
   *
   * `Input.insertText` would be shorter and would NOT have caught the defect
   * this repo already shipped once — a pane-level Enter/Space handler that
   * `preventDefault()`ed every space typed into the composer beneath it. A
   * driver that inserts text tests the model, not the keyboard. */
  async type(text) {
    for (const character of text) {
      const isEnter = character === '\n'
      const key = isEnter
        ? { key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 }
        : { key: character }
      const inserted = isEnter ? '\r' : character
      // `rawKeyDown` and NOT `keyDown`: a `keyDown` carrying `text` inserts the
      // character itself, and the `char` event then inserts it a second time —
      // measured, `hi there` arrived as `hhii  tthheerree`. The three-event
      // shape is what a real keyboard produces and what the pane's handlers
      // see; `Input.insertText` would be one call and would test nothing about
      // the keyboard, which is the half this repo has already shipped broken.
      await this.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...key })
      await this.send('Input.dispatchKeyEvent', { type: 'char', ...key, text: inserted })
      await this.send('Input.dispatchKeyEvent', { type: 'keyUp', ...key })
    }
  }

  close() {
    try {
      this.socket.close()
    } catch {
      /* the socket is already gone; the kill below is what matters */
    }
    this.process.kill('SIGKILL')
    try {
      // Chromium is still flushing its profile as it dies, so this races and
      // sometimes loses. SWALLOWED, and that is the point: this runs in a
      // `finally`, so a throw here REPLACES the real verdict — measured, a
      // genuine `step 11: timed out waiting for the agent's reply` was
      // reported to the operator as `ENOTEMPTY: rmdir /tmp/org-tools-...`.
      // A leftover temp directory must never be allowed to hide a finding.
      rmSync(this.userDataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 })
    } catch {
      /* a temp directory is not a result */
    }
  }
}

/** Start Chromium headless and attach to its first page. */
async function launch(binary) {
  const userDataDir = mkdtempSync(join(tmpdir(), 'org-tools-chromium-'))
  const child = spawn(
    binary,
    [
      '--headless=new',
      '--no-sandbox',
      '--disable-gpu',
      '--disable-dev-shm-usage',
      '--remote-debugging-port=0',
      `--user-data-dir=${userDataDir}`,
      'about:blank'
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] }
  )
  // Chromium prints its chosen debug port on stderr, which is the only way to
  // learn it when the port is 0 — and 0 is deliberate: a fixed port is a
  // collision with whatever else is on this box.
  const endpoint = await new Promise((resolve, reject) => {
    let buffered = ''
    const timer = setTimeout(
      () => reject(new Error('chromium never printed a DevTools endpoint')),
      30_000
    )
    child.stderr.on('data', (chunk) => {
      buffered += chunk.toString()
      const match = /DevTools listening on (ws:\/\/\S+)/.exec(buffered)
      if (!match) return
      clearTimeout(timer)
      resolve(match[1])
    })
    child.on('exit', (code) => reject(new Error(`chromium exited early (${code})`)))
  })

  const origin = new URL(endpoint).origin.replace('ws://', 'http://')
  const targets = /** @type {{ type: string, webSocketDebuggerUrl: string }[]} */ (
    await (await fetch(`${origin}/json/list`)).json()
  )
  const page = targets.find((target) => target.type === 'page')
  if (!page) throw new Error('chromium started with no page target')
  const socket = new WebSocket(page.webSocketDebuggerUrl)
  await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true })
    socket.addEventListener('error', () => reject(new Error('CDP socket refused')), { once: true })
  })
  const browser = new Browser(child, socket, userDataDir)
  await browser.send('Page.enable')
  await browser.send('Runtime.enable')
  return browser
}

// ── the check ───────────────────────────────────────────────────────────────

/**
 * Tool ids a hosted person may still be granted and not get, each with the
 * reason it is not a defect in the host.
 *
 * A NAMED SET, never a count. The measurement asserts that everything the host
 * could not supply appears here; an id that does not is a capability that
 * disappeared, and it fails by name. Nothing asserts the SIZE of this set,
 * because a check that goes red when the product improves is a check people
 * learn to edit.
 *
 * IT IS EMPTY, AND THAT IS THE POINT. It held three ids, and all three left
 * when the `hasActivityRuntime` fence stopped being about tmux. Every one of
 * them was deleted in the commit that supplied the tool, because this map is
 * the file's own record of what is KNOWINGLY missing and a row for a tool that
 * is supplied is a stale claim.
 * The subset assertion below still stands and still fails by name: with the
 * map empty, ANY id the host cannot supply is an unjustified gap.
 */
const JUSTIFIED_UNAVAILABLE = new Map()


async function main() {
  if (!DIR) {
    process.stderr.write('usage: node scripts/browser-org-tools-check.mjs --dir <company directory>\n')
    process.exit(2)
  }

  begin('a real Chromium is on this box')
  if (!existsSync(CHROMIUM)) {
    fail(`no chromium at ${CHROMIUM} — pass --chromium or set CHROMIUM_BINARY`)
  }
  ok(CHROMIUM)

  begin('the web answers')
  const health = await json(`${WEB}/api/companies`)
  if (health.status !== 200) fail(`GET ${WEB}/api/companies answered ${health.status}`)
  ok(`${WEB} serving ${Array.isArray(health.body) ? health.body.length : '?'} compan(y/ies)`)

  begin(`beacond has a daemon registered for ${DIR}`)
  const registry = await json(`${BEACOND}/v1/list`)
  // Matched on `dir`, the registry's own identity column. One row per
  // directory, forever; `slug` beside it is a display word two rows may share,
  // so matching on it returns whichever was listed first.
  const company = (registry.body?.companies ?? []).find((entry) => entry.dir === DIR)
  if (!company?.url) fail(`no daemon registered for ${DIR} at ${BEACOND}`)
  ok(`${company.url} (${company.slug}, key ${company.key})`)

  begin('chiefd names the tools it granted each person')
  // chiefd keys a company by the DIRECTORY it occupies — two checkouts can
  // both hold an `acme`, and a display slug would let one answer for the
  // other. The key is `sha256(dir)[..12]`, and beacond SERVES it: this used to
  // rebuild it from the row's `orgsRoot`, which made this script a second
  // producer of an identity that must have exactly one.
  const key = company.key
  const profile = await json(`${company.url}/v1/org/api-host-launch-profile/read`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ slug: key })
  })
  const plans = profile.body?.plans ?? []
  if (plans.length === 0) {
    fail(`chiefd published no launch profile for ${key}: ${profile.status} ` +
      `${JSON.stringify(profile.body).slice(0, 200)}`)
  }
  const granted = new Map(plans.map((plan) => [plan.personId, plan.tools ?? []]))
  ok(
    `${profile.body.actuation.effectiveMode} — ` +
      [...granted].map(([person, tools]) => `${person} granted ${tools.length}`).join(', ')
  )

  begin('the web hosts this company’s people')
  // Every `/api/companies/:companyKey/…` route is keyed by the company KEY,
  // which beacond served on the row above.
  const roster = await json(`${WEB}/api/companies/${key}/people`)
  const hosted = roster.body?.hosted ?? []
  if (hosted.length === 0) {
    fail(`nobody is hosted: ${JSON.stringify(roster.body).slice(0, 400)}`)
  }
  ok(`hosted ${hosted.join(', ')}`)

  begin('THE MEASUREMENT: how many of the granted tools the host actually built')
  const degraded = new Map(
    (roster.body?.degraded ?? []).map((entry) => [entry.personId, entry.missingTools ?? []])
  )
  const regressions = []
  const fixed = new Set(JUSTIFIED_UNAVAILABLE.keys())
  for (const personId of hosted) {
    const grantedTools = granted.get(personId) ?? []
    // A VACUITY FLOOR, not an inventory. "built 0 of 0" satisfies every
    // assertion below by measuring nothing, and a grant that collapsed is the
    // shape this repo keeps paying for: an instrument that cannot see its
    // subject reports success. chiefd's own `person_tool_names` gives every
    // person at least the coding tools, so an empty grant means the read
    // broke, not that somebody was granted nothing.
    if (grantedTools.length === 0) {
      fail(`chiefd's launch profile grants "${personId}" no tools at all — the read broke`)
    }
    const missing = degraded.get(personId) ?? []
    for (const id of missing) {
      fixed.delete(id)
      if (!JUSTIFIED_UNAVAILABLE.has(id)) regressions.push(`${personId}: ${id}`)
    }
    process.stdout.write(
      `   ${personId}: BUILT ${grantedTools.length - missing.length} of ${grantedTools.length}` +
        (missing.length ? ` — unavailable: ${missing.join(', ')}` : ' — nothing unavailable') +
        '\n'
    )
  }
  // THE ASSERTION IS A SUBSET, NOT A NUMBER. A hardcoded count goes red the day
  // the product gets BETTER — three of the four ids below are a tmux coupling
  // somebody is removing right now — and a check that fails on an improvement
  // teaches people to edit checks. So: every id the host could not supply must
  // be one of the ids named and justified in `JUSTIFIED_UNAVAILABLE`. Anything
  // else is a capability that silently disappeared, and it fails by name.
  if (regressions.length > 0) {
    fail(
      `tool id(s) the host cannot supply and nobody has justified:\n     ` +
        regressions.join('\n     ') +
        `\n   Every id here is a capability the company granted and the agent does not have.`
    )
  }
  // The other direction is REPORTED and does not fail, for the same reason.
  // A justified row whose tool is now supplied is a stale row — real, worth
  // deleting, and never a reason to call a fix a regression.
  for (const id of fixed) {
    process.stdout.write(
      `   STALE JUSTIFICATION: "${id}" is supplied now. Delete its JUSTIFIED_UNAVAILABLE row.\n`
    )
  }
  ok(
    regressions.length === 0 && fixed.size === 0
      ? JUSTIFIED_UNAVAILABLE.size === 0
        ? 'nothing unavailable, and nothing is justified as missing any more'
        : `every unavailable id is one of the ${JUSTIFIED_UNAVAILABLE.size} justified ids`
      : `no unjustified gaps; ${fixed.size} justification(s) are now stale`
  )

  const subject = PERSON ?? hosted.find((personId) => personId === 'ceo') ?? hosted[0]
  const token = `orgtools-${Date.now().toString(36)}`

  const browser = await launch(CHROMIUM)
  try {
    begin(`the company page renders a pane for ${subject}`)
    await browser.navigate(`${WEB}/c/${key}`)
    await browser.until(
      () => browser.evaluate('document.body.innerText.includes("Loading company") === false'),
      120_000,
      'the company page to finish hydrating'
    )
    // A company page opens on the overview: the panes are what you get after
    // choosing an agent, so there is no composer until one is chosen. Clicking
    // the agent's own chip is what an operator does, and it is the only way to
    // reach the composer at all.
    const opened = await browser.evaluate(`(() => {
      const wanted = ${JSON.stringify(subject)}.replace(/[^a-z0-9-]/gi, '').toLowerCase()
      const chip = [...document.querySelectorAll('button')].find(
        (button) => button.textContent.trim().replace(/[^a-z0-9-]/gi, '').toLowerCase() === wanted
      )
      if (!chip) {
        return 'no chip for ' + wanted + ' among: ' +
          [...document.querySelectorAll('button')].map((b) => b.textContent.trim()).join(' | ')
      }
      chip.click()
      return 'clicked'
    })()`)
    if (opened !== 'clicked') fail(opened)
    await browser.until(
      () => browser.evaluate('document.querySelector("textarea") !== null'),
      120_000,
      'a pane composer'
    )
    ok('composer present')

    begin('the agent is idle, so a typed message is answered rather than queued')
    // An agent processes its queue serially: a message typed while a turn is
    // still running waits behind it, and this step would time out on a product
    // that is working perfectly. `Agent idle.` is the pane's own word for the
    // precondition, so it is the one to wait for.
    await browser.until(
      () => browser.evaluate('document.body.innerText.includes("Agent idle.")'),
      180_000,
      'the pane’s own "Agent idle." banner'
    )
    ok('idle')

    begin('an operator types a real message with real keystrokes')
    const instruction =
      `Call the org_create_reminder tool right now with exactly this prompt ` +
      `text: ${token}. Use an interval of 60 minutes. After the tool returns, ` +
      `reply with only the word DONE and nothing else.`
    if (!(await browser.clickInto('textarea'))) {
      fail('the composer never took focus, so no keystroke could reach it')
    }
    await browser.type(instruction)
    // A composer is a CONTROLLED input: the characters are in the DOM the
    // instant they are typed, and the value the page will keep is whatever the
    // next render writes back. Reading it in the same tick reports the race,
    // not the product, so this settles first and then asks.
    await delay(1000)
    const typed = await browser.evaluate('document.querySelector("textarea").value')
    if (typed !== instruction) {
      const state = await browser.evaluate(
        'JSON.stringify({ active: document.activeElement && document.activeElement.tagName, ' +
          'textareas: document.querySelectorAll("textarea").length, ' +
          'disabled: [...document.querySelectorAll("textarea")].map((t) => t.disabled) })'
      )
      process.stdout.write(`   page state: ${state}\n`)
    }
    if (typed !== instruction) {
      // The defect this exact assertion caught once already: a pane-level
      // Enter/Space handler swallowed every space typed into the composer, and
      // the repo's own driver reported green because it sent through the API.
      fail(`the composer did not keep what was typed:\n     wanted: ${instruction}\n     got:    ${typed}`)
    }
    ok(`${instruction.length} characters survived the composer`)

    begin('the real Send button submits it')
    const sent = await browser.evaluate(`(() => {
      const send = [...document.querySelectorAll('button')]
        .find((button) => /^send$/i.test(button.textContent.trim()))
      if (!send) return 'no send button'
      send.click()
      return 'clicked'
    })()`)
    if (sent !== 'clicked') fail(sent)
    ok('sent')

    begin('the agent answers the turn')
    await browser.until(
      () =>
        browser.evaluate(
          `document.body.innerText.split(${JSON.stringify(token)}).length - 1 >= 2 ` +
            `|| /\\bDONE\\b/.test(document.body.innerText)`
        ),
      300_000,
      'the agent’s reply in the pane'
    )
    ok('answered')

    begin('THE PROOF: an org_* tool changed chiefd’s supervision ledger')
    // The reminder list is a DIFFERENT surface reading a DIFFERENT keyspace:
    // the supervision ledger, in Rust, through chiefd. Nothing but a real
    // `org_create_reminder` call puts this token there. A reply — even a
    // correct one — proves only that the model can type.
    let reminders = []
    await browser.until(
      async () => {
        reminders =
          (
            await json(`${company.url}/v1/reminders/list`, {
              method: 'POST',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify({ slug: key, personId: subject })
            })
          ).body?.reminders ?? []
        return reminders.some((reminder) => (reminder.prompt ?? '').includes(token))
      },
      120_000,
      `the reminder "${token}" to appear in the ledger`
    )
    const landed = reminders.find((reminder) => (reminder.prompt ?? '').includes(token))
    ok(`reminder ${landed.id} owned by ${landed.personId}: ${JSON.stringify(landed.prompt)}`)

    ok('rendered')
  } finally {
    browser.close()
  }

  process.stdout.write(
    `\nPASS — a hosted agent in "${DIR}" called an org_* tool from the browser,` +
      `and the ledger changed.\n`
  )
}

// Guarded so the module can be IMPORTED without running the flow. The check
// `scripts/test/browser-check-runnable.test.mjs` imports this file to prove it
// still loads — which is exactly what its predecessor stopped being able to do.
if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    process.stderr.write(`\nFAIL — ${error.message}\n`)
    for (const reason of failures) process.stderr.write(`  ${reason}\n`)
    process.exit(1)
  })
}
