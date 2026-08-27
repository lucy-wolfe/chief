import { readFileSync } from 'node:fs'

import { checkExact } from './cargo-test-floor-lib.mjs'
import { deriveExpectedCounts } from './cargo-test-derive.mjs'

const [, , logPath, ...memberPaths] = process.argv
if (!logPath || memberPaths.length === 0) {
  console.error('usage: node scripts/cargo-test-workspace-shard-floor.mjs LOG MEMBER...')
  process.exit(2)
}

const output = readFileSync(logPath, 'utf8')
const root = new URL('../apps/chiefd/', import.meta.url).pathname
const expected = deriveExpectedCounts(root, { memberPaths })
const exact = checkExact(output, expected.expectedExecuted, expected.expectedBlocks)

console.log(
  `[cargo-test-shard-floor] members=${memberPaths.join(',')} expected=${expected.expectedExecuted}/${expected.expectedBlocks} ` +
    `observed=${exact.executed}/${exact.blocks}`,
)
if (!expected.vacuity.ok) {
  console.error('[cargo-test-shard-floor] refusing an invalid test inventory derivation')
  process.exitCode = 1
}
if (!exact.ok) {
  console.error(`[cargo-test-shard-floor] exact loss ratchet failed: ${exact.message}`)
  process.exitCode = 1
}
