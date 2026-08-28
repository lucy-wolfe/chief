import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { LaunchTrace } from "@chief/piing/extension-runtime";
// Company genesis is chiefd's. This is the only way an extension reaches it —
// the CLI-subprocess bridge that used to sit here is deleted.
import {
  FounderLaunchClient,
  founderUrlFromEnvironment,
  type FounderLaunchPhase,
  type FounderLaunchResult,
} from "@chief/chiefing/extension-runtime";
import { CARD_GLYPHS } from "./card-style";

/**
 * What this tool reports back to the model. `slug`/`session` exist only on a
 * successful launch — they are declared optional so the refusal branch is a
 * genuine member of the SAME result type rather than widening it into a
 * union whose `details.slug` is `string | undefined` at every read site.
 */
interface FounderLaunchToolDetails {
  ok: boolean;
  slug?: string;
  session?: string;
  /** Was the operator's terminal actually moved to the CEO? Success-only. */
  handedOver?: boolean;
  /** The step running right now. Present only on an in-flight update. */
  phase?: string;
}

/**
 * What each chiefd phase is called in front of a human (#1051).
 *
 * A launch takes minutes and the pane used to show `⠹ Working...` for all of
 * it, so the human could not tell a long step from a hung one. These sentences
 * say what is happening in plain words; chiefd's own phase name is kept in
 * `details.phase` for anything that reads structure.
 *
 * An unknown phase falls back to its raw name rather than being dropped. A
 * chiefd that learns a new step must never make this pane go quiet again —
 * that is the whole defect, and a silent default would rebuild it.
 */
const PHASE_LABELS: Readonly<Record<string, string>> = {
  preflight: "Checking this host is ready",
  "beacond-ensure": "Checking the company directory",
  // Two phases, not one, because they are two very different waits: the
  // directory answering a probe is instant, and STARTING it spawns a process
  // and waits on a budget.
  "beacond-starting": "Starting the company directory",
  "beacond-ready": "The company directory is up",
  "company-claim": "Claiming the company name",
  "company-daemon-start": "Starting the company daemon",
  "company-daemon-ready": "The company daemon is up",
  "durable-create": "Creating the company",
  "durable-create-complete": "The company is durable",
  "durable-create-failed": "The company could not be created",
  "company-daemon-stop": "Stopping the daemon after a failed create",
  "company-daemon-stopped": "The daemon is stopped",
  "company-daemon-stop-failed": "The daemon could not be stopped",
  // TOMBSTONE (chief-home-is-cwd §4c): "ceo-prepare" / "ceo-prepare-failed".
  // The phases they labelled are deleted with the daemon-side CEO boot. This
  // map falls back to the raw phase name for anything it does not know, so a
  // stale label here would be the WORSE failure of the two: it would render a
  // confident sentence about a step nothing performs.
  "chief-start": "Starting the CEO",
  "chief-start-failed": "The CEO could not be started",
  handover: "Bringing the company up and moving you to the CEO",
  "handover-complete": "The handover is finished",
  // The one step chiefd cannot name, because it happens in this process before
  // its stream exists.
  starting: "Starting",
};

/**
 * How often the progress line is refreshed while nothing else has happened.
 *
 * One second, because the number it moves is in seconds. Cheap: it rewrites one
 * tool row and touches no I/O.
 */
const PROGRESS_TICK_MS = 1_000;

/** The label for one phase, never empty. */
export function launchPhaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? phase;
}

/**
 * The progress line the pane shows while the launch runs.
 *
 * The elapsed seconds are the load-bearing half. A phase name alone still looks
 * frozen when one step owns four minutes of a four-and-a-half minute launch;
 * a number that keeps climbing is what tells a human the difference between
 * slow and stuck.
 */
export function launchProgressText(name: string, phase: string, elapsedMs: number): string {
  const seconds = Math.max(0, Math.round(elapsedMs / 1_000));
  const elapsed = seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  return `⏳ Launching ${name} · ${launchPhaseLabel(phase)} · ${elapsed}`;
}

