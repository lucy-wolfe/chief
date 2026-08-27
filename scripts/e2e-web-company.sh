#!/usr/bin/env bash
# Boot one company API-hosted and prove the web can talk to its agents.
#
# WHY THIS IS A SCRIPT AND NOT A SEQUENCE OF COMMANDS
#
# This sequence was first attempted as one-shot remote commands and failed
# repeatedly for reasons that had nothing to do with the product: nested
# heredocs that silently wrote nothing, `tmux send-keys` into panes with no
# shell, and — twice — reading a log left over from the previous attempt and
# believing it was current. Every one of those looked exactly like a product
# failure.
#
# So the whole sequence runs in ONE process, every step asserts, and every
# artifact it reads is one it just created. A failure names its step.
#
# WHAT IT REFUSES TO DO
#
# It does not install anything and it does not hand-assemble `chiefd run`.
# Assembling that by hand once produced a daemon keyed to the wrong directory
# that answered `unknown-company` to every route — `attach` owns that assembly
# and gets it right.
#
#   usage: e2e-web-company.sh <company directory> [--repo <dir>] [--install-home <dir>]

set -euo pipefail

# THE COMPANY. It is a directory, so this takes one — never a slug, which names
# no company (two directories may hold companies called the same thing).
COMPANY_DIR="${1:?usage: e2e-web-company.sh <company directory> [--repo <dir>] [--install-home <dir>]}"
shift || true
REPO="${REPO:-/root/chief}"
# The box's own chief directory: the `bin/` symlinks and the versioned installs
# live here (`chief-cli::paths::install_home`, `release-chiefd.ts`). `.chief`
# and not `.chiefd` — the latter was the global company tree and is gone.
#
# It is NOT a data root and never was one: `--data-root` meant the ORGS root in
# one place and this directory in another, and that ambiguity cost a day the
# last time this file used the word.
INSTALL_HOME="${INSTALL_HOME:-/root/.chief}"
WEB="${WEB:-http://127.0.0.1:3000}"
BEACOND="${BEACOND:-http://127.0.0.1:6969}"

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="$2"; shift 2 ;;
    --install-home) INSTALL_HOME="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Canonicalized once, here: the company key is `sha256(<canonical dir>)` and
# `chiefd run` canonicalizes its own `--dir` the same way, so a relative or
# symlinked spelling would key one company two ways.
[ -d "$COMPANY_DIR" ] || { echo "no such company directory: $COMPANY_DIR" >&2; exit 2; }
COMPANY_DIR="$(cd "$COMPANY_DIR" && pwd -P)"

STEP=0
step() { STEP=$((STEP + 1)); printf '\n[%d] %s\n' "$STEP" "$1"; }
fail() { printf '   FAILED at step %d: %s\n' "$STEP" "$1" >&2; exit 1; }
ok() { printf '   ok — %s\n' "$1"; }

CHIEF="$INSTALL_HOME/bin/chief"
BUILT="$REPO/apps/chiefd/target/debug/chief"

step "the installed chief is THIS build"
# Three separate stale-install failures happened while writing this: beacond
# and chiefd each built before the discovery port moved, and a .next holding
# another build's chunks. Every one started perfectly and was wrong, so the
# first thing this checks is that the binary about to run is the current one.
[ -x "$CHIEF" ] || fail "no chief installed at $CHIEF"
if [ -x "$BUILT" ] && ! cmp -s "$CHIEF" "$BUILT"; then
  fail "installed chief differs from $BUILT — copy the build over it first"
fi
ok "$CHIEF matches the build"

step "beacond answers"
curl -sf --max-time 5 "$BEACOND/v1/health" >/dev/null || fail "beacond unreachable at $BEACOND"
ok "$BEACOND healthy"

step "the installed chief has its resources beside it"
# `prepare-ceo-only` refuses `launcher-root-unusable` without this, and a CEO
# that comes up with no org_* tools cannot staff its own company.
#
# Derived from the BINARY, the same way the binary derives it — the pointer
# file this used to read is deleted, and with it the possibility of a harness
# and a product resolving two different trees.
CHIEF_REAL="$(readlink -f "$INSTALL_HOME/bin/chief" 2>/dev/null || true)"
[ -n "$CHIEF_REAL" ] || fail "no installed chief at $INSTALL_HOME/bin/chief (run 'bun run release')"
ROOT="$(dirname "$(dirname "$CHIEF_REAL")")/resources"
[ -d "$ROOT/packages/piing/extensions" ] || fail "no packages/piing/extensions under '$ROOT'"
ok "$ROOT"

step "a Pi runtime is resolvable"
# Encoded as a precondition rather than assumed from an ambient environment.
# Pi's CLI begins `#!/usr/bin/env node`, so a host with bun and no node has Pi
# fully installed and unable to run — which chiefd used to report as "no
# runtime found", sending an operator to reinstall what was already on disk.
if [ -z "${TEAM_LAUNCHER_PI:-}" ] && [ -x "$REPO/node_modules/.bin/pi" ]; then
  export TEAM_LAUNCHER_PI="$REPO/node_modules/.bin/pi"
fi
[ -n "${TEAM_LAUNCHER_PI:-}" ] || fail "no Pi: set TEAM_LAUNCHER_PI, or install the repo's dependencies so $REPO/node_modules/.bin/pi exists"
"$TEAM_LAUNCHER_PI" --version >/dev/null 2>&1 \
  || fail "$TEAM_LAUNCHER_PI does not answer --version — Pi is present and cannot execute; check its interpreter (its CLI starts with '#!/usr/bin/env node')"
