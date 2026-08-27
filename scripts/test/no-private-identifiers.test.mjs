// The public tree carries no private identifier: no machine from the private
// fleet, no operator home path, no internal mailbox, no routable address.
//
// WHY THIS EXISTS, WHICH IS A MEASURED FAILURE AND NOT A HYPOTHETICAL.
// A redaction packet swept the tree for leaked hostnames, reported the sweep
// complete, and was disproved by a reviewer's grep in one command. The packet
// had matched only the FQDN form; every BARE hostname survived it, including
// one in a file the packet itself named as generalized. 74 occurrences of a
// single machine name were still in the tree.
//
// THE READING: a redaction whose acceptance is "the files I edited" is not a
// redaction, it is a diff summary. The only acceptance that means anything is
// a grep over the WHOLE tree, run by something that is not the person who did
// the editing. That is this file. It is the acceptance test for the
// redaction section of the open-source launch, and it runs on every commit
// thereafter so the next leak is caught at the commit that introduces it
// rather than by a reviewer, or by nobody.
//
// # SHAPE FIRST, NAMES LAST -- and the names are ENCODED
//
// The rows come in two kinds, deliberately in this order.
//
// A SHAPE rule bans a FORM without naming anything: an address at an internal
// mail domain, a hostname under the fleet domain, a routable IP literal.
// A shape catches the NEXT leak as well as this one, which a list of known
// names can never do, and it publishes nothing.
//
// An ENCODED row is the shapeless residue -- a bare machine name is just a
// word, and no form distinguishes it from prose. Those are stored base64 and
// decoded at runtime. THIS IS NOT SECURITY-BY-OBSCURITY AND NOT SELF-MATCH
// AVOIDANCE: it is the point that a scanner banning private hostnames must
// not itself publish them. An earlier draft split each name across a string
// concatenation, which defeats the guard's own grep and defeats nothing else
// -- a human reader, or a code search, reconstructs `'exam' + 'ple-host'`
// instantly, and the rows even stated each machine's ROLE, which turned a
// list into a map of the private fleet. (That example is FICTIONAL on
// purpose: an earlier draft of this very paragraph illustrated the point
// with a real hostname, which is ruling 1 defeated by the file's own prose.
// The self-test below checks decoded LITERALS, so it passed. Prose is not
// exempt from the rule it explains.) Encoding, plus deliberately generic
// reasons, is what makes this file safe to publish. A test below asserts no
// decoded name appears in this file's own source.
//
// # A GUARD THAT CRIES WOLF GETS DELETED
//
// Two deliberate narrowings, stated so nobody reads them as oversights.
// A machine name that is ALSO an ordinary word (`demo`, `support`, `tribes`)
// is banned only in its FQDN shape, because banning the bare word would fire
// on `chiefd-support`, on the organization's own name, and on prose. And IP
// literals are banned only OUTSIDE the loopback, private and documentation
// ranges, because every test that binds a socket names one of those.
//
// # THE OBSERVATION WINDOW, and its bound
//
// Stated so nobody assumes coverage that does not exist: this guard's window
// is the WORKING TREE at a commit. A PR title, a commit message, a branch name
// and a release note are all OUTSIDE it -- those belong to review, not to this
// file. Measured: a banned literal sat in a pull request's TITLE after the
// tree was fully clean by this guard's own grep, and the squash subject would
// have carried it into commit history, which nothing here scans and nothing
// can amend afterwards.
//
// AND THE BOUND, so nobody over-builds against that. main's commit history is
// NEVER published -- the public repository is cut as a single orphan-root
// commit -- so a banned string in a squash subject lands in the PRIVATE
// archive, beside the old ledger, and not in the public tree. Do not propose a
// commit-message scanner for a public exposure that does not exist.
//
// The one PUBLISHED commit message is the orphan root's own, and it is checked
// by a verbatim step in the flip runbook, at the only moment it exists. That
// is the honest split: the tree is guarded continuously, the metadata is
// reviewed, and the single published message gets a named step.
//
// Mandate 1 (reactive-only): every check here is a single synchronous read --
// no polling, no interval, no sleep.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const __filename = fileURLToPath(import.meta.url)
const __dirname = dirname(__filename)
const root = join(__dirname, '..', '..')

/** Decode one stored row. Kept as a named function so the call sites read as
 *  "the banned literal", not as base64 plumbing. */
const decode = (encoded) => Buffer.from(encoded, 'base64').toString('utf8')

/**
 * SHAPE rules: a form is banned, and nothing is named.
 *
 * These are the rows that catch a leak nobody has seen yet, which is the only
 * kind of row that keeps working after everybody who remembers the private
 * fleet has moved on.
 *
 * @type {ReadonlyArray<{name: string, pattern: string, why: string}>}
 */
