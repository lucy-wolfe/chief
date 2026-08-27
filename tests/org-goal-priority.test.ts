import { describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { join } from "node:path";
import {
  DEFAULT_GOAL_PRIORITY,
  FOCUS_DEFERRAL_CAP,
  GOAL_PRIORITIES,
  GOAL_PRIORITY_INVALID_MESSAGE,
  GOAL_PRIORITY_RANK,
  compareFocusOrder,
  effectiveGoalPriority,
  effectiveGoalRank,
  goalPriorityLabel,
  isGoalPriority,
  normalizeGoalPriority,
  orderByFocus,
  type FocusOrderItem,
} from "@chief/piing/extension-runtime";
import { piingExtensionsRoot } from "@chief/piing";

const AT = (offsetMs: number) => new Date(Date.parse("2026-07-15T12:00:00.000Z") + offsetMs).toISOString();

function item(overrides: Partial<FocusOrderItem> & { id: string }): FocusOrderItem {
  return { priority: "normal", focusDeferralCount: 0, createdAt: AT(0), ...overrides };
}

describe("goal priority vocabulary", () => {
  test("normalizes an omitted priority to the neutral default and preserves the four exact values", () => {
    expect(normalizeGoalPriority(undefined)).toBe("normal");
    expect(normalizeGoalPriority(null)).toBe("normal");
    expect(DEFAULT_GOAL_PRIORITY).toBe("normal");
    for (const priority of GOAL_PRIORITIES) expect(normalizeGoalPriority(priority)).toBe(priority);
  });

  test("rejects aliases, whitespace, casing, numbers, and empty strings with the exact calm copy", () => {
    for (const invalid of ["critical", "p0", " high", "High", "URGENT", "", "0", 3, {}]) {
      expect(() => normalizeGoalPriority(invalid as unknown)).toThrow(GOAL_PRIORITY_INVALID_MESSAGE);
    }
    expect(isGoalPriority("urgent")).toBeTrue();
    expect(isGoalPriority("critical")).toBeFalse();
  });

  test("human labels are stable and rank is urgent > high > normal > low", () => {
    expect(goalPriorityLabel("urgent")).toBe("🚨 Urgent");
    expect(goalPriorityLabel("high")).toBe("⬆ High");
    expect(goalPriorityLabel("normal")).toBe("🎯 Normal");
    expect(goalPriorityLabel("low")).toBe("⬇ Low");
    expect(GOAL_PRIORITY_RANK.urgent).toBeGreaterThan(GOAL_PRIORITY_RANK.high);
    expect(GOAL_PRIORITY_RANK.high).toBeGreaterThan(GOAL_PRIORITY_RANK.normal);
    expect(GOAL_PRIORITY_RANK.normal).toBeGreaterThan(GOAL_PRIORITY_RANK.low);
  });
});

describe("bounded focus aging", () => {
  test("promotes one rank per four deferrals and caps at urgent", () => {
    expect(effectiveGoalRank("low", 0)).toBe(GOAL_PRIORITY_RANK.low);
    expect(effectiveGoalRank("low", 3)).toBe(GOAL_PRIORITY_RANK.low);
    expect(effectiveGoalRank("low", 4)).toBe(GOAL_PRIORITY_RANK.normal);
    expect(effectiveGoalRank("low", 8)).toBe(GOAL_PRIORITY_RANK.high);
    expect(effectiveGoalRank("low", 12)).toBe(GOAL_PRIORITY_RANK.urgent);
    // The cap prevents a long outage from manufacturing unbounded promotions.
    expect(effectiveGoalRank("low", 999)).toBe(GOAL_PRIORITY_RANK.urgent);
    expect(effectiveGoalRank("high", FOCUS_DEFERRAL_CAP)).toBe(GOAL_PRIORITY_RANK.urgent);
    expect(effectiveGoalPriority("low", 12)).toBe("urgent");
    expect(effectiveGoalPriority("normal", 0)).toBe("normal");
  });
});

describe("canonical comparator", () => {
  test("a later urgent goal outranks an earlier low/normal/high goal", () => {
    const low = item({ id: "a", priority: "low", createdAt: AT(0) });
    const normal = item({ id: "b", priority: "normal", createdAt: AT(1) });
    const high = item({ id: "c", priority: "high", createdAt: AT(2) });
    const urgent = item({ id: "d", priority: "urgent", createdAt: AT(3) });
    const ordered = orderByFocus([low, normal, high, urgent]).map((entry) => entry.id);
    expect(ordered).toEqual(["d", "c", "b", "a"]);
  });

  test("equal priority orders by never-focused, then last focus, then createdAt, then id", () => {
    const neverA = item({ id: "z", createdAt: AT(10) });
    const neverB = item({ id: "a", createdAt: AT(10) });
    const focusedEarly = item({ id: "x", createdAt: AT(0), lastFocusedAt: AT(5) });
    const focusedLate = item({ id: "y", createdAt: AT(0), lastFocusedAt: AT(9) });
    const ordered = orderByFocus([focusedLate, neverA, focusedEarly, neverB]).map((entry) => entry.id);
    // never-focused (tie-broken by id a<z) precede focused (earlier focus first)
    expect(ordered).toEqual(["a", "z", "x", "y"]);
  });

  test("aging lets a continuously deferred low goal overtake a fresh normal goal", () => {
    const agedLow = item({ id: "low", priority: "low", focusDeferralCount: 8, createdAt: AT(0) });
    const freshNormal = item({ id: "normal", priority: "normal", focusDeferralCount: 0, createdAt: AT(100) });
    expect(compareFocusOrder(agedLow, freshNormal)).toBeLessThan(0);
  });

  test("is a total deterministic order stable across shuffles (restart determinism)", () => {
    const items = [
      item({ id: "g1", priority: "normal", createdAt: AT(5) }),
      item({ id: "g2", priority: "normal", createdAt: AT(5) }),
      item({ id: "g3", priority: "high", createdAt: AT(9) }),
      item({ id: "g4", priority: "low", focusDeferralCount: 12, createdAt: AT(1) }),
    ];
    const first = orderByFocus(items).map((entry) => entry.id);
    const reshuffled = orderByFocus([...items].reverse()).map((entry) => entry.id);
    expect(first).toEqual(reshuffled);
    // g4 is low but aged 12 deferrals -> effective urgent, so it outranks high g3.
    expect(first).toEqual(["g4", "g3", "g1", "g2"]);
  });
});

describe("single goal-priority runtime authority", () => {
  test("uses the public Piing runtime and has no retired extension copy", () => {
    expect(existsSync(join(piingExtensionsRoot(), "organization-goal-priority.ts"))).toBeFalse();
  });
});
