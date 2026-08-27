/**
 * A managed Pi turn must periodically return control so durable organization
 * mail can enter the session. Long-lived services and clock waits belong in a
 * durable schedule or detached supervised process, never one foreground tool.
 */
export const ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS = 4 * 60;

export { organizationForegroundResponsivenessContract } from "@chief/piing/extension-runtime";