ok "$TEAM_LAUNCHER_PI ($("$TEAM_LAUNCHER_PI" --version 2>/dev/null))"

step "no daemon is already running for $COMPANY_DIR"
# The config is read ONCE at daemon start and a live daemon never re-reads it,
# so a daemon older than the pointer file above resolves the old launcher root
# and an actuation-mode change is invisible to it. This is why the sequence
# stops here rather than attaching on top of whatever is alive.
# Checked TWO ways, because `pgrep` on a command line is the weaker one: a
# daemon started by an older binary can carry different argv and slip past a
# pattern match while still holding the port. beacond's registration is the
# product's OWN answer to "is this company running", so it is authoritative
# here — and a daemon that survives this step silently ignores every setting
# below, which is exactly how step 7 came to look unexplainable.
# `([[:space:]]|$)` anchors the argv word. `pgrep -f` matches an ERE, and
# `--dir …/acme` is a PREFIX of `--dir …/acme-corp`, so an unanchored probe
# refuses this run because a DIFFERENT company is up.
if pgrep -f "chiefd run --dir $COMPANY_DIR"'([[:space:]]|$)' >/dev/null 2>&1; then
  fail "a daemon for $COMPANY_DIR is already running (by process) — stop it first, or it will ignore every setting below"
fi
REGISTERED="$(curl -sf --max-time 5 "$BEACOND/v1/list" \
  | python3 -c "import sys,json;print(next((c.get('url','') for c in json.load(sys.stdin)['companies'] if c['dir']=='$COMPANY_DIR'),''))" 2>/dev/null || true)"
if [ -n "$REGISTERED" ]; then
  fail "beacond still has $COMPANY_DIR registered at $REGISTERED — a daemon is alive that pgrep did not match; stop it, or it will serve every request below with its own older config"
fi
ok "none alive, and beacond has no registration for it"

step "the company is set to API-hosted (shadow) mode"
# ONE argument, and it is the company directory. This used to be
# `--company <slug> --data-root <dir>`, whose second half meant the ORGS root
# one level ABOVE the company; passing this script's chiefd home instead landed
# the write in an orphan database one directory up, printed success, and the
# daemon below never saw it. Step 10 then failed with a refusal that looked
# like a defect in the actuation gate, which is where a day went. `--dir` has
# no second meaning.
"$CHIEF" set-actuation-config --dir "$COMPANY_DIR" --mode shadow >/dev/null \
  || fail "could not set actuation mode"
ok "mode=shadow (adopted at the next daemon start, which is why it is set BEFORE attach)"

step "attach starts the company"
[ -n "${TMUX:-}" ] || fail "run this inside tmux — chiefd refuses outside one, and so does this"
# `attach` takes no company: it opens THIS DIRECTORY's, so the directory has to
# be where the process is standing.
( cd "$COMPANY_DIR" && "$CHIEF" attach ) || fail "attach refused; its message names the reason"
ok "attach returned"

step "the company registered a url with beacond"
# The KEY comes back on the same row as the url, and is what every web route
# below is keyed by. It is read here rather than derived: beacond records the
# key its caller minted, and a second producer of an identity is what the
# directory hash exists to delete.
URL=""; KEY=""
for _ in $(seq 1 20); do
  ROW="$(curl -sf --max-time 5 "$BEACOND/v1/list" \
    | python3 -c "import sys,json;c=next((c for c in json.load(sys.stdin)['companies'] if c['dir']=='$COMPANY_DIR'),{});print(c.get('url',''));print(c.get('key',''))" 2>/dev/null || true)"
  URL="$(printf '%s\n' "$ROW" | sed -n 1p)"
  KEY="$(printf '%s\n' "$ROW" | sed -n 2p)"
  [ -n "$URL" ] && break
  sleep 1
done
[ -n "$URL" ] || fail "no url registered for $COMPANY_DIR — a daemon that never registers is usually an older binary than the discovery port"
[ -n "$KEY" ] || fail "beacond registered $COMPANY_DIR with no company key — the web routes are keyed by it and cannot be addressed"
ok "$URL (key $KEY)"

step "the web serves this company's tree"
curl -sf --max-time 10 "$WEB/api/companies/$KEY/tree" >/dev/null \
  || fail "the web could not read the tree — is the Next server up at $WEB, and is its .next from THIS build?"
ok "tree served"

step "the web hosts this company's people"
PEOPLE="$(curl -s --max-time 30 "$WEB/api/companies/$KEY/people")"
case "$PEOPLE" in
  *company-not-api-hosted*)
    fail "still tmux-actuating: the daemon predates the shadow setting — stop it and re-run" ;;
  *hosted*) ok "$PEOPLE" ;;
  *) fail "unexpected people answer: $PEOPLE" ;;
esac

step "an agent answers a turn"
# The one seam no unit test can settle. Everything between the browser and the
# harness is covered; this proves the harness actually replies.
PERSON="$(printf '%s' "$PEOPLE" | python3 -c "import sys,json;h=json.load(sys.stdin)['hosted'];print(h[0] if h else '')")"
[ -n "$PERSON" ] || fail "nobody is hosted, so there is no agent to ask"
REPLY="$(curl -s --max-time 180 -X POST "$WEB/api/companies/$KEY/people/$PERSON/say" \
  -H 'content-type: application/json' -d '{"text":"Reply with the single word: ready."}')"
case "$REPLY" in
  *'"reply"'*) ok "$PERSON answered: $REPLY" ;;
  *) fail "no reply from $PERSON: $REPLY" ;;
esac

printf '\nPASS — the web talked to a live agent in %s.\n' "$COMPANY_DIR"