export const SHAPE_RULES = [
  {
    name: 'an address at an internal mail domain',
    pattern: '[A-Za-z0-9._%+-]+@(tribes\\.xyz|zbox\\.sh)',
    why: 'A mailbox at an internal domain identifies a person or a sandbox. Fixtures use example.com or example.invalid, which are reserved for exactly this.',
  },
  {
    name: 'a citation of an unpublished plan document',
    pattern: 'plans/[a-z0-9][a-z0-9-]*\\.(md|png)',
    why: 'A plan is a LOCAL working document and is git-ignored, so a concrete plan filename in the tree is a pointer at something no reader can open. Drop the POINTER and keep the sentence: "Stage 4 of that work deletes the reap". Where the citation IS the sentence, the sentence goes. This row needs NO allowlist by construction -- the sanctioned placeholders (`plans/<slug>.md`, `plans/**`) cannot match a pattern that requires a lowercase letter or digit after the slash, so the safe case is UNMATCHABLE rather than excused.',
  },
  {
    name: 'a hostname under the internal fleet domain',
    pattern: '[a-z0-9][a-z0-9-]*\\.zbox\\.sh',
    why: 'Any name under the fleet domain is a private machine. This is the ONE form the first sweep caught, which is why the bare names are rows of their own below rather than folded in here.',
  },
]

/**
 * The shapeless residue: bare machine and operator identifiers, stored
 * base64 so this file does not publish what it bans.
 *
 * Every `why` is deliberately GENERIC. An earlier draft named each machine's
 * role -- "the build host", "the QA box" -- which is strictly worse than the
 * name alone: it turns a list into a map.
 *
 * Row shape, and what each narrowing is FOR -- every one exists because a row
 * that is too broad refuses something the operator approved:
 *   `wordBoundary`  the literal only as a whole word.
 *   `homePath`      only as somebody's home directory, on either platform.
 *   `notAfter`      not when preceded by that character (the maintainer's
 *                   `@handle` versus the bare hostname they share a spelling
 *                   with).
 *   `notAllLowercase` every capitalisation EXCEPT the all-lower-case one, for
 *                   a name whose lower-case spelling is a ruled survivor.
 *
 * @type {ReadonlyArray<{name: string, encoded: string, why: string,
 *   wordBoundary?: boolean, homePath?: boolean, notAfter?: string,
 *   notAllLowercase?: boolean}>}
 */
