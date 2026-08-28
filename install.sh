#!/bin/sh
# chief installer — download a prebuilt release, verify it, and install it under
# ~/.chief. No clone, no Rust toolchain, no build.
#
#   curl -fsSL https://chief.zipbox.ai/install.sh | sh
#
# That short URL is a redirect to this file's raw address on GitHub, kept
# outside this repository. The raw address keeps working and is equally
# supported, so nothing here depends on the redirect existing:
#
#   curl -fsSL https://raw.githubusercontent.com/tribes-protocol/chief/main/install.sh | sh
#
# It installs the SAME versioned layout `bun run release` and `chief upgrade`
# produce — bin/ symlinks into versions/<v>/{bin,resources,manifest.json} — so
# `chief upgrade` takes over seamlessly afterwards. macOS and Linux only.
#
# POSIX sh on purpose: it is piped into `sh`, so it uses no bashisms.
set -eu

REPO="tribes-protocol/chief"
CHIEF_HOME="${CHIEF_HOME:-$HOME/.chief}"
PI_INSTALL="npm install -g --ignore-scripts @earendil-works/pi-coding-agent"

say() { printf '%s\n' "$*"; }
die() { printf 'chief install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# --- the host ---------------------------------------------------------------
# The target triple is spelled exactly as the release assets are, from the four
# the release workflow builds. An unlisted pair refuses by name rather than
# downloading an asset that cannot be there.
os="$(uname -s)"
machine="$(uname -m)"
case "$machine" in
  arm64 | aarch64) cpu="aarch64" ;;
  x86_64 | amd64) cpu="x86_64" ;;
  *) die "unsupported CPU architecture '$machine'. chief ships aarch64 and x86_64 only." ;;
esac
case "$os" in
  Darwin) target="${cpu}-apple-darwin" ;;
  Linux) target="${cpu}-unknown-linux-gnu" ;;
  *) die "unsupported operating system '$os'. chief runs on macOS and Linux only." ;;
esac

have curl || die "curl is required to download the release, and it is not on PATH."
have tar || die "tar is required to unpack the release, and it is not on PATH."

# --- the release ------------------------------------------------------------
# The scratch directory is created BEFORE the first request, because the release
# lookup writes into it too. It used to be created after, which is why that
# lookup piped instead of saving — see below.
work="$(mktemp -d "${TMPDIR:-/tmp}/chief-install.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM

api="https://api.github.com/repos/${REPO}/releases/latest"
say "Resolving the latest chief release…"
# SAVED, NOT PIPED, and the difference is the first thing a stranger sees.
#
# This was `curl … | grep -m1 …`. `grep -m1` exits on its first match, and if
# curl is still writing when it does, curl's write fails and it prints
#
#   curl: (23) Failure writing output to destination
#
# on stderr. The tag has already been captured, so the install completes
# normally — but the message appears immediately under "Resolving the latest
# chief release…", on the first command anyone ever runs against this project,
# and it reads as a broken install to somebody with no reason to think
# otherwise. Whether it appears at all depends on whether the response outruns
# the pipe buffer, which is why it is intermittent rather than constant.
#
# Saving the response first also makes the failure honest: a request that
# genuinely fails now reports its own error instead of having it swallowed by
# the pipeline's exit status.
curl -fsSL -H 'Accept: application/vnd.github+json' -o "$work/release.json" "$api" \
  || die "could not reach GitHub to resolve the latest release (rate-limited, offline, or no release yet)."
tag="$(grep -m1 '"tag_name"' "$work/release.json" \
  | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
[ -n "$tag" ] || die "could not read the latest release tag from GitHub (rate-limited, or no release yet)."

base="https://github.com/${REPO}/releases/download/${tag}"
asset="chief-$(printf '%s' "$tag" | sed 's/^v//')-${target}.tar.gz"

say "Downloading ${asset}…"
curl -fsSL -o "$work/$asset" "$base/$asset" \
  || die "could not download $asset. This host's target may not be published for $tag."
curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS" \
  || die "could not download SHA256SUMS for $tag."

