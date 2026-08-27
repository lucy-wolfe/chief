// Types for SettingsClient — the `org_settings` singleton (E7-S3, #818).
// Replaces `state/launcher.json`: `launcherRoot` is now a column on this
// existing row instead of a file the launcher wrote and read directly.
// Rust authority: chiefd-api/src/docstore/router.rs `OrgSettingsDto`
// (`#[serde(rename_all = "camelCase")]`).

export interface OrgSettings {
  /** The absolute source-checkout path that last materialized this company.
   * `undefined` when never published (a live company that has not yet had a
   * materialization pass record one). */
  launcherRoot?: string
  supervisionIntervalMs: number
  acknowledgementTimeoutMs: number
  acknowledgementRetryLimit: number
  replacementLimit: number
}