export const ENCODED_IDENTIFIERS = [
  {
    name: 'a publication-gate placeholder',
    // Encoded like every other row, and for a sharper reason than usual: the
    // entry that ANNOUNCED this class was at zero quoted the grep proving it,
    // and was therefore the last remaining instance. The report of the zero
    // was the last nonzero.
    encoded: 'PDxIVU1BTg==',
    why:
      'A placeholder marking a decision the operator had not yet made. The tree must carry NONE before publication, and the grep that proves it must not be quotable into falseness. ' +
      'WHY THIS IS A GUARD AND NOT A RUNBOOK STEP: the run-the-guards-again rule cannot reach a check that never runs. Section I.4 was a grep to be run ONCE, by a human, at the moment of the flip -- against a tree whose own changelog had quietly falsified it. ' +
      'The general form: a precondition that is only ever checked by a human at the moment of use is not a precondition, it is an intention. Where one can be a guard, it should be.',
  },
  { name: 'retired private hostname (1)', encoded: 'c3BhcmtsaW5nLW1hbnRpcw==' },
  { name: 'retired private hostname (2)', encoded: 'bWFjaGluYQ==' },
  { name: 'retired private hostname (3)', encoded: 'bHVjeS1xYQ==' },
  // TWO DIFFERENT THINGS SHARE THIS SPELLING, and the row has to say which.
  // It is a retired private hostname AND, since the repository went public,
  // the maintainer's GitHub handle in `.github/CODEOWNERS`. The ban is the
  // BARE form; an `@`-prefixed owner reference is the operator's own approved
  // file and must never be reported. Without `notAfter` this row refuses a
  // file the operator just signed off.
  { name: 'retired private hostname (4)', encoded: 'aGlzaGJveQ==', notAfter: '@' },
  { name: 'retired private hostname (5)', encoded: 'c2FuZGJveC1ob3N0LWhpc2g=' },
  { name: 'retired private hostname (6)', encoded: 'c2hvd3plbg==' },
  { name: 'retired private hostname (7)', encoded: 'ZGVhbm5h' },
  { name: 'retired private hostname (8)', encoded: 'd2ViLWJyb3dzZXItcWE=' },
  { name: 'retired private hostname (9)', encoded: 'YXlh', wordBoundary: true },
  // Home paths are rows per NAME rather than a `/home/<word>` shape, because
  // the shape would fire on every generic fixture in the tree (`/home/op`,
  // `/home/user`, `/home/dev`) and a guard that cries wolf gets deleted.
  // Each row covers BOTH platform spellings -- the `/home/` row was written
  // against Linux and a macOS `/Users/` fixture carrying a real name walked
  // straight past it, which is the suffix-grep failure one directory over.
  { name: 'an operator home path (1)', encoded: 'bHVjeQ==', homePath: true },
  { name: 'an operator home path (2)', encoded: 'aGlzaA==', homePath: true },
  // A messaging chat id and its contact name. The id had to be found by grep
  // three separate times -- once as the fixture for the redaction function
  // that exists to redact it -- which is what a standing rule is for.
  { name: 'a personal messaging identifier', encoded: 'NTMwMTU0OTA4' },
  { name: 'a contact name', encoded: 'aGlzaA==', wordBoundary: true },
  // The operator's given NAME, in prose. Ruled 2026-08-25: comments cite the
  // decision, not the person -- `the operator's mandate`, in the house style
  // `AGENTS.md` already uses. The WHY always survives; only the byline goes.
  //
  // `notAllLowercase`, deliberately and uniquely. The `lucy/*` eslint
  // namespace is a ruled SURVIVOR (below), and it is the same letters in
  // lower case.
  //
  // THIS ROW WAS CASE-SENSITIVE UNTIL IT LET A LEAK THROUGH, and the failure is
  // worth keeping because the narrowing was chosen for a good reason and opened
  // a hole nobody then checked. Dropping the scan's `-i` spares the lower-case
  // namespace by spelling the name in exactly ONE capitalisation -- so an
  // ALL-CAPS occurrence is neither the banned spelling nor the survivor, and
  // two of them -- a dated incident citation and a test's headline, both
  // shouted -- sat in one Rust file through every run of this guard, including
  // the sweep that reported the name at zero. This comment does not spell
  // them: the row bans that form now, and an explanation is not exempt from
  // the rule it explains. (It caught this very line on the first run, which is
  // the second time this file has done that to its own author.)
  //
  // WHAT SHIPPED, AND WHAT DELIBERATELY DID NOT. The tempting design was to
  // narrow by what FOLLOWS -- the survivor is a package namespace, so ban the
  // name except before `/` -- and it failed within one run: the plugin also
  // appears as an import binding (`<name>: <name>Plugin`), inside hyphenated
  // rule names, and in prose ABOUT the namespace, so the "real distinction"
  // was not expressible as one lookaround. What shipped instead is
  // `notAllLowercase`: ban every capitalisation EXCEPT the all-lower one,
  // derived per letter position rather than hand-written (see `bannedRegex`),
  // scanned without `-i`. It is a proxy, chosen with its eyes open, and its
  // WINDOW IS STATED IN THE ROW'S OWN `why` BELOW: an all-lowercase prose
  // citation of the person, and a camel-embedded occurrence that `\b` cannot
  // reach, are both invisible to it BY DESIGN -- review is their instrument,
  // not this row. A zero from this guard means "no capitalised form", never
  // "the name is absent"; the last two zeros from this row overclaimed
  // exactly that way, which is why the window is written down. The same name
  // as a HOME PATH is covered by its own row above -- and this comment
  // deliberately does not spell that path, because the row above bans it and
  // an explanation is not exempt from the rule it explains. (It caught this
  // very line on the first run.)
  {
    name: 'a person\'s given name',
    encoded: 'THVjeQ==',
    notAllLowercase: true,
    why:
      'A person\'s given name, banned in every capitalisation except the all-lower-case spelling, ' +
      'which is a ruled survivor (the eslint plugin namespace). TWO FORMS ARE INVISIBLE TO THIS ROW ' +
      'BY DESIGN and are review\'s to catch, not this guard\'s: an all-lowercase prose citation of ' +
      'the person, and a camel-embedded occurrence a word boundary cannot reach. A zero here means ' +
      '"no capitalised form" -- it is not proof the name is absent.',
  },
].map((row) => ({
  wordBoundary: false,
  why:
    'A retired private identifier -- a machine in the fleet this project was developed on, or a path naming its operator. ' +
    'Generalize the prose ("a live box", "a build host", "a live company"): keep the incident, its date and its numbers, drop the name.',
  ...row,
}))

/** Ranges an IP literal may legitimately sit in. Everything else is a real
 *  address and is banned. Without this the guard would fire on every test that
 *  binds a socket, and a guard that cries wolf gets deleted. */
