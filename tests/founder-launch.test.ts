import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  FOUNDER_URL_ENV,
  FounderLaunchClient,
  founderUrlFromEnvironment,
} from "../packages/chiefing/src/resources/FounderLaunch";
import {
  observeFounderBootstrap,
  reportLaunchFailure,
} from "../packages/piing/extensions/founder-launch";

const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));

function extensionSource(): string {
  return readFileSync(join(REPO_ROOT, "packages/piing/extensions/founder-launch.ts"), "utf8");
}

describe("Founder launch reaches chiefd, not a CLI subprocess", () => {
  test("the extension spawns nothing and writes nothing", () => {
    // The deleted bridge, stated as the shapes that would mean it came back:
    // a child process, a temp directory, a serialized spec file, or the
    // retired command namespace it used to invoke.
    const source = extensionSource();
    for (const banned of [
      "child_process",
      "spawn(",
      "spawnSync",
      "mkdtempSync",
      "writeFileSync",
      "_create-and-boot",
      "triber",
    ]) {
      expect(source).not.toContain(banned);
    }
  });

  test("it reaches genesis through the one chiefing client", () => {
    const source = extensionSource();
    expect(source).toContain("@chief/chiefing/extension-runtime");
    expect(source).toContain("FounderLaunchClient");
    expect(source).toContain("founderUrlFromEnvironment");
  });

  test("there is exactly one pre-company identity and it is Founder", () => {
    const source = extensionSource();
    for (const retired of ["launcherMode", "LauncherMode", "launcher-mode", "designer"]) {
      expect(source).not.toContain(retired);
    }
  });

  test("the tool still refuses without an active session model route", () => {
    // The one fact only a live Pi session can observe: a company must never be
    // created against a model nobody proved this session is running.
    expect(() =>
      observeFounderBootstrap({ model: undefined, modelRegistry: undefined } as never),
    ).toThrow(/exact model running in this active Pi session/);
  });

  test("the launch path never asks the model registry to refresh", () => {
    // THE defect this closes: `await registry.refresh()` was 140 of a measured
    // launch's 143 seconds. It reaches Pi's `reloadConfig()`, which re-runs the
    // per-provider network fetch with no AbortSignal. Startup discovery is
    // Pi's own bounded operation; this extension must not add a second refresh.
    //
    // Driven, not grepped: the registry below fails the test if it is asked.
    const calls: string[] = [];
    const bootstrap = observeFounderBootstrap({
      model: { provider: "anthropic", id: "claude-opus-5" },
      modelRegistry: {
        refresh: async () => {
          calls.push("refresh");
        },
        getError: () => {
          calls.push("error");
          return undefined;
        },
        getAvailable: () => {
          calls.push("available");
          return [{ provider: "anthropic", id: "claude-opus-5" }];
        },
      },
    } as never);

    expect(calls).not.toContain("refresh");
    expect(calls).toEqual(["error", "available"]);
    expect(bootstrap.provider).toBe("anthropic");
    expect(bootstrap.model).toBe("claude-opus-5");
    expect(bootstrap.observation.models).toEqual([{ id: "claude-opus-5" }]);
  });

  test("the still-starting guard survives the change", () => {
    // `observeSessionProviderModels` doubles as the liveness guard: a Pi
    // session that has not finished starting reports no models, and the launch
    // must refuse rather than create a company against a route nobody proved.
    // This is the part a careless change silently drops.
    expect(() =>
      observeFounderBootstrap({
        model: { provider: "anthropic", id: "claude-opus-5" },
        modelRegistry: {
          refresh: async () => {},
          getError: () => undefined,
          getAvailable: () => [],
        },
      } as never),
    ).toThrow(/no available models for provider 'anthropic'/);
  });
});

