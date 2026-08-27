'use client'

import { type FormEvent, type ReactElement, useCallback, useState } from 'react'

import { useChiefApi } from '@/providers/ApiSessionProvider'
import type { DepartmentNode, DepartmentStateChangeResponse, TreePerson } from '@/types/ChiefApi'

/**
 * The org-structure editor: create a department, hire into one, transfer a
 * person between departments, re-parent a department, pause or resume a
 * department, and offboard someone.
 *
 * WHY IT LIVES BESIDE THE TREE RATHER THAN INSIDE A PANE
 *
 * A pane is one person talking. Every verb here changes who the people ARE,
 * where they are, or whether a whole unit is running at all, so they belong to
 * the tree and not to any pane in it — and every one of them changes the tree
 * the operator is looking at, which is why the whole rail refreshes from the
 * served tree after each verb rather than patching a local copy (mandate 2:
 * apps/api's answer is the state).
 *
 * WHAT THIS COMPONENT DOES NOT DECIDE
 *
 * Nothing structural. It sends the operator's intent — a name, a mandate, a
 * destination — and apps/api resolves the model authority, observes the
 * provider and runs chiefd's preview/commit handshake. The browser has no
 * provider credential and could not honestly do any of that; see the staffing
 * section of `types/ChiefApi.ts`.
 */

/** A department flattened for the depth-indented list and the destination
 * pickers. Depth is display-only. */
interface FlatDepartment {
  readonly node: DepartmentNode
  readonly depth: number
}

function flatten(nodes: readonly DepartmentNode[], depth = 0): FlatDepartment[] {
  return nodes.flatMap((node) => [{ node, depth }, ...flatten(node.children, depth + 1)])
}

/** Every department under `id`, including itself — the set a re-parent must
 * not target, because a department cannot be moved beneath its own descendant
 * (that detaches the subtree from the tree entirely). chiefd refuses it too;
 * this keeps the operator from being offered a choice that can only fail. */
function subtreeIds(node: DepartmentNode): Set<string> {
  const ids = new Set<string>([node.id])
  for (const child of node.children) for (const id of subtreeIds(child)) ids.add(id)
  return ids
}

/**
 * Turn pause/resume's refusal-as-a-value into a thrown error, so `run` can see
 * it.
 *
 * Every other verb on this rail reports a refusal by REJECTING — apps/api maps
 * chiefd's 422 onto the error envelope and `ChiefApiClientService` throws a
 * `ChiefApiError`. Pause and resume do not: chiefd's `AtomicDirectOutcome`
 * carries `{refused, detail}` as a successful value, the route passes it
 * through, and the response is a 200. So `await api.pauseDepartment(...)`
 * RESOLVES on the exact cases an operator most needs to read — pausing the
 * executive root answers `exec-root-protected` — and a handler that only
 * awaited it would clear the busy flag, close the draft and show nothing at
 * all. Silent failure, on the one verb whose failure is most likely.
 *
 * Throwing here (rather than inside the client) keeps the client honest about
 * what the route said and puts the refusal on the same error surface as every
 * other verb, in chiefd's own words. */
function applied(outcome: DepartmentStateChangeResponse): void {
  if ('refused' in outcome) throw new Error(`${outcome.refused}: ${outcome.detail}`)
}

type Draft =
  | { kind: 'none' }
  | { kind: 'new-department'; parentId: string }
  | { kind: 'hire'; departmentId: string }
  | { kind: 'transfer'; person: TreePerson; fromDepartmentId: string }
  | { kind: 'reparent'; node: DepartmentNode }
  | { kind: 'offboard'; person: TreePerson }