const ALLOWED_IP_PREFIXES = [
  '127.', // loopback
  '0.0.0.0',
  '255.', // broadcast
  '10.', // RFC 1918
  '192.168.', // RFC 1918
  '192.0.2.', // RFC 5737 TEST-NET-1
  '198.51.100.', // RFC 5737 TEST-NET-2
  '203.0.113.', // RFC 5737 TEST-NET-3
  ...Array.from({ length: 16 }, (_, index) => `172.${16 + index}.`), // RFC 1918
]

/**
 * Terms that LOOK like a row above and are deliberately kept, each with the
 * ruling that keeps it.
 *
 * This list SUPPRESSES NOTHING -- no banned pattern matches these -- so it is
 * not an exemption mechanism. It is the record of why a future reader greping
 * this tree and finding these should not redact them, kept beside the scanner
 * rather than in a document nobody will read.
 *
 * The test below asserts each is still PRESENT. A row whose term has since
 * gone is dead config, and dead config in an allowlist is the exact shape
 * (#963) that hid a real failure once already.
 *
 * @type {ReadonlyArray<{term: string, ruling: string}>}
 */
export const RULED_SURVIVORS = [
  {
    term: 'tribes-capital',
    ruling: 'Operator, 2026-08-25: not sensitive. No rename anywhere.',
  },
  {
    term: 'cobalt',
    ruling: 'Follows the tribes-capital ruling (team-lead, 2026-08-25): a company name in fixtures, not a machine.',
  },
  {
    term: 'taperoom',
    ruling: 'Follows the tribes-capital ruling (team-lead, 2026-08-25): a company name in fixtures and in measured incident reports, not a machine.',
  },
  {
    term: 'lucy/',
    ruling: 'The eslint rule namespace, kept by ruling (team-lead, 2026-08-25). A package namespace, not a machine and not a byline.',
  },
  {
    term: 'zipbox',
    ruling:
      'Team-lead, 2026-08-25, confirmed twice: a publicly named hosting platform and, in most occurrences, a LOAD-BEARING identifier -- a CA path product code reads, a provider name in a fixture, an eslint rule name, an extension filename. Renaming it would be a behaviour change wearing a redaction’s clothes. Its FQDN form is still banned by the fleet-domain shape rule above, which is the correct split: the platform is public, a machine under it is not.',
  },
  {
    term: '/root/workspace',
    ruling:
      'A conventional container path, not a machine and not a person -- and scripts/guard-repo-path.sh exists to protect precisely that path, so banning it would break a live guard. Named here so the next sweep does not re-litigate it.',
  },
  {
    term: 'amber',
    ruling:
      'A colour token (an amber status ring, #8a2b00) and a company name in Rust fixtures beside cobalt. The one place it named a machine was generalized in prose instead; banning the word would fire on every status-colour reference, and a guard that cries wolf gets deleted.',
  },
]

/**
 * The regex one encoded row bans. ONE definition, shared by the scanner and by
 * the does-this-file-republish self-test, because two copies of a predicate is
 * how the two answers start disagreeing.
 *
 * A `homePath` row is a NAME and is banned only as somebody's home directory,
 * on EITHER platform. Banning the bare name would fire on prose; banning one
 * platform's spelling is how a macOS `/Users/<name>` fixture walked past this
 * guard while its Linux twin was caught.
 */
export function bannedRegex(row) {
  const literal = decode(row.encoded).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  if (row.homePath) return `/(home|Users)/${literal}\\b`
  const core = row.wordBoundary ? `\\b${literal}\\b` : literal
  // `notAfter` narrows a row to the occurrences NOT preceded by one character.
  // POSIX ERE, which `git grep -E` speaks, has no lookbehind -- so the
  // alternation is line-start or any other character, which is the same rule
  // written the way the instrument can read it.
  // `notAllLowercase` bans every capitalisation EXCEPT the all-lower-case one,
  // for a name whose lower-case spelling is a ruled survivor. ERE cannot say
  // "not this string", so the pattern is DERIVED: one alternative per letter
  // position, each forcing that position upper-case and leaving the rest
  // either -- which is exactly "at least one capital", written the way
  // `git grep -E` can read it. Derived rather than hand-written because a
  // hand-written alternation is precisely where a missed case hides, and a
  // missed case in this row is what this narrowing exists to fix.
  if (row.notAllLowercase) {
    const letters = decode(row.encoded).split('')
    const either = letters.map((c) => `[${c.toLowerCase()}${c.toUpperCase()}]`)
    const branches = letters.map((c, index) =>
      either.map((slot, at) => (at === index ? c.toUpperCase() : slot)).join(''),
    )
    return `\\b(${branches.join('|')})\\b`
  }
  return row.notAfter ? `(^|[^${row.notAfter}])${core}` : core
}

