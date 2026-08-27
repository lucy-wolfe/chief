'use client'

/** In-memory companies directory state.  It fetches exactly on mount and
 * after a user-triggered lifecycle result; no interval or background poll
 * exists in this hook. */
import { useCallback, useEffect, useRef, useState } from 'react'

import { useChiefApi } from '@/providers/ApiSessionProvider'
import { useCompanyEventsDeps } from '@/providers/CompanyEventsProvider'
import type { ChiefApiClientService } from '@/services/ChiefApiClientService'
import { streamLifecycle } from '@/services/SseClientService'
import type { CompanySummary } from '@/types/ChiefApi'
import {
  type CompanyCreateInput,
  companyCreateRequest,
  type CompanyDirectoryState
} from '@/types/Companies'
import type { LifecyclePhaseFrame, LifecycleTerminal, SseHubDeps } from '@/types/Sse'

type CompanyDirectoryClient = Pick<ChiefApiClientService, 'listCompanies' | 'stopCompany'>
type LifecyclePath = '/companies' | `/companies/${string}/boot`

interface DirectorySnapshot {
  readonly companies: readonly CompanySummary[]
  readonly loading: boolean
  readonly error: unknown
}

function initialSnapshot(): DirectorySnapshot {
  return { companies: [], loading: true, error: undefined }
}

/** The injectable variant keeps the production hook on `useChiefApi()` while
 * giving the behavior a deterministic client/SSE seam for unit tests. */
export function useCompanyDirectoryWithClient(
  client: CompanyDirectoryClient,
  sseDeps: SseHubDeps
): CompanyDirectoryState {
  const [snapshot, setSnapshot] = useState<DirectorySnapshot>(initialSnapshot)
  const mountedRef = useRef(true)
  const refreshAbortRef = useRef<AbortController | undefined>(undefined)

  const refresh = useCallback(async (): Promise<readonly CompanySummary[]> => {
    refreshAbortRef.current?.abort()
    const controller = new AbortController()
    refreshAbortRef.current = controller
    if (mountedRef.current) {
      setSnapshot((previous) => ({ ...previous, loading: true, error: undefined }))
    }

    try {
      // `GET /companies` serves a BARE ARRAY of `CompanySummary`. This used
      // to read `response.companies` off a `{companies}` envelope apps/api
      // has never sent, so the directory rendered permanently empty.
      const companies = await client.listCompanies(controller.signal)
      if (mountedRef.current && refreshAbortRef.current === controller) {
        setSnapshot({ companies, loading: false, error: undefined })
      }
      return companies
    } catch (error) {
      if (controller.signal.aborted) return []
      if (mountedRef.current && refreshAbortRef.current === controller) {
        setSnapshot((previous) => ({ ...previous, loading: false, error }))
      }
      throw error
    } finally {
      if (refreshAbortRef.current === controller) refreshAbortRef.current = undefined
    }
  }, [client])

  useEffect(() => {
    mountedRef.current = true
    void refresh().catch(() => undefined)
    return () => {
      mountedRef.current = false
      refreshAbortRef.current?.abort()
      refreshAbortRef.current = undefined
    }
  }, [refresh])

  const runLifecycle = useCallback(
    async (
      path: LifecyclePath,
      body: unknown,
      onPhase: (frame: LifecyclePhaseFrame) => void
    ): Promise<LifecycleTerminal> => {
      let terminal: LifecycleTerminal | undefined
      let lifecycleError: unknown
      try {
        terminal = await streamLifecycle({ path, body, onPhase, deps: sseDeps })
      } catch (error) {
        lifecycleError = error
      }

      let refreshError: unknown
      try {
        await refresh()
      } catch (error) {
        refreshError = error
      }

      if (typeof lifecycleError !== 'undefined') throw lifecycleError
      if (typeof refreshError !== 'undefined') throw refreshError
      if (typeof terminal === 'undefined') {
        throw new Error('lifecycle stream returned no terminal frame')
      }
      return terminal
    },
    [refresh, sseDeps]
  )

  const create = useCallback(
    (
      input: CompanyCreateInput,
      onPhase: (frame: LifecyclePhaseFrame) => void
    ): Promise<LifecycleTerminal> =>
      runLifecycle('/companies', companyCreateRequest(input), onPhase),
    [runLifecycle]
  )

  const boot = useCallback(
    (
      companyKey: string,
      onPhase: (frame: LifecyclePhaseFrame) => void
    ): Promise<LifecycleTerminal> =>
      runLifecycle(`/companies/${encodeURIComponent(companyKey)}/boot`, undefined, onPhase),
    [runLifecycle]
  )

  const stop = useCallback(
    async (companyKey: string): Promise<void> => {
      let stopError: unknown
      try {
        await client.stopCompany(companyKey)
      } catch (error) {
        stopError = error
      }

      let refreshError: unknown
      try {
        await refresh()
      } catch (error) {
        refreshError = error
      }

      if (typeof stopError !== 'undefined') throw stopError
      if (typeof refreshError !== 'undefined') throw refreshError
    },
    [client, refresh]
  )

  return {
    ...snapshot,
    refresh,
    create,
    boot,
    stop
  }
}

/** Production entry point: REST reads/mutations use the typed session client,
 * and lifecycle streams receive the same app-API-only dependency set. */
export function useCompanyDirectory(): CompanyDirectoryState {
  const client = useChiefApi()
  const sseDeps = useCompanyEventsDeps()
  return useCompanyDirectoryWithClient(client, sseDeps)
}
