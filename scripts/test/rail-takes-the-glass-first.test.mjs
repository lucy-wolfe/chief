// A rail pane is a white rectangle until a program takes its terminal.
//
// # Why this file exists
//
// The operator photographed a department click and said "it flashes white".
// The screenshot carried its own proof: the rail pane was empty apart from a
// literal `^[^R` in the top-left corner. Those two characters are the rail's
// own `SELECTION_WAKE_KEY` (`C-M-r`) arriving at a pty that is still in
// CANONICAL mode with echo on — a state no running rail can be in, because raw
// mode does not echo and a program reading its terminal does not leave the key
// for the line discipline to print.
//
// It is the state a rail pane is in for the whole of `chief sidebar`'s boot.
// The pane is minted blank by `split-window` (`interpret::ensure_rail_in_window`)
// and nothing paints it until the first frame, so every millisecond the verb
// spends on discovery, on a beacond health wait that may SPAWN beacond, on a
// company round trip, on a key read and on two tmux calls is a millisecond of
// white. Measured from the rail's own log on the operator's box: a median of
// 11ms, and **804ms** for `%8`, the exact pane in the screenshot.
//
// The fix is an ORDERING — `Glass::take` before any of that work — and an
// ordering is exactly the kind of rule that regresses silently. Nothing goes
// red when a later edit moves one `await` above it. The pane simply goes white
// again for as long as that call takes, on a box nobody is watching, and the
// bug comes back wearing the same clothes.
//
// # The rule
//
// **The rail takes its terminal before it does anything that can block.**
//
// Not "early", not "near the top" — before. A call that can block is a call
// that can be slow, and every one of them that runs first is white on the
// operator's glass and a wake key echoed into their sidebar.
//
// # What it checks
//
// Two derived assertions over `run_sidebar`'s own body. Neither hand-lists the
// boot calls, because a hand-listed set is a set that goes stale the day
// somebody adds a step to the boot — which is the day this guard most needs to
// be right.
//
//   1. `Glass::take` precedes every `.await` in the body. An `await` is the
//      shape of every network and daemon wait in this function.
//   2. No `?` is applied before `Glass::take`. `?` is the shape of every
//      fallible call, and a fallible call that runs first is both a delay and
//      an early return out of a function that has not yet taken the terminal
//      it would have to give back.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** The verb that runs inside a rail pane. */
export const SIDEBAR_VERB_FILE = join(
  "apps",
  "chiefd",
  "crates",
  "chief-cli",
  "src",
  "main.rs"
);

/** The call that takes raw mode, the alternate screen and the first frame. */
export const TAKE = "Glass::take(";

/**
 * `run_sidebar`'s body, from its opening brace to the matching close.
 *
 * Braces are counted rather than matched by regex: the body contains closures
 * and struct literals, so "the next `\n}`" is not the end of it.
 */
function sidebarBody(source) {
  const signature = source.indexOf("async fn run_sidebar(");
  assert.notEqual(
    signature,
    -1,
    "run_sidebar is the verb a rail pane runs; if it has been renamed, this guard has to " +
      "follow it rather than silently pass"
  );
  const open = source.indexOf("{", signature);
  let depth = 0;
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === "{") depth += 1;
    else if (source[index] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, index);
    }
  }
  throw new Error("run_sidebar's body has no matching close brace");
}

/** The body with its comments removed, so prose about `?` is not read as code. */
function code(body) {
  return body
    .replace(/\/\/.*$/gm, "")
    .replace(/\/\*[\s\S]*?\*\//g, "");
}

test("the rail takes its terminal before it awaits anything", () => {
  const body = code(sidebarBody(readFileSync(join(repoRoot, SIDEBAR_VERB_FILE), "utf8")));

  const take = body.indexOf(TAKE);
  assert.notEqual(
    take,
    -1,
    `${TAKE} is how the rail takes raw mode, the alternate screen and its first painted ` +
      "frame; without it the pane is blank and echoing for the whole boot"
  );

  const firstAwait = body.indexOf(".await");
  assert.notEqual(firstAwait, -1, "run_sidebar is async and does await; this guard reads that");
  assert.ok(
    take < firstAwait,
    "every await in the boot is a wait the operator spends looking at a white rectangle, and " +
      "one in which a sibling rail's C-M-r is echoed into their sidebar as a literal ^[^R. " +
      "The terminal is taken FIRST."
  );
});

test("the rail takes its terminal before anything that can fail", () => {
  const body = code(sidebarBody(readFileSync(join(repoRoot, SIDEBAR_VERB_FILE), "utf8")));

  const take = body.indexOf(TAKE);
  const before = body.slice(0, take);

  assert.ok(
    !before.includes("?"),
    "a fallible call before the glass is taken is two defects at once: the delay is white on " +
      "the operator's glass, and its early return leaves the verb with nothing to give back. " +
      `What runs first instead: ${JSON.stringify(before.trim().split("\n").at(-1) ?? "")}`
  );
});