/**
 * Turn chiefd's launch outcome into what the operator is told.
 *
 * # Why the handoff claim is conditional
 *
 * chiefd is the only thing that knows whether the operator's operator client was
 * actually moved to the CEO, and it says so by putting `handoffWarning` on the
 * outcome when it was not. This tool used to announce
 * "CEO booted in its ChiefD runtime session" on every success, so a launch that
 * handed nobody over read, to an operator still sitting in the Founder pane, as
 * a completed handover. Reporting a place the operator is not is worse than
 * reporting nothing: it is the one claim they cannot check without looking, and
 * they had no reason to look.
 *
 * # And why the warning branch states NOTHING about whether the CEO is running
 *
 * It used to state two things, and lost one of them each time chiefd changed
 * underneath it. First "The CEO is booted in tmux session X, but you were NOT
 * moved there" — false after the actuation switchover, because a company
 * created by `chief` had a daemon, a manifest and a CEO-only intent and
 * NOBODY running. Then "the CEO is NOT running" — false now that chiefd starts
 * the actuator, waits for the lease, states the intent and waits for the
 * company session before it hands anybody over: a handover that moves nobody
 * (an unattended launch has no client to move) leaves a company that IS
 * running.
 *
 * A tool on the other side of an HTTP boundary cannot know which of those is
 * true, and every version that guessed was eventually wrong in the operator's
 * favour-losing direction. So this branch now asserts only what this process
 * observed — the company was CREATED, and the operator was not moved — and
 * chiefd's own `handoffWarning`, which is the only thing that watched the
 * bring-up, says what is running and what to do next.
 */
export function reportLaunch(
  name: string,
  launched: FounderLaunchResult,
): { text: string; details: FounderLaunchToolDetails } {
  const warning = launched.handoffWarning?.trim();
  const details: FounderLaunchToolDetails = {
    ok: true,
    slug: launched.slug,
    session: launched.session,
    handedOver: !warning,
  };
  const text = warning
    ? `✅ Company created · ${name}\nYou were NOT moved to the CEO. ${warning}`
    : `✅ Company launched · ${name}\nYou are now in the CEO of ${name} (tmux session ${launched.session}). The CEO can now create departments through organization tools.`;
  return { text, details };
}

/**
 * What the operator is told when a launch did not complete.
 *
 * # It states only that, and lets chiefd's own sentence say the rest
 *
 * This used to prepend, to every failure, "No company was created and no CEO
 * handoff occurred. Do not treat this company as launched or running." chiefd
 * answers 422 for outcomes where a company WAS created — `genesis.rs` has
 * "created '<slug>' but its CEO could not be prepared … registered and durable
 * but not running. Recover with: chief attach <slug>" — so the tool
 * overwrote an accurate sentence with a false one and told the operator to
 * ignore a company that exists. A client timeout makes it worse still, because
 * then this side genuinely does not know.
 *
 * That is the rule [`reportLaunch`] already states for the success branch,
 * applied to the branch that never got it: a tool on the other side of an HTTP
 * boundary cannot know which of those is true, and every version that guessed
 * was eventually wrong. The reason is passed through verbatim, because the
 * recovery command inside it is the operator's next move.
 */
export function reportLaunchFailure(
  name: string,
  reason: string,
): { text: string; details: FounderLaunchToolDetails } {
  return {
    text: `${CARD_GLYPHS.crossMark} Company launch did not complete · ${name}\n${reason}`,
    details: { ok: false },
  };
}

/**
 * Founder-only launch authority. The Founder never receives a shell
 * create/boot workflow, and there is no second pre-company identity.
 *
 * # Everything below the confirmation is Rust
 *
 * This tool captures the one fact a Pi extension is the only thing that can
 * observe — the confirmed name/purpose the human agreed to — and hands it to
 * chiefd. The slug, the durable company claim, the daemon, the manifest, the
 * CEO and the boot are all chiefd's decisions.
 *
 * It used to spawn `triber _create-and-boot --spec <tmpfile>
 * --founder-bootstrap <tmpfile>` and infer a launch from an exit code. That
 * bridge is deleted, with the temp files, the subprocess and the retired
 * command namespace.
 */
