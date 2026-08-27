// Manual (non-CI) capture-pane proof script for #366: renders every footer
// stat field through Pi's REAL default theme resolver (loadThemeFromPath ->
// Theme.fg, exactly the call team-ui.ts's footer now makes) in both the
// shipped "dark" and "light" themes, using the real footer copy so a tmux
// pane can be captured against each background and the result eyeballed for
// legibility. Not part of the automated suite -- this is the one-off
// "attach the captures" artifact the #366/#353 verification bar requires.
import { dirname, join } from "node:path";

const PI_THEME_DIR = join(
  dirname(Bun.resolveSync("@earendil-works/pi-coding-agent", import.meta.dir)),
  "modes/interactive/theme",
);

async function main() {
  const pi = await import(PI_THEME_DIR + "/theme.js");
  const darkTheme = pi.loadThemeFromPath(join(PI_THEME_DIR, "dark.json"), "truecolor");
  const lightTheme = pi.loadThemeFromPath(join(PI_THEME_DIR, "light.json"), "truecolor");

  // #366: the exact mapping team-ui.ts's footer now uses -- kept in sync by
  // hand since the extension can't import this test's theme module (or vice
  // versa); see the comment above the old footerPalette in team-ui.ts.
  const FOOTER_TOKEN_SAMPLES: Array<[token: string, text: string]> = [
    ["accent", "launcher"], // team identity
    ["customMessageLabel", "@operator"], // role
    ["success", "main"], // git branch
    ["border", "…/workspace/team-launcher-2.0"], // cwd
    ["warning", "example-model"], // model name / cache-hit-rate
    ["error", "high"], // reasoning level / goal count / settle countdown / memory-failed
    ["syntaxType", "50.5%/128K"], // context field / skill-created
  ];

  function sample(label: string, theme: any) {
    console.log(`\n== ${label} ==`);
    for (const [token, text] of FOOTER_TOKEN_SAMPLES) {
      console.log(`  ${token.padEnd(20)} ${theme.fg(token, text)}`);
    }
    // A full composed footer line, exactly the shape render() joins.
    const line1 = `${theme.fg("border", "…/workspace/team-launcher-2.0")} ${theme.fg("dim", "•")} ${theme.fg("success", "main")}      ${theme.fg("accent", "launcher")} ${theme.fg("dim", "•")} ${theme.fg("customMessageLabel", "@operator")}`;
    const line2 = `${theme.fg("warning", "CH 27.4%")} ${theme.fg("dim", "•")} ${theme.fg("syntaxType", "50.5%/128K")} ${theme.fg("dim", "•")} ${theme.fg("error", "1 goal")}      ${theme.fg("dim", "example-provider")} ${theme.fg("warning", "example-model")} ${theme.fg("error", "high")}`;
    console.log(`\n  ${line1}`);
    console.log(`  ${line2}`);
  }

  sample("DARK", darkTheme);
  sample("LIGHT", lightTheme);
}

main();