export function StructureRail(props: {
  companyKey: string
  departments: readonly DepartmentNode[]
  /** A company whose chiefd is not running cannot be restructured; every
   * route below resolves a chiefd client first. */
  readOnly: boolean
}): ReactElement {
  const api = useChiefApi()
  const [draft, setDraft] = useState<Draft>({ kind: 'none' })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | undefined>(undefined)

  const rows = flatten(props.departments)
  const rootId = props.departments[0]?.id

  /** One place where every verb runs: it owns the busy flag, the error
   * surface and the refresh, so no individual handler can forget one and
   * leave the rail lying about its state. */
  const run = useCallback(async (verb: () => Promise<unknown>): Promise<void> => {
    setBusy(true)
    setError(undefined)
    try {
      await verb()
      setDraft({ kind: 'none' })
      // No refetch here on purpose. Every one of these verbs writes the
      // manifest, chiefd pushes an `org-manifest` doc event, and
      // `OrgStoreProvider` re-reads the tree from that push — so the rail
      // updates from the served state without polling or a second read
      // (mandate 1).
    } catch (caught) {
      // apps/api's refusals carry chiefd's own code and detail. Showing that
      // verbatim is the point — "a direct operator has no durable
      // hiring-manager route" tells the operator what to do; "failed" does
      // not.
      setError(caught instanceof Error ? caught.message : String(caught))
    } finally {
      setBusy(false)
    }
  }, [])

  const close = useCallback((): void => {
    setDraft({ kind: 'none' })
    setError(undefined)
  }, [])

  /** Pause and resume take no form and no operator input — a unit is running
   * or it is not — so they go straight through `run` rather than opening a
   * draft. They still share its busy flag, its error surface and its
   * refresh-from-the-served-tree semantics; the tree's `state` field is what
   * flips the button, and that comes back from chiefd's `org-manifest` push
   * like every other structural change here. */
  const pause = useCallback(
    (departmentId: string): void => {
      void run(async () => applied(await api.pauseDepartment(props.companyKey, departmentId)))
    },
    [api, props.companyKey, run]
  )

  const resume = useCallback(
    (departmentId: string): void => {
      void run(async () => applied(await api.resumeDepartment(props.companyKey, departmentId)))
    },
    [api, props.companyKey, run]
  )

  return (
    <section aria-label="Structure" data-structure-rail="true">
      <h2>Structure</h2>

      {typeof error === 'undefined' ? null : (
        <p role="alert" data-structure-error="true" className="chief-error">
          {error}
        </p>
      )}

      {props.readOnly ? (
        <p data-structure-readonly="true" className="chief-note">
          This company is not running. Boot it to change its structure.
        </p>
      ) : (
        <button
          type="button"
          disabled={busy || typeof rootId === 'undefined'}
          onClick={(): void => {
            if (typeof rootId === 'undefined') return
            setDraft({ kind: 'new-department', parentId: rootId })
          }}
        >
          New department
        </button>
      )}

      <ul data-department-list="true" className="chief-tree">
        {rows.map(({ node, depth }) => (
          <li
            key={node.id}
            data-department-id={node.id}
            data-department-state={node.state}
            style={{ marginLeft: depth * 12 }}
          >
            <span data-department-name={node.name}>{node.name}</span>{' '}
            <span data-department-headcount={node.people.length}>({node.people.length})</span>
            {/* Named, not styled away: a paused unit still exists, and an
                operator who cannot see that it is paused has no reason to
                press Resume. */}
            {node.state === 'paused' ? ' · paused' : ''}
            {props.readOnly ? null : (
              <>
                {' '}
                <button
                  type="button"
                  disabled={busy}
                  onClick={(): void => setDraft({ kind: 'hire', departmentId: node.id })}
                >
                  Hire
                </button>{' '}
                {node.id === rootId ? null : (
                  <>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={(): void => setDraft({ kind: 'reparent', node })}
                    >
                      Move
                    </button>{' '}
                    {/* One button, never two: a department is active or
                        paused, and offering both would let an operator press
                        the one that is already true. Neither is offered for
                        the ROOT — chiefd refuses that with
                        `exec-root-protected`, and the same reasoning already
                        hides Move there: do not offer a choice that can only
                        fail.
                        No confirmation dialog, unlike Stop and Offboard:
                        pausing has an exact inverse sitting in its place the
                        moment it lands. */}
                    {node.state === 'paused' ? (
                      <button
                        type="button"
                        data-department-resume={node.id}
                        disabled={busy}
                        onClick={(): void => resume(node.id)}
                      >
                        Resume
                      </button>
                    ) : (
                      <button
                        type="button"
                        data-department-pause={node.id}
                        disabled={busy}
                        onClick={(): void => pause(node.id)}
                      >
                        Pause
                      </button>
                    )}
                  </>
                )}
              </>
            )}
            <ul>
              {node.people.map((person) => (
                <li
                  key={person.id}
                  data-person-id={person.id}
                  data-employment-state={person.employmentState}
                >
                  <span>{person.name}</span> <span>{person.title}</span>
                  {/* Named, not styled away, for the same reason a paused
                      department says so: somebody who has left still appears
                      here — the manifest keeps them and the tree places them —
                      and an operator who cannot see that they left has no way
                      to tell the roster from the alumni. */}
                  {person.employmentState === 'departed' ? ' · departed' : ''}
                  {person.employmentState === 'benched' ? ' · benched' : ''}
                  {/* Departed people get no verbs. Showing the state without
                      withdrawing Transfer and Offboard would label the hazard
                      and leave it — and both are choices that can only fail:
                      chiefd routes transfer AND offboard through
                      `movable_worker`, which refuses a departed target with
                      `PERSON_DEPARTED` ("Cannot transfer/offboard departed
                      person"). The same reasoning already hides Move and
                      Pause on the root. */}
                  {props.readOnly ||
                  person.id === node.headPersonId ||
                  person.employmentState === 'departed' ? null : (
                    <>
                      {' '}
                      <button
                        type="button"
                        disabled={busy}
                        onClick={(): void =>
                          setDraft({ kind: 'transfer', person, fromDepartmentId: node.id })
                        }
                      >
                        Transfer
                      </button>{' '}
                      <button
                        type="button"
                        disabled={busy}
                        onClick={(): void => setDraft({ kind: 'offboard', person })}
                      >
                        Offboard
                      </button>
                    </>
                  )}
                </li>
              ))}
            </ul>
          </li>
        ))}
      </ul>

      {draft.kind === 'new-department' ? (
        <NewDepartmentForm
          departments={rows}
          initialParentId={draft.parentId}
          busy={busy}
          onCancel={close}
          onSubmit={(request): void => {
            void run(() => api.createDepartment(props.companyKey, request))
          }}
        />
      ) : null}

      {draft.kind === 'hire' ? (
        <HireForm
          departmentId={draft.departmentId}
          busy={busy}
          onCancel={close}
          onSubmit={(request): void => {
            void run(() => api.hirePerson(props.companyKey, request))
          }}
        />
      ) : null}

      {draft.kind === 'transfer' ? (
        <TransferForm
          person={draft.person}
          // A transfer to the department the person is already in is not a
          // move; excluding it keeps the only offered choices real ones.
          destinations={rows.filter((row) => row.node.id !== draft.fromDepartmentId)}
          busy={busy}
          onCancel={close}
          onSubmit={(destinationId, reason): void => {
            void run(() =>
              api.transferPerson(props.companyKey, draft.person.id, destinationId, reason)
            )
          }}
        />
      ) : null}

      {draft.kind === 'reparent' ? (
        <ReparentForm
          node={draft.node}
          destinations={rows.filter((row) => !subtreeIds(draft.node).has(row.node.id))}
          busy={busy}
          onCancel={close}
          onSubmit={(newParentId): void => {
            void run(() => api.reparentDepartment(props.companyKey, draft.node.id, newParentId))
          }}
        />
      ) : null}

      {draft.kind === 'offboard' ? (
        <OffboardForm
          person={draft.person}
          busy={busy}
          onCancel={close}
          onSubmit={(reason): void => {
            void run(() => api.offboardPerson(props.companyKey, draft.person.id, reason))
          }}
        />
      ) : null}
    </section>
  )
}