/** Every tracked line matching `pattern`, under `cwd`. Empty array (never a
 *  thrown error) when nothing matches -- `git grep` exits 1 on no match, and
 *  that is this scanner's success, not an infrastructure failure. */
export function linesMatching(pattern, cwd = root, caseSensitive = false) {
  try {
    const flags = caseSensitive ? '-nIE' : '-nIiE'
    return execFileSync('git', ['grep', flags, pattern], { cwd, encoding: 'utf8' })
      .split('\n')
      .filter(Boolean)
  } catch (error) {
    if (error.status === 1) return []
    throw new Error(
      `REFUSING TO REPORT SUCCESS: cannot enumerate tracked files for ${JSON.stringify(pattern)} ` +
        `(${error.message}). This check has not passed, it has not run -- silence is never the green.`
    )
  }
}

/** IP literals outside the allowed ranges, as `file:line:text` rows. */
export function routableIpLiterals(cwd = root) {
  const hits = linesMatching('\\b([0-9]{1,3}\\.){3}[0-9]{1,3}\\b', cwd)
  const offending = []
  for (const line of hits) {
    for (const [address] of line.matchAll(/\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b/g)) {
      if (ALLOWED_IP_PREFIXES.some((prefix) => address.startsWith(prefix))) continue
      offending.push(`${line}   <-- ${address}`)
      break
    }
  }
  return offending
}

/** Every banned identifier still present under `cwd`, as reportable offences. */
export function findLeaks(cwd = root, shapes = SHAPE_RULES, encoded = ENCODED_IDENTIFIERS) {
  if (shapes.length === 0 && encoded.length === 0) {
    throw new Error(
      'REFUSING TO REPORT SUCCESS: zero banned rows -- a broken derivation, not evidence the tree is clean.'
    )
  }
  const offences = []
  // Both arms carry `caseSensitive` explicitly. Only a `notAllLowercase` row
  // needs it: its pattern encodes the capitalisations itself, so the scan must
  // not fold case underneath it. A SHAPE row bans a form and
  // never a name, so case never matters to one -- but leaving the field off
  // makes the two arms different types, and the scanner below has to ask every
  // row the same question.
  const rows = [
    ...shapes.map((row) => ({ ...row, regex: row.pattern, caseSensitive: false })),
    ...encoded.map((row) => ({ ...row, regex: bannedRegex(row), caseSensitive: row.notAllLowercase === true })),
  ]
  for (const row of rows) {
    const hits = linesMatching(row.regex, cwd, row.caseSensitive === true)
    if (hits.length === 0) continue
    offences.push(
      `${row.name} (${hits.length} occurrence(s)):\n  ${hits.slice(0, 10).join('\n  ')}` +
        (hits.length > 10 ? `\n  ... +${hits.length - 10} more` : '') +
        `\n  WHY BANNED: ${row.why}`
    )
  }
  const ips = routableIpLiterals(cwd)
  if (ips.length > 0) {
    offences.push(
      `a routable IP literal (${ips.length} occurrence(s)):\n  ${ips.slice(0, 10).join('\n  ')}` +
        '\n  WHY BANNED: a real address names a real machine. Loopback, RFC 1918 and the RFC 5737 ' +
        'documentation ranges (192.0.2.x, 198.51.100.x, 203.0.113.x) are allowed and are what a fixture should use.'
    )
  }
  return offences
}

// ---------------------------------------------------------------------------
// The real check: the whole tree, every commit.
// ---------------------------------------------------------------------------

test('no tracked file carries a private identifier', () => {
  const offences = findLeaks(root)
  assert.deepEqual(
    offences,
    [],
    offences.length > 0
      ? `The public tree carries ${offences.length} private-identifier class(es):\n\n${offences.join('\n\n')}\n\n` +
          'Generalize the prose -- keep the incident, its date and its numbers, drop the name -- rather than ' +
          'deleting the sentence. If a row here is genuinely wrong, change the row and record why; do not widen ' +
          'this assertion.'
      : undefined
  )
})