# --- verify, and refuse to install a tarball whose digest disagrees ---------
say "Verifying the download against SHA256SUMS…"
expected="$(grep "  ${asset}\$" "$work/SHA256SUMS" | awk '{print $1}')"
[ -n "$expected" ] || die "SHA256SUMS names no $asset; refusing to install it."
if have sha256sum; then
  actual="$(sha256sum "$work/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$work/$asset" | awk '{print $1}')"
fi
[ "$actual" = "$expected" ] \
  || die "$asset does not match SHA256SUMS (expected $expected, got $actual). Nothing was installed."

# --- unpack, name by the manifest version, and swap the symlinks ------------
tar -xzf "$work/$asset" -C "$work" || die "the release archive could not be unpacked."
[ -f "$work/manifest.json" ] || die "the release is missing manifest.json."
version="$(grep -m1 '"version"' "$work/manifest.json" \
  | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
[ -n "$version" ] || die "the release manifest names no version."

# macOS quarantine: curl never sets it, but a browser-downloaded tarball would.
# Cleared defensively before the binaries are placed; a failure here is not one.
if [ "$os" = "Darwin" ]; then
  xattr -dr com.apple.quarantine "$work/bin" >/dev/null 2>&1 || true
fi

versions="$CHIEF_HOME/versions"
mkdir -p "$versions" "$CHIEF_HOME/bin" "$CHIEF_HOME/state"
dest="$versions/$version"
rm -rf "$dest"
# The unpacked top level (bin/ resources/ manifest.json) IS the version dir.
mkdir -p "$dest"
mv "$work/bin" "$work/resources" "$work/manifest.json" "$dest/"

for name in chief chiefd beacond; do
  tmp="$CHIEF_HOME/bin/.$name.tmp.$$"
  rm -f "$tmp"
  ln -s "$dest/bin/$name" "$tmp"
  mv "$tmp" "$CHIEF_HOME/bin/$name"
done

say ""
say "chief $version is installed under $CHIEF_HOME."

# --- PATH ------------------------------------------------------------------
#
# The installer edits the user's shell profile, and says exactly which files it
# touched. A script that changes somebody's dotfiles and does not name them is
# asking to be distrusted, and rightly.
#
# There is deliberately no opt-out flag. Putting the binary on PATH is what
# this script is FOR, it is what every installer of this shape does, and a flag
# would be a setting nobody discovers on the one run where it matters. What
# does the work instead is the report: the output names each file, and says
# when it changed nothing.

export_line="export PATH=\"$CHIEF_HOME/bin:\$PATH\""

# Already exporting this bin directory? Then leave the file alone.
#
# Matched on the PATH rather than on the exact line, so a hand-written variant
# — different quoting, appended rather than prepended, wrapped in a conditional
# — is recognised as already done. Appending a second line that says the same
# thing is the failure mode people notice, because it happens on every re-run.
profile_has_chief() {
  [ -f "$1" ] || return 1
  grep -q "PATH=.*$CHIEF_HOME/bin" "$1" 2>/dev/null
}

add_to_profile() {
  profile="$1"
  if profile_has_chief "$profile"; then
    already="$already $profile"
    return 0
  fi
  # Writability is TESTED, not attempted. A failed `>>` is reported by the
  # shell itself, before any redirection of stderr can suppress it, and under
  # `set -e` it would abort the script — so a strange profile path (a
  # directory, a root-owned file) would end the installer with a raw error
  # AFTER chief was already installed. Nothing at the PATH step is worth
  # failing an install that has otherwise succeeded.
  if [ -e "$profile" ]; then
    { [ -f "$profile" ] && [ -w "$profile" ]; } || { unwritable="$unwritable $profile"; return 0; }
  else
    [ -w "$(dirname "$profile")" ] || { unwritable="$unwritable $profile"; return 0; }
  fi
  {
    printf '\n# Added by the chief installer.\n'
    printf '%s\n' "$export_line"
  } >> "$profile" 2>/dev/null || { unwritable="$unwritable $profile"; return 0; }
  written="$written $profile"
}