function DepartmentOptions({ rows }: { rows: readonly FlatDepartment[] }): ReactElement {
  return (
    <>
      {rows.map(({ node, depth }) => (
        <option key={node.id} value={node.id}>
          {'— '.repeat(depth)}
          {node.name}
        </option>
      ))}
    </>
  )
}

function NewDepartmentForm(props: {
  departments: readonly FlatDepartment[]
  initialParentId: string
  busy: boolean
  onCancel: () => void
  onSubmit: (request: {
    name: string
    purpose: string
    parentId: string
    head: { name: string; mandate: string }
  }) => void
}): ReactElement {
  const [name, setName] = useState('')
  const [purpose, setPurpose] = useState('')
  const [parentId, setParentId] = useState(props.initialParentId)
  const [headName, setHeadName] = useState('')
  const [headMandate, setHeadMandate] = useState('')

  return (
    <form
      aria-label="New department"
      data-structure-form="new-department"
      className="chief-form chief-form--rail"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault()
        props.onSubmit({
          name,
          purpose,
          parentId,
          // No title. It is not asked for AND not derived here: chiefd fills a
          // blank head title with `Head of <name>` using the same rule genesis
          // uses, so deriving the same string in the browser would be a second
          // copy of it that can only drift.
          head: { name: headName, mandate: headMandate }
        })
      }}
    >
      <label>
        Name
        <input value={name} onChange={(e): void => setName(e.target.value)} required />
      </label>
      <label>
        Purpose
        <input value={purpose} onChange={(e): void => setPurpose(e.target.value)} required />
      </label>
      <label>
        Reports to
        <select value={parentId} onChange={(e): void => setParentId(e.target.value)}>
          <DepartmentOptions rows={props.departments} />
        </select>
      </label>
      <label>
        Head&apos;s name
        <input value={headName} onChange={(e): void => setHeadName(e.target.value)} required />
      </label>
      <label>
        Head&apos;s mandate
        <input
          value={headMandate}
          onChange={(e): void => setHeadMandate(e.target.value)}
          required
        />
      </label>
      <button type="submit" disabled={props.busy}>
        {props.busy ? 'Creating…' : 'Create department'}
      </button>
      <button type="button" onClick={props.onCancel} disabled={props.busy}>
        Cancel
      </button>
    </form>
  )
}

