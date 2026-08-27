import { expect, test } from "bun:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { verifyTmuxSingleWriterProbe } from "../scripts/verify-tmux-single-writer-probe";

function event(overrides: Record<string, unknown> = {}) {
  return {
    version: 1,
    writer: "rust",
    process_id: 100,
    phase: "attempt",
    timestamp_ms: 1,
    sequence: 1,
    test_id: "p1-forced-dual-writer",
    correlation_id: "p1-forced-dual-writer",
    socket: "socket",
    organization: "org",
    session: "session",
    target: "session",
    generation: null,
    verb: "set-option",
    topology_affecting: true,
    outcome: null,
    ...overrides,
  };
}

function artifact(events: unknown[]): string {
  const path = join(mkdtempSync(join(tmpdir(), "tmux-p1-verifier-")), "events.jsonl");
  writeFileSync(path, events.map((value) => JSON.stringify(value)).join("\n") + "\n");
  return path;
}

function completeControlEvents() {
  const rustAttempt = event();
  const rustResult = event({ phase: "result", sequence: 2, outcome: { kind: "ok", exit_status: 0 } });
  const tsAttempt = event({ writer: "typescript", process_id: 200, sequence: 1 });
  const tsResult = event({ writer: "typescript", process_id: 200, phase: "result", sequence: 2, outcome: { kind: "ok", exit_status: 0 } });
  const negativeAttempt = event({ writer: "typescript", process_id: 300, test_id: "p1-single-writer", correlation_id: "p1-single-writer", sequence: 1 });
  const negativeResult = event({ writer: "typescript", process_id: 300, test_id: "p1-single-writer", correlation_id: "p1-single-writer", phase: "result", sequence: 2, outcome: { kind: "ok", exit_status: 0 } });
  return [rustAttempt, rustResult, tsAttempt, tsResult, negativeAttempt, negativeResult];
}

test("P1 verifier fails closed on malformed lines and missing required held evidence", () => {
  const malformed = join(mkdtempSync(join(tmpdir(), "tmux-p1-verifier-")), "events.jsonl");
  writeFileSync(malformed, "not-json\n");
  expect(() => verifyTmuxSingleWriterProbe(malformed)).toThrow("malformed JSON");
  const invalidOutcome: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  const rustResult = invalidOutcome.find((value) => value.writer === "rust" && value.phase === "result")!;
  rustResult.outcome = { kind: "invented", exit_status: 0 };
  expect(() => verifyTmuxSingleWriterProbe(artifact(invalidOutcome))).toThrow("invalid result outcome");
  expect(() => verifyTmuxSingleWriterProbe(artifact(completeControlEvents()), ["p1-held-missing"])).toThrow("required held test has no probe evidence");
});

test("P1 verifier rejects malformed numeric and outcome combinations", () => {
  const invalidProcess: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  invalidProcess[0].process_id = 0;
  expect(() => verifyTmuxSingleWriterProbe(artifact(invalidProcess))).toThrow("invalid process_id");
  const invalidSequence: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  invalidSequence[0].sequence = 1.5;
  expect(() => verifyTmuxSingleWriterProbe(artifact(invalidSequence))).toThrow("invalid sequence");
  const invalidTimestamp: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  invalidTimestamp[0].timestamp_ms = -1;
  expect(() => verifyTmuxSingleWriterProbe(artifact(invalidTimestamp))).toThrow("invalid timestamp_ms");
  const invalidGeneration: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  invalidGeneration[0].generation = 1.5;
  expect(() => verifyTmuxSingleWriterProbe(artifact(invalidGeneration))).toThrow("invalid generation");
  const nullNonzero: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  const tsResult = nullNonzero.find((value) => value.writer === "typescript" && value.test_id === "p1-forced-dual-writer" && value.phase === "result")!;
  tsResult.outcome = { kind: "nonzero", exit_status: null };
  expect(() => verifyTmuxSingleWriterProbe(artifact(nullNonzero))).toThrow("nonzero without a non-zero integer exit status");
});

test("P1 verifier rejects unpaired events and same-target coincidence without a successful TS result", () => {
  const unpaired = completeControlEvents().filter((value) => !(
    (value as { writer: string }).writer === "typescript"
    && (value as { phase: string }).phase === "result"
  ));
  expect(() => verifyTmuxSingleWriterProbe(artifact(unpaired))).toThrow("unpaired attempt");
  const falseCollision: Array<Record<string, unknown>> = completeControlEvents().map((value) => ({ ...value }));
  const tsResult = falseCollision.find((value) => value.writer === "typescript" && value.test_id === "p1-forced-dual-writer" && value.phase === "result")!;
  tsResult.outcome = { kind: "nonzero", exit_status: 1 };
  expect(() => verifyTmuxSingleWriterProbe(artifact(falseCollision))).toThrow("successful cross-writer topology overlap");
});

test("P1 verifier rejects a held Rust/TypeScript overlap even when TS lacks optional provenance", () => {
  const heldRustAttempt = event({
    test_id: "p1-held-startup-pane", correlation_id: "p1-held-startup-pane", target: "@1", timestamp_ms: 10,
  });
  const heldRustResult = event({
    test_id: "p1-held-startup-pane", correlation_id: "p1-held-startup-pane", target: "@1", timestamp_ms: 30, sequence: 2,
    phase: "result", outcome: { kind: "ok", exit_status: 0 },
  });
  const heldTsAttempt = event({
    writer: "typescript", process_id: 200, test_id: "p1-held-startup-pane", correlation_id: "p1-held-startup-pane",
    organization: null, session: null, target: "@1", timestamp_ms: 20,
  });
  const heldTsResult = event({
    writer: "typescript", process_id: 200, test_id: "p1-held-startup-pane", correlation_id: "p1-held-startup-pane",
    organization: null, session: null, target: "@1", timestamp_ms: 25, sequence: 2,
    phase: "result", outcome: { kind: "ok", exit_status: 0 },
  });
  expect(() => verifyTmuxSingleWriterProbe(artifact([
    ...completeControlEvents(), heldRustAttempt, heldRustResult, heldTsAttempt, heldTsResult,
  ]))).toThrow("held path contains in-flight cross-writer overlap on one physical tmux target");
});

test("P1 verifier ignores explicitly cosmetic overlap but not topology overlap", () => {
  const heldRustAttempt = event({
    test_id: "p1-held-cosmetic", correlation_id: "p1-held-cosmetic", target: "@1", timestamp_ms: 10,
  });
  const heldRustResult = event({
    test_id: "p1-held-cosmetic", correlation_id: "p1-held-cosmetic", target: "@1", timestamp_ms: 30,
    sequence: 2, phase: "result", outcome: { kind: "ok", exit_status: 0 },
  });
  const cosmeticAttempt = event({
    writer: "typescript", process_id: 200, test_id: "p1-held-cosmetic", correlation_id: "p1-held-cosmetic",
    target: "@1", timestamp_ms: 20, topology_affecting: false,
  });
  const cosmeticResult = event({
    writer: "typescript", process_id: 200, test_id: "p1-held-cosmetic", correlation_id: "p1-held-cosmetic",
    target: "@1", timestamp_ms: 25, sequence: 2, phase: "result",
    topology_affecting: false, outcome: { kind: "ok", exit_status: 0 },
  });
  expect(() => verifyTmuxSingleWriterProbe(artifact([
    ...completeControlEvents(), heldRustAttempt, heldRustResult, cosmeticAttempt, cosmeticResult,
  ]))).not.toThrow();
});
