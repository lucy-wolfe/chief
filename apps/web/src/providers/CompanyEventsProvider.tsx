'use client'

/** Supplies the one injected apps/api SSE dependency set to E6 consumers. */
import { createContext, type ReactElement, type ReactNode, useContext, useMemo } from 'react'

import { publicApiBaseUrl } from '@/common/PublicEnv'
import { useAccessToken } from '@/providers/ApiSessionProvider'
import type { SseHubDeps } from '@/types/Sse'

const CompanyEventsContext = createContext<SseHubDeps | undefined>(undefined)

function AuthenticatedCompanyEventsProvider(props: { children: ReactNode }): ReactElement {
  const accessToken = useAccessToken()
  const deps = useMemo<SseHubDeps>(
    () => ({ baseUrl: publicApiBaseUrl(), accessToken }),
    [accessToken]
  )
  return (
    <CompanyEventsContext.Provider value={deps}>{props.children}</CompanyEventsContext.Provider>
  )
}

/**
 * Production children inherit S2's in-memory bearer getter. A caller may
 * inject the same narrow dependency shape for deterministic stream tests;
 * neither form introduces another service address or token persistence path.
 */
export function CompanyEventsProvider(props: {
  children: ReactNode
  deps?: SseHubDeps
}): ReactElement {
  if (props.deps) {
    return (
      <CompanyEventsContext.Provider value={props.deps}>
        {props.children}
      </CompanyEventsContext.Provider>
    )
  }
  return <AuthenticatedCompanyEventsProvider>{props.children}</AuthenticatedCompanyEventsProvider>
}

export function useCompanyEventsDeps(): SseHubDeps {
  const deps = useContext(CompanyEventsContext)
  if (!deps) {
    throw new Error('useCompanyEvents/usePersonStream must be used within CompanyEventsProvider')
  }
  return deps
}