function HireForm(props: {
  departmentId: string
  busy: boolean
  onCancel: () => void
  onSubmit: (request: {
    departmentId: string
    name: string
    title: string
    mandate: string
  }) => void
}): ReactElement {
  const [name, setName] = useState('')
  const [title, setTitle] = useState('')
  const [mandate, setMandate] = useState('')

  return (
    <form
      aria-label="Hire"
      data-structure-form="hire"
      className="chief-form chief-form--rail"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault()
        props.onSubmit({ departmentId: props.departmentId, name, title, mandate })
      }}
    >
      <label>
        Name
        <input value={name} onChange={(e): void => setName(e.target.value)} required />
      </label>
      <label>
        Title
        <input value={title} onChange={(e): void => setTitle(e.target.value)} required />
      </label>
      <label>
        Mandate
        <input value={mandate} onChange={(e): void => setMandate(e.target.value)} required />
      </label>
      <button type="submit" disabled={props.busy}>
        {props.busy ? 'Hiring…' : `Hire into ${props.departmentId}`}
      </button>
      <button type="button" onClick={props.onCancel} disabled={props.busy}>
        Cancel
      </button>
    </form>
  )
}

function TransferForm(props: {
  person: TreePerson
  destinations: readonly FlatDepartment[]
  busy: boolean
  onCancel: () => void
  onSubmit: (destinationId: string, reason: string) => void
}): ReactElement {
  const [destinationId, setDestinationId] = useState(props.destinations[0]?.node.id ?? '')
  const [reason, setReason] = useState('')

  return (
    <form
      aria-label="Transfer"
      data-structure-form="transfer"
      className="chief-form chief-form--rail"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault()
        props.onSubmit(destinationId, reason)
      }}
    >
      <p>Transfer {props.person.name}</p>
      <label>
        To
        <select
          value={destinationId}
          onChange={(e): void => setDestinationId(e.target.value)}
          required
        >
          <DepartmentOptions rows={props.destinations} />
        </select>
      </label>
      <label>
        Reason
        <input value={reason} onChange={(e): void => setReason(e.target.value)} required />
      </label>
      <button type="submit" disabled={props.busy || destinationId === ''}>
        {props.busy ? 'Transferring…' : 'Transfer'}
      </button>
      <button type="button" onClick={props.onCancel} disabled={props.busy}>
        Cancel
      </button>
    </form>
  )
}

function ReparentForm(props: {
  node: DepartmentNode
  destinations: readonly FlatDepartment[]
  busy: boolean
  onCancel: () => void
  onSubmit: (newParentId: string) => void
}): ReactElement {
  const [newParentId, setNewParentId] = useState(props.destinations[0]?.node.id ?? '')

  return (
    <form
      aria-label="Move department"
      data-structure-form="reparent"
      className="chief-form chief-form--rail"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault()
        props.onSubmit(newParentId)
      }}
    >
      <p>Move {props.node.name}</p>
      <label>
        Under
        <select value={newParentId} onChange={(e): void => setNewParentId(e.target.value)} required>
          <DepartmentOptions rows={props.destinations} />
        </select>
      </label>
      <button type="submit" disabled={props.busy || newParentId === ''}>
        {props.busy ? 'Moving…' : 'Move'}
      </button>
      <button type="button" onClick={props.onCancel} disabled={props.busy}>
        Cancel
      </button>
    </form>
  )
}

function OffboardForm(props: {
  person: TreePerson
  busy: boolean
  onCancel: () => void
  onSubmit: (reason: string) => void
}): ReactElement {
  const [reason, setReason] = useState('')

  return (
    <form
      aria-label="Offboard"
      data-structure-form="offboard"
      className="chief-form chief-form--rail"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault()
        props.onSubmit(reason)
      }}
    >
      {/* A reason is required rather than optional: offboarding is the one
          verb here with no inverse, and the durable record is the only place
          the "why" can still be read afterwards. */}
      <p>Offboard {props.person.name}. This cannot be undone.</p>
      <label>
        Reason
        <input value={reason} onChange={(e): void => setReason(e.target.value)} required />
      </label>
      <button type="submit" disabled={props.busy}>
        {props.busy ? 'Offboarding…' : 'Offboard'}
      </button>
      <button type="button" onClick={props.onCancel} disabled={props.busy}>
        Cancel
      </button>
    </form>
  )
}