export default function founderLaunch(pi: ExtensionAPI) {
  // The stream, opened once per Founder session. Its schema, its directory and
  // its rotation bound are `chiefd-log`'s, so this file and the Rust ones merge
  // on timestamp in one `~/.chiefd/logs/` listing.
  const trace = new LaunchTrace({ environment: process.env });
  // Written at extension load, before any session exists, for two reasons an
  // operator needs. It NAMES THE FILE, so a launch that fails before
  // `session_start` — the refusal branch below has exactly that shape — still
  // leaves a record that says where to look. And it timestamps extension load,
  // which is the only in-process mark before the session opens.
  trace.event("info", "founder.trace.open", { stream: trace.path });
  pi.on("session_start", () => {
    // Comparing this line against the transcript's own `toolCall` timestamp is
    // what measures the one segment no in-process instrument can see: the gap
    // before `execute` is entered.
    trace.event("info", "founder.session.start", { stream: trace.path });
  });
  pi.on("session_shutdown", () => {
    trace.event("info", "founder.session.shutdown", {});
  });
  pi.registerTool({
    name: "chiefd_launch_company",
    label: "Launch company",
    description: "Create a durable company from its confirmed name and purpose, then boot only its CEO. This is the sole company-launch action available to Founder mode.",
    parameters: Type.Object({
      name: Type.String({ minLength: 2, maxLength: 120, description: "Confirmed company name" }),
      purpose: Type.String({ minLength: 4, maxLength: 2_000, description: "Confirmed company purpose" }),
    }, { additionalProperties: false }),
    async execute(_toolCallId, params, _signal, onUpdate) {
      const name = params.name.trim();
      const purpose = params.purpose.trim();
      // TWO OUTPUTS FROM ONE SET OF BOUNDARIES (#1052 + #1051). The trace
      // records below are for whoever reads the log afterwards; the progress
      // reports are for the human waiting right now. They are taken at exactly
      // the same points, deliberately: a step worth timing after the fact is
      // exactly a step worth showing during it, and two instruments that
      // drifted apart would describe one launch two ways.
      //
      // Everything from here to the return is timed. A launch measured live
      // spent 98.2% of 143 seconds inside this function before it opened its
      // first socket to chiefd, and wrote nothing anywhere while it did.
      const launch = trace.step("founder.launch", { name, purposeLength: purpose.length });
      // #1051: THE PANE MUST NEVER GO QUIET. That same launch showed
      // `⠹ Working...` and nothing else, so the operator concluded it had hung.
      // Pi hands every `execute` an `onUpdate` that streams a partial result to
      // the tool row; this tool ignored it and took only `(toolCallId, params)`.
      const startedAt = Date.now();
      let phase = "starting";
      const report = () => {
        onUpdate?.({
          content: [{ type: "text", text: launchProgressText(name, phase, Date.now() - startedAt) }],
          details: { ok: false, phase },
        });
      };
      // A ticker, not just a per-phase report. The phases are minutes apart —
      // that IS the problem — so a line that only moves when a phase changes is
      // frozen for exactly as long as the wait a human is trying to read.
      const tick = setInterval(report, PROGRESS_TICK_MS);
      // `unref` where the runtime has it: a progress timer must never be the
      // reason a process stays alive after the launch it narrates is over.
      (tick as unknown as { unref?: () => void }).unref?.();
      try {
        const client = trace.measureSync("founder.endpoint.resolve", () =>
          new FounderLaunchClient({ url: founderUrlFromEnvironment(process.env) }));
        phase = "beacond-ensure";
        report();
        const launched = await trace.measure(
          "founder.chiefd.launch",
          () =>
            client.launch({ name, purpose }, (frame: FounderLaunchPhase) => {
              // chiefd's own phases, forwarded to the pane as they arrive. The
              // trace records the whole call's duration; this reports where
              // inside it the launch currently is.
              phase = frame.phase;
              report();
            }),
        );
        // From here the lines name the company, so one grep spans both
        // runtimes' records of the same launch.
        trace.nameCompany(launched.slug);
        const reported = reportLaunch(name, launched);
        launch.close({
          ok: true,
          slug: launched.slug,
          session: launched.session,
          handedOver: reported.details.handedOver === true,
        });
        return { content: [{ type: "text", text: reported.text }], details: reported.details };
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error);
        // A refusal is measured too: the step that failed already wrote its own
        // duration, and this closes the launch with the reason beside it.
        launch.fail(error, { ok: false });
        const reported = reportLaunchFailure(name, reason);
        return {
          content: [{ type: "text", text: reported.text }],
          details: reported.details,
          isError: true,
        };
      } finally {
        clearInterval(tick);
      }
    },
  });
}