test('this guard does not republish what it bans', () => {
  // The whole reason the rows are encoded. If a decoded name ever appears in
  // this file's own source, the scanner has become the leak.
  //
  // It asks each row's OWN question -- `bannedRegex(row)`, the same predicate
  // the scanner uses -- rather than a raw substring test. A substring test
  // reported this file for the four letters of a short contact name sitting
  // inside the word "republish", which is a false positive that would have
  // been silenced by weakening the check. Sharing the predicate is the repair:
  // if a row would not flag the text in the tree, it must not flag it here.
  const source = readFileSync(__filename, 'utf8')
  for (const row of ENCODED_IDENTIFIERS) {
    assert.ok(
      !new RegExp(bannedRegex(row)).test(source),
      `${row.name} appears in plaintext in this guard's own source -- encode it. A scanner that bans ` +
        'a private identifier must not print it.'
    )
  }
})

test('every ruled survivor is still present, so no allowlist row outlives its reason', () => {
  const stale = []
  for (const { term, ruling } of RULED_SURVIVORS) {
    if (linesMatching(term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), root).length === 0) {
      stale.push(`${JSON.stringify(term)} matches nothing -- stale row. Ruling was: ${ruling}`)
    }
  }
  assert.deepEqual(stale, [], stale.join('\n'))
})

test('a row cannot be filed in the wrong list: SHAPE rows have a pattern, ENCODED rows have an encoded', () => {
  // MEASURED, on the commit that added the placeholder row. It was filed into
  // SHAPE_RULES, which reads `row.pattern` -- absent on an encoded row. The
  // scanner stringified `undefined` and greped for it, so the guard reported
  // 1,785 leaks, every one a line containing the WORD "undefined".
  //
  // The failure was loud, but loud in a way that pointed nowhere near the
  // cause: nothing in the output said "this row has no pattern", and the
  // reported hits were ordinary TypeScript. A missing field became a regex
  // that matched a common English word, which is the worst available failure
  // -- a scanner that fires constantly gets its ROW deleted, not its bug
  // found.
  for (const row of SHAPE_RULES) {
    assert.ok(
      Object.hasOwn(row, 'pattern') && typeof row.pattern === 'string',
      `shape row ${JSON.stringify(row.name)} has no pattern -- an encoded row filed here greps for ` +
        'the string "undefined"'
    )
    assert.ok(!Object.hasOwn(row, 'encoded'), `shape row ${JSON.stringify(row.name)} carries an encoded field; it belongs in ENCODED_IDENTIFIERS`)
  }
  for (const row of ENCODED_IDENTIFIERS) {
    assert.ok(
      Object.hasOwn(row, 'encoded') && typeof row.encoded === 'string',
      `encoded row ${JSON.stringify(row.name)} has no encoded value`
    )
    assert.ok(!Object.hasOwn(row, 'pattern'), `encoded row ${JSON.stringify(row.name)} carries a pattern; it belongs in SHAPE_RULES`)
    assert.notEqual(decode(row.encoded), '', `encoded row ${JSON.stringify(row.name)} decodes to nothing`)
  }
})

test('every banned row states WHY, and every survivor states its RULING', () => {
  // A pattern with no stated subject is the thing this repo keeps having to
  // reconstruct months later. Both halves are checked, because a survivor row
  // with no ruling is indistinguishable from somebody quietly excusing a leak
  // they did not want to fix.
  for (const row of [...SHAPE_RULES, ...ENCODED_IDENTIFIERS]) {
    assert.ok(row.name.length > 0, 'a banned row has no name')
    assert.ok(row.why.length > 30, `banned row ${JSON.stringify(row.name)} has no real reason`)
  }
  for (const row of RULED_SURVIVORS) {
    assert.ok(row.ruling.length > 30, `survivor ${JSON.stringify(row.term)} has no recorded ruling`)
  }
})

// ---------------------------------------------------------------------------
// NON-VACUITY. A scanner that has never been watched fire is a scanner nobody
// has evidence about -- and this one runs against a tree that is expected to
// be clean, so it reports success on every single run by design. That is
// precisely the condition under which a broken scanner is invisible.
//
// Every fixture plants its needle by DECODING at runtime, so the proofs do
// not spell in source what the rows exist to keep out of source.
// ---------------------------------------------------------------------------

