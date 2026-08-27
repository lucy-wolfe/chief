# Operator loop — every 15 minutes, for the whole run

## Heartbeat mechanism

Run a backgrounded command that re-invokes you when it exits:

```
sleep 900 && echo HEARTBEAT
```

(`run_in_background: true`. When the HEARTBEAT arrives, execute the checklist
below, then immediately start the next backgrounded sleep so the loop never
stops. The loop runs until the final plan's release tag is pushed.)

## Checklist per beat

### 1. Nudge every teammate individually

For EACH teammate (every engineer and the merger), send a direct SendMessage
asking for status, and WAIT for a real acknowledgement with substance back.
This is not a fire-and-forget ping. A real status contains:

- current ticket / branch
- concrete state (what works now)
- next step
- a specific time estimate

If a teammate does not answer, inspect its pane:

```
tmux list-panes -a -F "#{session_name}:#{window_index}.#{pane_index} #{pane_title} #{pane_dead}"
tmux capture-pane -p -t <pane>
```

A dead or wedged pane gets killed and relaunched with the same brief, and the
relaunch is noted in the status update.

### 2. Reconcile the board

Bring every board item's status current with reality:

- tickets an engineer started → In Progress
- branches the merger landed → Done
- newly unblocked tickets → visible and assigned
- anything stalled → note why and what you did about it

The board must never lag what the teammates told you.

### 3. Post ONE synthesized status update to the user

One message, written by you. NEVER a pasted transcript of what teammates said —
synthesize. The update:

- restates that no permission is ever needed and none is being asked for
- reports judgment calls you made since the last beat (Phase 3 choices)
- covers: per-plan progress, per-teammate one-liners, board deltas, blockers
  you are actively clearing, and the next milestone with a time estimate

If the `i-have-adhd` output skill is present, shape the update with it:

1. Next action / current motion first — not context.
2. Numbered steps for anything multi-step.
3. State restated every update (which plan is in flight, N of M tickets Done).
4. Specific time estimates ("~40 min to next landing"), never "soon".
5. Visible wins called out concretely ("X now works; verified by Y").
6. No preamble, no closing pleasantries.
