#!/bin/sh
# Repo-default Claude Code status line.
#
# Checked in so everyone working in this repo gets the same readout without
# configuring anything. Wired up by .claude/settings.json.
#
#   ~/Developer/terminal (main) │ ◆ Opus 5 (1M context) │ 612k/1.0M 61% │ 15m20s
#     │ effort medium │ 91% session │ 29% week │ +2k -146 │ ⟳ 3 bg
#
# Every value is READ from the JSON Claude Code pipes in on stdin — none of it is
# estimated or recomputed. Fields used (v2.1.220):
#   .model.display_name .effort.level
#   .rate_limits.five_hour.used_percentage  (5-hour session window)
#   .rate_limits.seven_day.used_percentage  (7-day window)
#   .context_window.{total_input_tokens,context_window_size,used_percentage}
#   .cost.{total_duration_ms,total_lines_added,total_lines_removed}
#
# The three used_percentage fields above arrive as floats and are truncated to
# integers at the jq extraction below (#922) — every comparison downstream is
# integer-only `[ ]`/`$(( ))`.
#
# PORTABILITY: no `tail -r` (BSD-only), no transcript parsing, and no bash-only
# parameter expansion. An earlier version walked the whole transcript backwards
# to derive context usage — macOS-only AND re-read a growing file on every
# render. Claude Code reports the same number directly, so this reads it.
# Checked with `dash -n`, not just `sh -n`: on macOS /bin/sh is bash in POSIX
# mode and happily accepts bashisms that break on Linux.
#
# Degrades silently: any absent field drops its segment rather than printing an
# error. A field that disappears in a future version should leave a shorter
# line, never a broken one.
input=$(cat)

# jq is the only dependency. Without it, print a bare path so the line still
# says something useful instead of erroring on every render.
if ! command -v jq >/dev/null 2>&1; then
  printf '%s' "$(printf '%s' "$PWD" | sed "s|^$HOME|~|")"
  exit 0
fi

