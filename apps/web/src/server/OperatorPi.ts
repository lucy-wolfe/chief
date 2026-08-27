/**
 * The route every hosted conversation on this server runs on: the operator's
 * own Pi defaults, resolved by Pi itself.
 *
 * # One code path, because there is one operator
 *
 * This server holds a SINGLE operator identity — `common/Env`'s
 * `OPERATOR_IDENTITY_ID` is a literal, the signing key is the one file the
 * daemon mints on this box, and the Pi agent dir is derived from this process's
 * own `$HOME`. Nothing here is per-tenant, so a per-person credential scope had
 * nothing to separate: every hosted agent and the Founder are the same
 * operator, on the same box, spending the same key. The three modules that used
 * to split this — a per-person `pi-home` credential read, a registry
 * projection, and a Founder-only copy of the same precedence rules — are one
 * function now, and the Founder and a company person get byte-identical
 * treatment because they ARE identical.
 *
 * # Nothing here selects a model
 *
 * Chief is out of the provider/model business. `ModelRuntime` and
 * `SettingsManager` are Pi's own, from the package the panes run, pointed at
 * the operator's own agent dir: the catalog is whatever Pi would build there,
 * and the route is `defaultProvider`/`defaultModel` — the exact pair Pi uses
 * for a session that names no model, which is what `chief`'s panes are. A
 * second resolution here would be this server deciding what a company runs on,
 * and that decision no longer exists anywhere in the product.
 */
import { join } from 'node:path'

import { ModelRuntime, SettingsManager } from '@earendil-works/pi-coding-agent'

import { operatorPiAgentDir } from '@/common/Env'
import type { OperatorRoute } from '@/types/OperatorPi'
import { isNullish } from '@/utils/Nullish'

/**
 * Pi's own default route on this box, or `undefined` when Pi has none.
 *
 * `undefined` is a REFUSAL for the caller to surface, never a substitution:
 * an agent silently moved onto some other model would run a whole company on a
 * route nobody chose.
 *
 * ONE refusal, not three, because there is one question: can Pi build this
 * route. `settings.json` naming nothing, a provider with no credential, and a
 * model no source describes are three ways to answer no, and Pi's own
 * `ModelRuntime` answers all three the same way — `getModel` returns nothing.
 * Splitting them here would be this server re-deciding what Pi already
 * decided, and the operator's next move is identical in every case: point Pi
 * at a model it can actually run.
 *
 * `allowModelNetwork` is off: this is a resolution of what the operator already
 * configured, and a Next request must never block on a per-provider catalog
 * fetch. Pi's own panes refresh that catalog for themselves.
 */
export async function operatorRoute(): Promise<OperatorRoute | undefined> {
  const agentDir = operatorPiAgentDir()
  const settings = SettingsManager.create(process.cwd(), agentDir)
  const provider = settings.getDefaultProvider()
  const modelId = settings.getDefaultModel()
  if (isNullish(provider) || isNullish(modelId)) return undefined
  const models = await ModelRuntime.create({
    authPath: join(agentDir, 'auth.json'),
    modelsPath: join(agentDir, 'models.json'),
    modelsStorePath: join(agentDir, 'models-store.json'),
    allowModelNetwork: false
  })
  const model = models.getModel(provider, modelId)
  if (isNullish(model)) return undefined
  return { models, model, provider }
}