written=""
already=""
unwritable=""

for profile in "$HOME/.bashrc" "$HOME/.zshrc"; do
  [ -e "$profile" ] && add_to_profile "$profile"
done

# Neither exists: create the one this user's shell will actually read, rather
# than inventing a config for a shell they do not use.
if [ -z "$written$already$unwritable" ]; then
  case "${SHELL:-}" in
    *zsh) add_to_profile "$HOME/.zshrc" ;;
    *) add_to_profile "$HOME/.bashrc" ;;
  esac
fi

say ""
for profile in $written; do say "Added chief to your PATH in $profile."; done
# Reports what was OBSERVED — the path appears in the file — rather than what
# would be inferred from it. A line that is commented out still mentions the
# directory, and saying "already puts chief on your PATH" about it would be
# false in the one place a stranger reads this project's output first. The
# match is deliberately unchanged: excluding comments would re-add a line the
# user disabled on purpose.
for profile in $already; do say "$profile already mentions chief's bin directory; left it unchanged."; done
for profile in $unwritable; do say "Could not write to $profile."; done

# THE HONEST PART. `curl … | sh` runs in a CHILD shell: nothing this script
# does can change the PATH of the shell the user is sitting in, and sourcing a
# profile in here would change it only for the child that is about to exit.
# Saying otherwise would be a promise the script cannot keep.
case ":$PATH:" in
  *":$CHIEF_HOME/bin:"*) ;;
  *)
    first_written="$(printf '%s' "$written" | awk '{print $1}')"
    say ""
    if [ -n "$first_written" ]; then
      say "New shells will pick that up. For THIS one, run:"
      say "    source $first_written"
    else
      say "Add chief to your PATH — put this in your shell profile:"
      say "    $export_line"
    fi
    ;;
esac

have tmux || {
  say ""
  say "tmux is required and was not found. Install it:"
  say "    macOS:         brew install tmux"
  say "    Debian/Ubuntu: apt-get install -y tmux"
}
# --- Pi -------------------------------------------------------------------
#
# chief installs and upgrades Pi rather than printing a command and hoping.
# The asymmetry is deliberate: an ABSENT Pi is installed without asking, since
# chief cannot run a single person without it and there is nothing to weigh.
# An EXISTING Pi that is merely too old is the user's, and replacing somebody's
# working tool without asking is a different act, so that one prompts.
#
# THE FLOOR IS READ, NEVER WRITTEN HERE. `pi_floor.rs` holds the single
# definition, `release-chiefd.ts` stamps it into the release manifest as
# `piFloor`, and this reads it out of the manifest already unpacked above. A
# version bump therefore needs no edit to this file, and the repository's
# single-definition guard stays satisfied — a number restated here would be a
# second definition wearing a copy's clothes.
pi_floor="$(grep -m1 '"piFloor"' "$dest/manifest.json" 2>/dev/null \
  | sed -E 's/.*"piFloor"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"

# Sorts dotted versions without assuming a numeric field count.
version_below() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | head -n1)" = "$1" ]
}

# A PROMPT IN A PIPED SCRIPT MUST NOT READ ITS OWN SOURCE.
#
# This file is `curl … | sh`, so stdin is the script text: reading stdin would
# consume the rest of the installer. The question goes to the terminal
# directly. Where there is no terminal — CI, a container build — nothing can be
# asked, so the default answer is taken and SAID, because a silent choice made
# on somebody's behalf is the thing that surprises them later.
confirm_default_yes() {
  # OPENABLE, not merely present. `[ -r /dev/tty ]` is TRUE in a container with
  # no controlling terminal — the device node exists and the permission bits
  # allow reading — and the redirect then fails with a raw shell error that no
  # `2>/dev/null` on the command can suppress, because the shell reports it
  # while setting the redirection up. Measured, not reasoned: it printed
  # "cannot create /dev/tty: No such device or address" twice, before the
  # question. Testing the open is the only honest test of whether it will work.
  # A SUBSHELL, and that is load-bearing rather than stylistic. `:` is a POSIX
  # SPECIAL BUILT-IN, and a redirection error on one is fatal to a
  # non-interactive shell — `{ : < /dev/tty; } 2>/dev/null` does not evaluate
  # to false in a container, it ENDS THE INSTALLER, silently, with status 2 and
  # no message. Measured: the run stopped dead at this line. The subshell
  # contains the death so the `if` sees an ordinary false.
  if ( : < /dev/tty ) 2>/dev/null; then
    printf '%s [Y/n] ' "$1" > /dev/tty
    read -r reply < /dev/tty || reply=""
  else
    reply=""
    say "$1 [Y/n] — no terminal to ask on, taking the default (yes)."
  fi
  case "$reply" in
    [Nn]*) return 1 ;;
    *) return 0 ;;
  esac
}