# ONE jq invocation for every field, not one per field. The previous version
# defined j() and called it twelve times; each call forks a jq process, and
# twelve forks measured ~435ms against ~22ms for a single call — essentially the
# whole render cost, with git rev-parse at 6ms and not the problem. Claude Code
# kills a status line command that overruns its timeout, so on a loaded machine
# the line vanished and reappeared as the box got busy and quiet again.
#
# Fields are joined with \037 (US) and split by setting IFS to it. That has to
# be a NON-whitespace delimiter: IFS whitespace collapses runs, which would
# silently shift every field after an absent one. A literal END sentinel is
# appended so no real field is last — a trailing delimiter's empty field is not
# reliably preserved across shells.
#
# jq's `// empty` treated only null as absent for these fields; mapping null to
# "" here is equivalent, since none of them is ever false.
# The three used_percentage fields (#922) can arrive fractional (e.g. "6.2"),
# but every comparison downstream (`[ ]`, `$(( ))`) is integer-only bash/sh
# arithmetic. Truncated DELIBERATELY here, at the point of read -- the one
# place all three pass through -- rather than left to fail silently in `[ ]`/
# `$(( ))` later: that failure writes to stderr (discarded by the harness) and
# the comparison it was feeding simply stops participating, which is worse
# than a truncated value because nothing visible ever indicates it happened.
US=$(printf '\037')
fields=$(printf '%s' "$input" | jq -r --arg us "$US" '
  def trunc_pct: if . == null then null else floor end;
  [
    .cwd,
    .model.display_name,
    .context_window.total_input_tokens,
    .context_window.context_window_size,
    (.context_window.used_percentage | trunc_pct),
    .cost.total_duration_ms,
    .effort.level,
    (.rate_limits.five_hour.used_percentage | trunc_pct),
    (.rate_limits.seven_day.used_percentage | trunc_pct),
    .cost.total_lines_added,
    .cost.total_lines_removed,
    .session_id,
    "END"
  ] | map(if . == null then "" else tostring end) | join($us)
')

saved_ifs=$IFS
set -f
IFS=$US
# shellcheck disable=SC2086
set -- $fields
set +f
IFS=$saved_ifs

f_cwd=$1
f_model=$2
f_ctx_used=$3
f_ctx_window=$4
f_ctx_pct=$5
f_duration_ms=$6
f_effort=$7
f_session_pct=$8
f_week_pct=$9
shift 9
f_lines_added=$1
f_lines_removed=$2
f_session_id=$3

# Dim pipe between segments. Segments are COLLECTED and joined here rather than
# each site appending its own " │ " — that version leaves an orphan separator
# whenever a segment is absent, which is exactly the case this line has to
# survive.
SEP=$(printf ' \033[2m│\033[0m ')
line=""
add() {
  [ -z "$1" ] && return 0
  if [ -z "$line" ]; then line="$1"; else line="${line}${SEP}$1"; fi
}

# --- location ---------------------------------------------------------------
cwd=$f_cwd
[ -z "$cwd" ] && cwd=$(pwd)
disp=$(printf '%s' "$cwd" | sed "s|^$HOME|~|")
branch=$(git --no-optional-locks -C "$cwd" rev-parse --abbrev-ref HEAD 2>/dev/null)
if [ -n "$branch" ]; then
  add "$(printf "%s \033[36m(%s)\033[0m" "$disp" "$branch")"
else
  add "$disp"
fi

# --- model -------------------------------------------------------------------
model_disp=$f_model
[ -n "$model_disp" ] && add "$(printf "\033[35m◆ %s\033[0m" "$model_disp")"

# --- context window ----------------------------------------------------------
fmt() {
  awk -v n="$1" 'BEGIN{ if(n>=1000000) printf "%.1fM",n/1000000; else if(n>=1000) printf "%.0fk",n/1000; else printf "%d",n }'
}

used=$f_ctx_used
window=$f_ctx_window
pct=$f_ctx_pct
if [ -n "$used" ] && [ -n "$window" ] && [ -n "$pct" ] && [ "$window" -gt 0 ] 2>/dev/null; then
  # Bright green normally, red once genuinely full. Distinct from session (cyan)
  # and week (orange) so three numeric segments never look alike.
  if [ "$pct" -lt 80 ]; then col=92; else col=91; fi
  add "$(printf "\033[%sm%s/%s %s%%\033[0m" "$col" "$(fmt "$used")" "$(fmt "$window")" "$pct")"
fi

# --- elapsed -----------------------------------------------------------------
# Grey on purpose: orientation, not something to act on, so it must not compete
# with the numbers that are.
dur_ms=$f_duration_ms
if [ -n "$dur_ms" ] && [ "$dur_ms" -gt 0 ] 2>/dev/null; then
  secs=$((dur_ms / 1000))
  mins=$((secs / 60))
  rem=$((secs % 60))
  if [ "$mins" -gt 0 ]; then
    add "$(printf "\033[90m%sm%ss\033[0m" "$mins" "$rem")"
  else
    add "$(printf "\033[90m%ss\033[0m" "$secs")"
  fi
fi

# --- effort ------------------------------------------------------------------
# Label dim, value bright: the word never changes, the level does, so only the
# level should draw the eye.
effort=$f_effort
[ -n "$effort" ] && add "$(printf "\033[38;5;208meffort\033[0m \033[91m%s\033[0m" "$effort")"

# --- rate limits: how much of the session and the week is LEFT ---------------
# Shown as REMAINING, not used. They are the same fact, but only one of them
# answers the question you have before starting something expensive.
#
# Each gets its OWN colour so the line can be parsed at a glance. The alarm
# survives as an OVERRIDE: at 20% or less remaining a limit turns red whatever
# colour it was assigned, so "nearly out" still shouts.
#
# Colours are SGR parameter STRINGS, not bare codes, so a segment can use a
# 256-colour value (38;5;N). Bright yellow (93) was unreadable on a light
# background — orange (38;5;208) replaced it.
LIM_CRITICAL=20
lim_col() { [ "$1" -le "$LIM_CRITICAL" ] && echo 91 || echo "$2"; }

sess_used=$f_session_pct
if [ -n "$sess_used" ]; then
  sess_left=$((100 - sess_used))
  add "$(printf "\033[%sm%s%% session\033[0m" "$(lim_col "$sess_left" 96)" "$sess_left")"
fi

week_used=$f_week_pct
if [ -n "$week_used" ]; then
  week_left=$((100 - week_used))
  add "$(printf "\033[%sm%s%% week\033[0m" "$(lim_col "$week_left" "38;5;208")" "$week_left")"
fi

# --- lines changed -----------------------------------------------------------
# Only when something actually changed, so a read-only session does not carry a
# permanent "+0 -0".
added=$f_lines_added
removed=$f_lines_removed
if [ -n "$added" ] && [ -n "$removed" ] && { [ "$added" -gt 0 ] 2>/dev/null || [ "$removed" -gt 0 ] 2>/dev/null; }; then
  add "$(printf "\033[92m+%s\033[0m \033[91m-%s\033[0m" "$(fmt "$added")" "$(fmt "$removed")")"
fi

# --- background agents -------------------------------------------------------
# Live background sessions from the local daemon roster, and how many are mid
# loop-iteration. Reads Claude Code's own per-user state under $HOME, so it is
# correct for whoever runs it and prints nothing for someone with no background
# work. Excludes THIS session and anything already done/failed.
self=$(printf '%s' "$f_session_id" | cut -c1-8)
roster="$HOME/.claude/daemon/roster.json"
if [ -f "$roster" ]; then
  counts=$(jq -rn --slurpfile r "$roster" --arg self "$self" '
    ($r[0].workers // {} | keys) as $live
    | [ inputs
        | select( ((.daemonShort // "") as $s
                   | ($live | index($s)) != null and $s != $self)
                  and (.state | IN("done","failed") | not) ) ] as $jobs
    | "\($jobs|length) \($jobs | map(select((.inFlight.kinds // []) | index("session_cron"))) | length)"
  ' "$HOME"/.claude/jobs/*/state.json 2>/dev/null)
  n_bg=${counts%% *}
  n_loops=${counts##* }
  if [ "${n_bg:-0}" -gt 0 ] 2>/dev/null; then
    seg="⟳ ${n_bg} bg"
    if [ "${n_loops:-0}" -gt 0 ] 2>/dev/null; then
      s=""
      [ "$n_loops" -ne 1 ] && s="s"
      seg="${seg}, ${n_loops} loop${s}"
    fi
    add "$(printf "\033[94m%s\033[0m" "$seg")"
  fi
fi

printf '%s' "$line"
