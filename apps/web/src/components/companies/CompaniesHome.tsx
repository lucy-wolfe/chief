'use client'

import Link from 'next/link'
import { useRouter } from 'next/navigation'
import { type ReactElement, useCallback, useRef, useState } from 'react'

import { BootPhaseConsole } from '@/components/companies/BootPhaseConsole'
import { CompanyDirectory } from '@/components/companies/CompanyDirectory'
import { useCompanyDirectory } from '@/hooks/UseCompanyDirectory'
import { type CompanyLifecycleView, lifecycleFailureFrom } from '@/types/Companies'
import type { LifecyclePhaseFrame } from '@/types/Sse'

function CompaniesHomeBody(): ReactElement {
  const router = useRouter()
  const directory = useCompanyDirectory()
  const runCounter = useRef(0)
  const [lifecycle, setLifecycle] = useState<CompanyLifecycleView | undefined>(undefined)
  const [actionError, setActionError] = useState<unknown>(undefined)

  const appendPhase = useCallback((runId: number, frame: LifecyclePhaseFrame): void => {
    setLifecycle((current) => {
      if (!current || current.runId !== runId) return current
      return { ...current, phases: [...current.phases, frame] }
    })
  }, [])

  const startBoot = useCallback(
    async (companyKey: string): Promise<void> => {
      const runId = (runCounter.current += 1)
      setActionError(undefined)
      setLifecycle({ runId, phases: [], companyKey })
      try {
        const terminal = await directory.boot(companyKey, (frame) => appendPhase(runId, frame))
        if (terminal.kind === 'failed') {
          setLifecycle((current) =>
            current && current.runId === runId ? { ...current, failure: terminal.error } : current
          )
          return
        }
        setLifecycle((current) =>
          current && current.runId === runId ? { ...current, terminal } : current
        )
        // The key that was booted, NOT `terminal.slug`. That field is chiefd's
        // answer off its own slug-keyed lifecycle wire, and `/c/:companyKey`
        // resolves nothing by a slug — two directories may hold companies
        // called the same thing. The console still shows the slug, because a
        // slug is what a person reads.
        router.push(`/c/${encodeURIComponent(companyKey)}`)
      } catch (error) {
        setActionError(error)
        setLifecycle((current) =>
          current && current.runId === runId
            ? { ...current, failure: lifecycleFailureFrom(error) }
            : current
        )
      }
    },
    [appendPhase, directory, router]
  )

  const stop = useCallback(
    async (companyKey: string): Promise<void> => {
      setActionError(undefined)
      try {
        await directory.stop(companyKey)
      } catch (error) {
        setActionError(error)
      }
    },
    [directory]
  )

  // BOOT is the only lifecycle this page starts now, so it is the only one it
  // can retry. Creating a company is Founder's, on its own page, with its own
  // failure reporting — a Retry button here for a run that began somewhere
  // else would re-run it without the conversation that decided its name.
  const retry = useCallback(async (): Promise<void> => {
    if (!lifecycle?.companyKey) return
    await startBoot(lifecycle.companyKey)
  }, [lifecycle, startBoot])

  const lifecycleRunning =
    typeof lifecycle !== 'undefined' &&
    typeof lifecycle.terminal === 'undefined' &&
    typeof lifecycle.failure === 'undefined'

  return (
    <main className="chief-page">
      <CompanyDirectory
        companies={directory.companies}
        error={actionError ?? directory.error}
        loading={directory.loading}
        onBoot={startBoot}
        onRetry={actionError && lifecycle?.failure ? retry : directory.refresh}
        onStop={stop}
      />
      {/* THE front door. The name/purpose form that stood here is gone: a
          company is created by talking to Founder, which is how the tmux
          surface has always done it. `POST /api/companies` is untouched and
          still creates a company — it is what Founder's own launch tool
          calls — so nothing lost a capability; what changed is who fills in
          the two facts, and it is no longer a form. */}
      <p className="chief-founder-entry">
        <Link className="chief-link-button chief-link-button--primary" href="/founder">
          Founder Mode
        </Link>
      </p>
      {lifecycle ? (
        <BootPhaseConsole
          failure={lifecycle.failure}
          label="Boot company"
          onRetry={lifecycle.failure ? retry : undefined}
          phases={lifecycle.phases}
          running={lifecycleRunning}
          terminal={lifecycle.terminal}
        />
      ) : null}
    </main>
  )
}

/** The lifecycle stream context comes from the root layout, which mounts it
 * for every route. This component wrapped itself in a second, identical
 * provider — harmless where it stood, but it is why `/c/[companyKey]` could
 * ship without one at all: the requirement looked like a component's business
 * rather than the app's. */
export function CompaniesHome(): ReactElement {
  return <CompaniesHomeBody />
}