function withScratchGitRepo(body) {
  const dir = mkdtempSync(join(tmpdir(), 'no-private-identifiers-'))
  try {
    execFileSync('git', ['init', '-q'], { cwd: dir })
    execFileSync('git', ['config', 'user.email', 'guard@example.com'], { cwd: dir })
    execFileSync('git', ['config', 'user.name', 'Guard'], { cwd: dir })
    body(dir)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

function commitAll(dir) {
  execFileSync('git', ['add', '-A'], { cwd: dir })
  execFileSync('git', ['commit', '-q', '-m', 'fixture'], { cwd: dir })
}

test('NON-VACUITY: a planted bare hostname IS caught, in a real git tree', () => {
  withScratchGitRepo((dir) => {
    mkdirSync(join(dir, 'src'), { recursive: true })
    const planted = decode(ENCODED_IDENTIFIERS[0].encoded)
    writeFileSync(join(dir, 'src', 'note.rs'), `// Measured on \`${planted}\`: a refusal every pass.\n`)
    commitAll(dir)
    const offences = findLeaks(dir)
    assert.equal(offences.length, 1, `expected the planted hostname to be caught; got ${JSON.stringify(offences)}`)
    assert.match(offences[0], /src\/note\.rs/)
    assert.match(offences[0], /WHY BANNED:/)
  })
})

test('NON-VACUITY: the BARE form is caught, not only the FQDN -- the exact miss this guard exists for', () => {
  withScratchGitRepo((dir) => {
    // The first sweep matched the FQDN and reported completion. Both forms
    // must fail here, and the bare one is the case that regressed.
    const planted = decode(ENCODED_IDENTIFIERS[0].encoded)
    writeFileSync(join(dir, 'fqdn.md'), `built on ${planted}.zbox` + '.sh\n')
    writeFileSync(join(dir, 'bare.md'), `built on ${planted}\n`)
    commitAll(dir)
    const names = findLeaks(dir).join('\n')
    assert.match(names, /fqdn\.md/)
    assert.match(
      names,
      /bare\.md/,
      'the bare hostname must be caught on its own -- matching only the FQDN is the measured failure this guard replaces'
    )
  })
})

test('NON-VACUITY: an internal mailbox is caught by SHAPE, with no name in the row', () => {
  withScratchGitRepo((dir) => {
    writeFileSync(join(dir, 'contact.md'), 'write to someone@zbox' + '.sh for access\n')
    commitAll(dir)
    const offences = findLeaks(dir)
    assert.equal(offences.length, 1, `expected the mailbox to be caught; got ${JSON.stringify(offences)}`)
    assert.match(offences[0], /internal mail domain/)
  })
})

test('NON-VACUITY: a routable IP is caught and the reserved ranges are NOT', () => {
  withScratchGitRepo((dir) => {
    writeFileSync(
      join(dir, 'ok.md'),
      'bind 127.0.0.1, 0.0.0.0, 10.1.2.3, 192.168.0.7, 172.20.0.1, 192.0.2.5, 198.51.100.1, 203.0.113.9\n'
    )
    commitAll(dir)
    assert.deepEqual(findLeaks(dir), [], 'loopback, RFC 1918 and the documentation ranges must never fire')

    // Composed at runtime, not written as a literal: this file is itself in
    // the tree the guard scans, and a fixture spelling a routable address
    // would make the scanner its own first offender.
    const routable = [93, 184, 216, 34].join('.')
    writeFileSync(join(dir, 'leak.md'), `the box answered on ${routable}\n`)
    commitAll(dir)
    const offences = findLeaks(dir)
    assert.equal(offences.length, 1, `expected the routable address to be caught; got ${JSON.stringify(offences)}`)
    assert.ok(offences[0].includes(routable))
  })
})

test('CONTROL: a tree carrying only the ruled survivors is clean', () => {
  withScratchGitRepo((dir) => {
    writeFileSync(
      join(dir, 'survivors.md'),
      RULED_SURVIVORS.map((r) => `${r.term} is kept: ${r.ruling}`).join('\n') + '\n'
    )
    commitAll(dir)
    assert.deepEqual(findLeaks(dir), [], 'the ruled survivors must never be reported as leaks')
  })
})

test('CONTROL: a hostname that is also an ordinary word is banned only in its FQDN form', () => {
  // The deliberate narrowing, pinned so nobody "completes" the list later and
  // turns the guard into noise.
  withScratchGitRepo((dir) => {
    writeFileSync(join(dir, 'prose.md'), 'the chiefd-support crate, a demo company, the tribes organization\n')
    commitAll(dir)
    assert.deepEqual(findLeaks(dir), [], 'the bare ordinary words must not fire')

    writeFileSync(join(dir, 'host.md'), 'built on support.zbox' + '.sh\n')
    commitAll(dir)
    assert.equal(findLeaks(dir).length, 1, 'the same word under the fleet domain IS a machine and must fire')
  })
})

test('CONTROL: the sanctioned plan placeholders are UNMATCHABLE, not allowlisted', () => {
  // The property worth keeping, and the reason this row carries no exception:
  // `plans/<slug>.md` and `plans/**` cannot match a pattern that requires a
  // lowercase letter or digit immediately after the slash. Prefer the rule
  // whose safe case is unmatchable over the rule that needs a list of
  // exceptions -- a list is a thing that rots, and an impossibility is not.
  withScratchGitRepo((dir) => {
    writeFileSync(
      join(dir, 'convention.md'),
      'Write `plans/<slug>.md` before implementation. `plans/` is git-ignored.\n' +
        'CI once ignored `plans/**` on the paths-ignore block.\n'
    )
    commitAll(dir)
    assert.deepEqual(findLeaks(dir), [], 'the sanctioned placeholders must never fire')

    // Composed at runtime for the same reason the routable-IP fixture is:
    // this file is in the tree the guard scans, and a fixture spelling a
    // concrete plan filename would make the scanner its own first offender.
    // It already did, on this row's first run.
    const citation = `plans/${'instant'}-click.md`
    writeFileSync(join(dir, 'stale.rs'), `//! Stage 4 of \`${citation}\` deletes that reap.\n`)
    commitAll(dir)
    const offences = findLeaks(dir)
    assert.equal(offences.length, 1, `a concrete plan filename must fire; got ${JSON.stringify(offences)}`)
    assert.match(offences[0], /unpublished plan document/)
  })
})

test('CONTROL: the maintainer\'s @handle passes while the bare hostname still fires', () => {
  // Two different things share this spelling, and getting it wrong in either
  // direction is a real failure: too loose and a retired hostname ships; too
  // strict and the guard refuses `.github/CODEOWNERS`, a file the operator
  // personally approved.
  const shared = decode(ENCODED_IDENTIFIERS.find((row) => row.notAfter === '@').encoded)
  withScratchGitRepo((dir) => {
    mkdirSync(join(dir, '.github'), { recursive: true })
    writeFileSync(join(dir, '.github', 'CODEOWNERS'), `*    @${shared}\n`)
    commitAll(dir)
    assert.deepEqual(findLeaks(dir), [], 'an @-prefixed owner reference must never be reported')

    writeFileSync(join(dir, 'note.md'), `built on ${shared}\n`)
    commitAll(dir)
    const offences = findLeaks(dir)
    assert.equal(offences.length, 1, `the bare hostname must still fire; got ${JSON.stringify(offences)}`)
    assert.match(offences[0], /note\.md/)
  })
})

test('CONTROL: the given name fires in ANY case, the <name>/ namespace never does', () => {
  // The row that let a leak through. It used to be the file's one
  // case-sensitive row, spelling the name in exactly ONE capitalisation to
  // spare the lower-case eslint namespace -- so an ALL-CAPS citation was
  // neither the banned spelling nor the survivor, and two of them sat in
  // `daemon.rs` through every run of this guard, including the sweep that
  // reported the name at zero.
  //
  // The real distinction is what FOLLOWS the name, so that is what the row
  // narrows on now. Every arm below is a case this must get right.
  const row = ENCODED_IDENTIFIERS.find((candidate) => candidate.notAllLowercase === true)
  assert.ok(row, 'the given-name row must ban every capitalisation but the survivor')
  const name = decode(row.encoded)
  withScratchGitRepo((dir) => {
    // SURVIVORS: the package namespace, in the two shapes the tree uses.
    writeFileSync(join(dir, 'rules.mjs'), `'${name.toLowerCase()}/no-bignumber-to-string': 'error'\n`)
    writeFileSync(join(dir, 'import.mjs'), `import ${name.toLowerCase()}Plugin from '../x.js'\n`)
    commitAll(dir)
    assert.deepEqual(findLeaks(dir), [], 'the ruled eslint namespace must never be reported')

    // THE REGRESSION: all-caps, which passed for months.
    writeFileSync(join(dir, 'shout.rs'), `// ${name.toUpperCase()}'S EXACT FAILURE: a release stopped it.\n`)
    commitAll(dir)
    const shouted = findLeaks(dir)
    assert.equal(shouted.length, 1, `an ALL-CAPS citation must fire; got ${JSON.stringify(shouted)}`)
    assert.match(shouted[0], /shout\.rs/)
  })

  withScratchGitRepo((dir) => {
    // And the capitalisation that always fired must still fire.
    writeFileSync(join(dir, 'comment.rs'), `/// ${name}'s mandate: one fast SQL txn.\n`)
    commitAll(dir)
    const offences = findLeaks(dir)
    assert.equal(offences.length, 1, `the given name must fire; got ${JSON.stringify(offences)}`)
    assert.match(offences[0], /comment\.rs/)
  })
})

test('CONTROL: an empty row set REFUSES rather than reporting a clean tree', () => {
  withScratchGitRepo((dir) => {
    writeFileSync(join(dir, 'a.md'), 'anything\n')
    commitAll(dir)
    assert.throws(() => findLeaks(dir, [], []), /REFUSING TO REPORT SUCCESS/)
  })
})
