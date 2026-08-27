'use client'

/**
 * Holds the operator JWT in React state — in memory only, never in any
 * browser persistence layer (mandate 2). Acquires on mount via `POST
 * /api/session`; exposes `refresh()` as `ChiefApiClientService`'s
 * `onUnauthorized` hook, so a 401 re-acquires once and retries, never loops.
 * A hard reload simply re-runs this provider's mount effect.
 */
import {
  createContext,
  type ReactElement,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState
} from 'react'

import { publicApiBaseUrl } from '@/common/PublicEnv'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import { SessionClientService } from '@/services/SessionClientService'

interface ApiSessionContextValue {
  ready: boolean
  client: ChiefApiClientService
  accessToken: () => string | null
}

const ApiSessionContext = createContext<ApiSessionContextValue | undefined>(undefined)

function InjectedApiSessionProvider(props: {
  children: ReactNode
  client: ChiefApiClientService
  tokenGetter: () => string | null
}): ReactElement {
  const value = useMemo<ApiSessionContextValue>(
    () => ({ ready: true, client: props.client, accessToken: props.tokenGetter }),
    [props.client, props.tokenGetter]
  )
  return <ApiSessionContext.Provider value={value}>{props.children}</ApiSessionContext.Provider>
}

function AuthenticatedApiSessionProvider(props: { children: ReactNode }): ReactElement {
  const [token, setToken] = useState<string | null>(null)
  const [ready, setReady] = useState(false)
  // Kept in sync with `token` state below (every render) so the client's
  // `accessToken` closure always reads the current value without itself
  // becoming a changing dependency the client would need to be rebuilt for.
  const tokenRef = useRef<string | null>(null)
  tokenRef.current = token

  const [sessionClient] = useState(() => new SessionClientService())

  const refresh = useCallback(async (): Promise<void> => {
    const result = await sessionClient.acquire()
    setToken(result.token)
    setReady(true)
  }, [sessionClient])

  const [client] = useState(
    () =>
      new ChiefApiClientService({
        baseUrl: publicApiBaseUrl(),
        accessToken: () => tokenRef.current,
        onUnauthorized: refresh
      })
  )

  useEffect(() => {
    void refresh()
  }, [refresh])

  const value = useMemo<ApiSessionContextValue>(
    () => ({ ready, client, accessToken: () => tokenRef.current }),
    [ready, client]
  )

  return <ApiSessionContext.Provider value={value}>{props.children}</ApiSessionContext.Provider>
}

export function ApiSessionProvider(props: {
  children: ReactNode
  client?: ChiefApiClientService
  tokenGetter?: () => string | null
}): ReactElement {
  if (props.client && props.tokenGetter) {
    return (
      <InjectedApiSessionProvider client={props.client} tokenGetter={props.tokenGetter}>
        {props.children}
      </InjectedApiSessionProvider>
    )
  }
  return <AuthenticatedApiSessionProvider>{props.children}</AuthenticatedApiSessionProvider>
}

function useApiSessionContext(): ApiSessionContextValue {
  const context = useContext(ApiSessionContext)
  if (!context) {
    throw new Error('useChiefApi/useAccessToken must be used within ApiSessionProvider')
  }
  return context
}

/** The configured `ChiefApiClientService` — the ONE way any component talks
 * to apps/api. */
export function useChiefApi(): ChiefApiClientService {
  return useApiSessionContext().client
}

/** The in-memory token getter, for callers (S4's SSE client) that build
 * their own `Authorization` header rather than going through the client. */
export function useAccessToken(): () => string | null {
  return useApiSessionContext().accessToken
}