pi_version_now() {
  pi --version 2>/dev/null | tr -d 'v' | awk '{print $NF}'
}

install_pi() {
  have npm || die "npm is required to install Pi, and it is not on PATH. Install Node.js, then run: $PI_INSTALL"
  say "Installing Pi ($PI_INSTALL)…"
  # A failure here is REPORTED, never silent, and never claimed as success.
  if ! $PI_INSTALL; then
    say "Pi could not be installed. chief is installed; run this yourself and it will work:"
    say "    $PI_INSTALL"
    return 1
  fi
  have pi || {
    say "npm reported success but pi is not on PATH yet — open a new shell, or run:"
    say "    $PI_INSTALL"
    return 1
  }
  installed_version="$(pi_version_now)"
  # CHECKED, not assumed. npm exiting zero is not the same as the floor being
  # met — a global install can land somewhere earlier on PATH, or resolve to a
  # version that is still too old — and reporting "ready" without looking is
  # the same shape as every other claim that outran its evidence.
  if [ -n "$pi_floor" ] && [ -n "$installed_version" ] && version_below "$installed_version" "$pi_floor"; then
    say "Pi is $installed_version, still below $pi_floor. Install it yourself with:"
    say "    $PI_INSTALL"
    return 1
  fi
  say "Pi ${installed_version:-installed} is ready."
}

say ""
if ! have pi; then
  # ABSENT: no question. chief cannot run a person without it.
  say "Pi is the agent runtime every person in a company runs, and was not found."
  # NONZERO, for the same reason the declined upgrade below is nonzero, and more
  # strongly. `install_pi` has already told the PERSON what to run; what it
  # cannot do is tell a CALLER. A failed install here leaves the thing that runs
  # people ABSENT, which is strictly worse than the too-old Pi the decline path
  # already refuses to call success — so this cannot be the branch that reports
  # ready. `|| true` said the opposite to every script consuming this installer.
  install_pi || die "chief itself is installed under $CHIEF_HOME; Pi did not install — run: $PI_INSTALL"
else
  pi_version="$(pi_version_now)"
  if [ -n "$pi_floor" ] && [ -n "$pi_version" ] && version_below "$pi_version" "$pi_floor"; then
    say "Pi $pi_version is installed; chief needs $pi_floor or newer."
    if confirm_default_yes "Upgrade Pi to >= $pi_floor?"; then
      # Nonzero for the reason above: an ACCEPTED upgrade that then failed
      # leaves exactly the too-old Pi the branch below refuses to call success.
      # Agreeing to fix it does not make it fixed.
      install_pi || die "chief itself is installed under $CHIEF_HOME; Pi did not upgrade — run: $PI_INSTALL"
    else
      # One of the places this script exits nonzero after chief is installed —
      # countless, because a count in a comment is a fact that goes stale
      # silently. It is not a failed install: it is a declined prerequisite, and
      # saying so with a zero status would tell a script that everything is
      # ready when the thing that runs people is too old. The failed-install
      # paths above exit nonzero for the same reason, applied to a worse state.
      say ""
      die "chief requires Pi $pi_floor or newer, and the upgrade was declined. chief itself is installed under $CHIEF_HOME."
    fi
  fi
fi

say ""
say "Then found your first company:"
say "    mkdir acme && cd acme && chief"
