/**
 * The only tool names a person seed may declare.
 *
 * Every person receives this Pi builtin floor automatically. A seed may repeat
 * one of these names, but it must never name an `org_*` tool: organization
 * tools are composed from the person's live role and subtree scope below.
 */
export const BUILTIN_TOOLS = ['read', 'bash', 'edit', 'write', 'grep', 'find', 'ls'] as const

/** Materialized active Pi homes advertise the full normal tool surface. */
export const ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES = [
  'org_send',
  'org_roster',
  'org_create_reminder',
  'org_list_reminders',
  'org_stop_reminder'
] as const

/**
 * The subtree-growth surface — every person carries it, whatever their kind.
 *
 * # Why this list exists apart from the manager list
 *
 * The product rule is that EVERY LEAF CAN BECOME A PARENT: anyone may create a
 * department beneath themselves, become its head, and staff it. The authority
 * layer already implements exactly that (`authorityRootDepartmentId` in
 * `organization-intercom.ts` — "creating a child unit takes no authority over
 * anybody who already exists, and the creator becomes the new unit's head"),
 * and every handler below checks SUBTREE SCOPE rather than a job title.
 *
 * The catalog did not agree. All thirty structural tools sat behind one
 * `isManager` gate, so a worker's pane was launched without `org_add_department`
 * at all and the mandate was unreachable in practice: the authority layer said
 * yes and the model was never offered the tool to ask with.
 *
 * The split is by WHAT THE HANDLER ENFORCES, which is the only line that can
 * stay true. A tool whose handler checks the caller's KIND belongs in
 * [`ORGANIZATION_MANAGER_TOOL_NAMES`], because offering it to a leaf would
 * offer a tool that can never succeed. A tool whose handler
 * checks `departmentIsInScope` belongs HERE, because for a leaf it refuses
 * today and succeeds the moment that leaf heads a unit — a state, not a
 * permanent condition, and the refusals in between are the safety model
 * working.
 *
 * Granting these takes authority over nobody: a leaf heads no unit, so
 * `departmentIsInScope` is empty for it and every one of these verbs refuses
 * until it creates a unit of its own. Growth is DOWNWARD only; nothing here
 * reaches sideways at a peer or upward at a manager.
 */
export const ORGANIZATION_SUBTREE_TOOL_NAMES = [
  'org_launch_department',
  'org_stop_department',
  'org_remove_department',
  'org_launch_contract',
  'org_stop_contract',
  'org_remove_contract',
  'org_add_department',
  'org_pause_department',
  'org_resume_department',
  'org_resume_departments',
  'org_hire',
  'org_bench',
  'org_recall',
  'org_start_person',
  'org_stop_person',
  'org_transfer',
  'org_offboard',
  'org_reparent_department',
  'org_move_department_members',
  'org_appoint_department_head',
  // `org_lifecycle_status` used to be role-gated. It is here because it is now
  // fenced SERVER-SIDE and the catalog no longer has to stand in for a check
  // the daemon does itself: it reaches a board whose scope is derived from the
  // caller instead of chosen by it.
  //
  // TOMBSTONE: `org_maintain_session` sat beside it for the same reason. The
  // whole tool is deleted — operator ruling, 2026-08-24.
  'org_lifecycle_status'
] as const

/**
 * THE ROLE-GATED SURFACE IS EMPTY, AND THAT IS THE POINT.
 *
 * It held tools whose handlers read the caller's KIND and refused a
 * non-manager outright: the lifecycle control board and session maintenance.
 * They were a deliberate exception to the rule that authority is the subtree
 * you head and never the job title, and the exception was justified by ROUTES
 * THAT ENFORCED NOTHING — the TypeScript check WAS the authorization rather
 * than a pre-flight in front of one.
 *
 * That premise is gone. Every one of them is fenced server-side now, so they
 * moved to [`ORGANIZATION_SUBTREE_TOOL_NAMES`] and this list emptied.
 *
 * DO NOT DELETE THIS AS DEAD CODE. It stays at zero entries on purpose,
 * because it is the third gate's authority: `resource_catalog.rs`'s
 * `MANAGER_TOOLS` is pinned equal to it, so an empty list that is CHECKED is a
 * stronger statement than a deleted one — it says "no tool is granted by kind"
 * and FAILS the moment somebody adds one back without an argument. Delete it
 * and the parity guard goes with it, which is how a retired model quietly
 * returns: the thing being guarded and the guard leave in the same tidy-up.
 */
export const ORGANIZATION_MANAGER_TOOL_NAMES = [] as const

/**
 * The verbs that speak to or for the OPERATOR, about the company as a whole.
 *
 * Not a role gate, and this list must never become one. Every name here is
 * fenced server-side by the same subtree question every other verb asks — the
 * routes ask whether the caller heads the ROOT department, because each of
 * these writes reaches every person in the company. What makes them belong
 * together is their subject: the company, and the person outside it.
 *
 * `org_stand_down` and `org_resume` joined `org_escalate_to_operator` here
 * after a live company was told to stop all work and could not. The CEO obeyed
 * the instruction exactly — it parked six people and said so — and forty-five
 * seconds later every one of them was back, re-granted by the mail they had
 * queued to each other. Stopping people one at a time is a decision about
 * PEOPLE and never adds up to a decision about the COMPANY, so no number of
 * `org_stop_person` calls could have expressed what the operator asked for.
 * See `chiefd_core::store::stand_down`.
 */
export const ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES = [
  'org_escalate_to_operator',
  'org_stand_down',
  'org_resume'
] as const
