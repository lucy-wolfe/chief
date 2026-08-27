# Live recovery runbook

Use this document only for a current build of chiefd. Historical
incident procedures live in the dated entries of `CHANGELOG.md` and
`DECISIONS.md`; do not revive them as operating instructions.

## Recovery principles

- ChiefD's normalized rows are authoritative. There is no `org.json`; the files
  below `people/` are Pi's own artifacts and derived projections, and must never
  be hand-edited to repair durable state.
- Use the named ChiefD/CLI operation that owns the state you need to change.
  Structural staffing, lifecycle, and runtime-preference operations validate
  current state inside their database transaction.
- Do not compare, supply, restore, or synthesize a global organization
  counter. Event records identify committed history; they are not recovery
  inputs or mutation preconditions.
- Start only the people explicitly needed for the recovery. The CEO-only path
  is the safe default while the organization is being diagnosed.

## Safe recovery sequence

1. Resolve the organization's recorded data root. Do not infer it from the
   checkout in the current shell.
2. Take a filesystem snapshot of the organization's derived projection and a
   consistent SQLite backup before any mutation.
3. Read current state through ChiefD's supported inspection surface. Verify the
   hierarchy, employment state, lifecycle transitions, active goals, and tmux
   ownership agree semantically.
4. Apply the narrow named operation for the fault, then re-read those same
   facts. If the fault needs a raw database repair, stop and escalate with the
   snapshot and exact invariant that failed.
5. Rebuild derived projections through the supported materialization or runtime
   command. Confirm the required person's contract MD5 and runtime ownership;
   launch no additional people unless an explicit recovery decision requires it.

## Escalation record

Capture the company slug, current command output, the named invariant that
failed, the backup location, and the exact operation attempted. Do not include
secrets, raw provider credentials, or copied private prompts in the incident
record.