describe("a launch that did not complete never claims a company was not created", () => {
  test("it states only that the launch did not complete, and passes chiefd's words through", () => {
    // chiefd answers 422 for outcomes where a company WAS created — "created
    // '<slug>' but its CEO could not be prepared … registered and durable but
    // not running. Recover with: chief attach <slug>". The tool used to
    // overwrite that accurate sentence with "No company was created and no CEO
    // handoff occurred", telling the operator to ignore a company that exists
    // and burying the recovery command.
    const chiefdSaid =
      "ChiefD: created 'leo-capital' but its CEO could not be prepared: boom\n" +
      "'leo-capital' is registered and durable but not running. Recover with: chief attach leo-capital";
    const reported = reportLaunchFailure("Leo Capital", chiefdSaid);

    expect(reported.text).not.toContain("No company was created");
    expect(reported.text).not.toContain("Do not treat this company as launched or running");
    // Verbatim, because the recovery command inside it is the next move.
    expect(reported.text).toContain(chiefdSaid);
    expect(reported.details).toEqual({ ok: false });
  });

  test("an unknown outcome is passed through as unknown", () => {
    // The client's timeout message. It must survive to the operator unaltered:
    // it is the only thing that says the answer is not known either way.
    const timedOut =
      "ChiefD did not answer the launch within 120s. The company may or may not have been created — " +
      "this client cannot tell. Check with `chief ls` before trying again; do not assume the launch failed.";
    expect(reportLaunchFailure("Leo Capital", timedOut).text).toContain(timedOut);
  });
});

describe("founderUrlFromEnvironment", () => {
  test("resolves the endpoint chief published", () => {
    expect(founderUrlFromEnvironment({ [FOUNDER_URL_ENV]: "http://127.0.0.1:54321/" })).toBe(
      "http://127.0.0.1:54321",
    );
  });

  test("refuses rather than guessing a localhost port", () => {
    // A Founder pane that was not started by `chief` has no
    // company-creation authority; inventing an address would either fail
    // confusingly or reach an unrelated listener.
    expect(() => founderUrlFromEnvironment({})).toThrow(/Start the Founder with `chief`/);
    expect(() => founderUrlFromEnvironment({ [FOUNDER_URL_ENV]: "  " })).toThrow(
      /Start the Founder with `chief`/,
    );
  });
});

describe("FounderLaunchClient", () => {
  const bootstrap = {
    provider: "anthropic",
    model: "claude-opus-5",
    observation: { observationId: "obs-1", observedAt: "2026-08-07T00:00:00.000Z", models: [] },
  } as never;

  test("posts one document and returns what chiefd created", async () => {
    const seen: Array<{ url: string; body: unknown }> = [];
    const original = globalThis.fetch;
    globalThis.fetch = (async (url: string, init: RequestInit) => {
      seen.push({ url, body: JSON.parse(String(init.body)) });
      return new Response(
        JSON.stringify({
          slug: "acme-capital",
          url: "http://127.0.0.1:8791",
          ceoPersonId: "executive-ceo",
          session: "org-acme-capital",
        }),
        { status: 200 },
      );
    }) as typeof fetch;
    try {
      const client = new FounderLaunchClient({ url: "http://127.0.0.1:54321" });
      const result = await client.launch({
        name: "  Acme Capital  ",
        purpose: "  Sell anvils  ",
        bootstrap,
      });
      expect(result.slug).toBe("acme-capital");
      expect(result.ceoPersonId).toBe("executive-ceo");
      expect(seen).toHaveLength(1);
      expect(seen[0]!.url).toBe("http://127.0.0.1:54321/v1/founder/launch");
      // The slug is chiefd's decision, not the caller's: the request carries a
      // name and a purpose and nothing structural.
      expect(seen[0]!.body).toEqual({
        name: "Acme Capital",
        purpose: "Sell anvils",
        bootstrap,
      });
    } finally {
      globalThis.fetch = original;
    }
  });

  test("a refusal surfaces chiefd's own text, never a bare status", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = (async () =>
      new Response(
        JSON.stringify({ code: "launch-failed", detail: "Company name needs letters or numbers." }),
        { status: 422 },
      )) as typeof fetch;
    try {
      const client = new FounderLaunchClient({ url: "http://127.0.0.1:54321" });
      await expect(client.launch({ name: "...", purpose: "p", bootstrap })).rejects.toThrow(
        /Company name needs letters or numbers/,
      );
    } finally {
      globalThis.fetch = original;
    }
  });

  test("an unreadable success body is never reported as a launched company", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ ok: true }), { status: 200 })) as typeof fetch;
    try {
      const client = new FounderLaunchClient({ url: "http://127.0.0.1:54321" });
      await expect(client.launch({ name: "Acme", purpose: "p", bootstrap })).rejects.toThrow(
        /body this client cannot read/,
      );
    } finally {
      globalThis.fetch = original;
    }
  });
});
