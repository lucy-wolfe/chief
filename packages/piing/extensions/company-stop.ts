import { spawn as nodeSpawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * `/stop` — tear the whole company down from inside any pane.
 *
 * # Why this command is not called `quit`
 *
 * Pi owns `/quit`. Its interactive mode matches 26 command names with a
 * literal `if (text === "/quit")` chain and RETURNS from that chain before it
 * ever asks `isExtensionCommand(text)` — measured on the installed 0.80.10
 * (`dist/modes/interactive/interactive-mode.js:2231`) and on upstream 0.84.2
 * (`src/modes/interactive/interactive-mode.ts:3090`, the extension check first
 * appearing at 3116). An extension that registers `quit` is therefore never
 * consulted, and would look broken rather than absent. `/stop` is not one of
 * the 26, so it reaches this file.
 *
 * Nor is there a "the operator quit" event to bind to instead:
 * `session_shutdown` is the only shutdown event Pi emits and it ALSO fires on
 * session switch, fork, clone and reload. Stopping a company there would end
 * the working day for everybody because one person forked a session.
 *
 * # Why this shells out instead of calling chiefd
 *
 * `chief stop` obeys an ordering law (`chief-cli/src/stop.rs`): the durable
 * teardown commits through the LIVE daemon, then the tmux session dies, then
 * the daemon does. This extension runs inside the tmux session that step two
 * kills, so it cannot perform the sequence itself — it would die halfway and
 * leave the daemon running with the company half torn down. The work has to go
 * to a process outside the session, and the process that already implements
 * the law correctly is the `chief` binary. Re-implementing the law behind a
 * new daemon route would give it a second home, which is the failure this
 * codebase has paid for more than once.
 */

/** The command name. Never `quit`: Pi owns that one and never delegates it. */
export const STOP_COMMAND_NAME = "stop";

/** The company directory a pane is stamped with. */
export const ORG_DIR_ENVIRONMENT_NAME = "ORG_LAUNCHER_ORG_DIR";

/** What the confirmation dialog is titled. */
export const CONFIRM_TITLE = "Stop the whole company?";

/**
 * What the operator is asked, in full.
 *
 * It names the blast radius rather than the mechanism, because the person
 * reading it is deciding whether everyone else stops working, and "runs chief
 * stop" does not tell them that.
 */
export const CONFIRM_MESSAGE =
  "Every person in this company stops, the window closes, and the company daemon exits. " +
  "Nothing durable is lost — goals, mail, memory and assignments all survive — and " +
  "`chief` in this directory starts it again. This affects everyone, not just this pane.";

/** Said when the operator answers no. Silence would read as a hang. */
export const DECLINED_MESSAGE = "The company is still running.";

/** Said once the teardown has been handed off. */
export const HANDED_OFF_MESSAGE =
  "Stopping the company. Every pane closes in a moment.";

/**
 * The refusal for a pane that cannot name its own company.
 *
 * A pane with no company stamp must say so rather than guess, because the only
 * available guess — the process working directory — could name a DIFFERENT
 * company, and stopping the wrong company is the worst outcome this command
 * has.
 */
export const NO_COMPANY_REFUSAL =
  `This pane is not stamped with ${ORG_DIR_ENVIRONMENT_NAME}, so it cannot say which company to stop. ` +
  "Run `chief stop` in the company directory instead.";

/** The refusal for a context with no dialog to ask in. */
export const NO_UI_REFUSAL =
  "Stopping the company needs a confirmation and this session has no interactive UI. " +
  "Run `chief stop` in the company directory instead.";

/** Everything needed to run one stop, and nothing else. */
export interface StopPlan {
  /** The installed `chief` binary. */
  readonly binary: string;
  /** Its arguments. */
  readonly argv: readonly string[];
  /** The company directory the stop runs in — its identity. */
  readonly cwd: string;
}

/** The one shape this file needs from `spawn`; a test substitutes anything like it. */
export interface SpawnedLike {
  unref(): void;
  /**
   * The child's own lifecycle, when the spawner reports one.
   *
   * OPTIONAL because a test substitute need not have it — but production DOES,
   * and it is the whole of the receipt below. A detached spawn with
   * `stdio: "ignore"` is fire-and-forget: a binary that is missing, a
   * permission that is denied, a child that dies at birth all produce EXACTLY
   * the silence of a stop nobody ran.
   */
  on?(event: "error" | "exit", handler: (payload: unknown) => void): void;
}

/** Said when the stop could not even be launched. */
export function stopFailedToLaunch(why: string): string {
  return (
    `Stopping the company FAILED TO LAUNCH: ${why}. Nothing was stopped and the company is ` +
    "still running. Run `chief stop` in the company directory to see the error in full."
  );
}

/** Said when the stop launched and then died before finishing. */
export function stopExitedEarly(status: string): string {
  return (
    `The stop command exited (${status}) before reporting that it finished. Some of the ` +
    "company may still be running. Run `chief stop` in the company directory to see what " +
    "survived."
  );
}

/**
 * The two UI verbs this command needs.
 *
 * Declared here rather than taken whole from Pi so the command body can be
 * driven by a test without a type assertion. Pi's own command context is
 * structurally assignable to it, so production passes the real thing
 * unchanged.
 */
export interface StopUi {
  notify(message: string, type?: "info" | "warning" | "error"): void;
  confirm(title: string, message: string): Promise<boolean>;
}

/** The slice of a Pi command context this command reads. */
export interface StopContext {
  readonly hasUI: boolean;
  readonly ui: StopUi;
}

/**
 * The one Pi verb this file calls.
 *
 * Declared structurally, with {@link StopContext} as the handler's context, so
 * the registration can be exercised with an ordinary object instead of a
 * fabricated `ExtensionCommandContext`. Pi's real `ExtensionAPI` satisfies it.
 */
export interface CommandRegistrar {
  registerCommand(
    name: string,
    options: {
      description?: string;
      handler: (argumentText: string, ctx: StopContext) => Promise<void>;
    }
  ): void;
}

/** Everything the command body needs from outside itself. */
export interface StopDependencies {
  readonly environment: Record<string, string | undefined>;
  readonly home: () => string;
  readonly spawn: (plan: StopPlan) => SpawnedLike;
}

export interface CompanyStopOptions {
  /** Test seam: the pane environment. Defaults to `process.env`. */
  environment?: Record<string, string | undefined>;
  /** Test seam: the operator's home directory. Defaults to `os.homedir`. */
  home?: () => string;
  /** Test seam: substitutes the real detached spawn. */
  spawn?: (plan: StopPlan) => SpawnedLike;
}

/**
 * Where the installed `chief` lives.
 *
 * ONE definition, matching `chief-cli/src/paths.rs`. There is deliberately no
 * PATH lookup behind it: a pane's PATH is not chief's to reason about, and a
 * fallback that found some OTHER `chief` would stop some other company.
 */
export function chiefBinaryPath(home: string): string {
  return join(home, ".chief", "bin", "chief");
}

/**
 * Build the plan, or say why there is none.
 *
 * Returns the refusal as a string rather than throwing, so the caller renders
 * one kind of message for every failure the operator can actually cause.
 */
export function stopPlan(
  environment: Record<string, string | undefined>,
  home: string
): StopPlan | string {
  const cwd = environment[ORG_DIR_ENVIRONMENT_NAME]?.trim();
  if (cwd === undefined || cwd === "") {
    return NO_COMPANY_REFUSAL;
  }
  return { binary: chiefBinaryPath(home), argv: ["stop"], cwd };
}

/**
 * Hand the stop to a process that outlives this one.
 *
 * `detached` plus `unref` is the whole point: `chief stop` kills the tmux
 * session this pane lives in partway through its own sequence, so a child that
 * stayed in this process group would be killed with us and the daemon would
 * survive. stdio is ignored because there is no terminal left to write to by
 * the time the interesting half runs.
 */
export function spawnDetachedStop(plan: StopPlan): SpawnedLike {
  return nodeSpawn(plan.binary, [...plan.argv], {
    cwd: plan.cwd,
    detached: true,
    stdio: "ignore"
  });
}

/**
 * One run of `/stop`, start to finish.
 *
 * Split out of the registration so it can be driven directly: a test that had
 * to fabricate a whole Pi command context would be asserting against a type
 * assertion rather than against this behaviour.
 */
export async function runStopCommand(
  ctx: StopContext,
  dependencies: StopDependencies
): Promise<void> {
  const plan = stopPlan(dependencies.environment, dependencies.home());
  if (typeof plan === "string") {
    ctx.ui.notify(plan, "error");
    return;
  }
  if (!ctx.hasUI) {
    ctx.ui.notify(NO_UI_REFUSAL, "error");
    return;
  }
  const confirmed = await ctx.ui.confirm(CONFIRM_TITLE, CONFIRM_MESSAGE);
  if (!confirmed) {
    ctx.ui.notify(DECLINED_MESSAGE, "info");
    return;
  }
  // THE RECEIPT. SILENCE IS THE ONE OUTCOME THIS MUST NOT HAVE.
  //
  // The teardown itself is correct — traced end to end, and observed completing
  // in 200ms. What could fail invisibly was the DELIVERY: a detached spawn with
  // `stdio: "ignore"` whose failure produces no log line, no event and no
  // notification, so an invoked `/stop` that did nothing is indistinguishable
  // from one that was never invoked. The operator hit exactly that, four deploy
  // attempts in an afternoon, and ended up reaching for `pkill -f "chief"`.
  //
  // This is the same defect class as the fence key, the ownership `socketName`
  // and the click that logged a navigation it never made: code that works but
  // never reports when it doesn't. The handed-off line now comes AFTER a spawn
  // that actually started, and the two ways it can fail each say so.
  let child: SpawnedLike;
  try {
    child = dependencies.spawn(plan);
  } catch (error) {
    ctx.ui.notify(stopFailedToLaunch(stopSpawnReason(error)), "error");
    return;
  }
  child.on?.("error", (payload) => {
    ctx.ui.notify(stopFailedToLaunch(stopSpawnReason(payload)), "error");
  });
  child.on?.("exit", (payload) => {
    // A CLEAN EXIT IS SILENT, and must be: the teardown kills this pane's own
    // session, so the ordinary success path ends with nobody left to notify.
    // Only a non-zero status is news.
    const status = stopExitStatus(payload);
    if (status !== undefined) ctx.ui.notify(stopExitedEarly(status), "error");
  });
  child.unref();
  ctx.ui.notify(HANDED_OFF_MESSAGE, "info");
}

/** The most useful sentence available about a failed spawn. */
function stopSpawnReason(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "the operating system refused the command and said nothing";
}

/** A non-zero exit rendered for the operator, or `undefined` for a clean one. */
function stopExitStatus(payload: unknown): string | undefined {
  if (typeof payload === "number") return payload === 0 ? undefined : `status ${payload}`;
  return undefined;
}

export default function companyStop(
  pi: CommandRegistrar,
  options: CompanyStopOptions = {}
): void {
  const dependencies: StopDependencies = {
    environment: options.environment ?? process.env,
    home: options.home ?? homedir,
    spawn: options.spawn ?? spawnDetachedStop
  };

  pi.registerCommand(STOP_COMMAND_NAME, {
    description: "Stop this whole company — every person, the window, the daemon",
    handler: async (_arguments, ctx) => {
      await runStopCommand(ctx, dependencies);
    }
  });
}
