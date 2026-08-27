#!/usr/bin/env bash
# Run one disk-bounded part of the workspace test gate. The old single
# `cargo test --workspace` command compiled every test target into one runner
# target directory. That directory exhausted the GitHub runner before tests
# started. Each CI shard now owns a package group and its own target directory.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="$(mktemp)"
TARGET_LOG_DIR="$(mktemp -d)"
trap 'rm -f "$LOG"; rm -rf "$TARGET_LOG_DIR"' EXIT

if [[ -z "${CI_CARGO_PACKAGES:-}" || -z "${CI_CARGO_MEMBERS:-}" ]]; then
  echo "CI_CARGO_PACKAGES and CI_CARGO_MEMBERS are required" >&2
  exit 2
fi

read -r -a packages <<< "$CI_CARGO_PACKAGES"
read -r -a members <<< "$CI_CARGO_MEMBERS"
if (( ${#packages[@]} == 0 || ${#packages[@]} != ${#members[@]} )); then
  echo "package/member groups must have equal, non-zero lengths" >&2
  exit 2
fi

run_only=0
if (( $# > 0 )); then
  if [[ "$1" != "--no-run" || $# -ne 1 ]]; then
    echo "the only supported shard option is --no-run" >&2
    exit 2
  fi
  run_only=1
fi

: > "$LOG"
status=0
for package in "${packages[@]}"; do
  echo "[cargo-test-shard] package=$package"
  cargo_args=(--locked --no-fail-fast)
  if (( run_only )); then
    cargo_args+=(--no-run)
  fi
  parallel_targets=()
  if [[ "$package" == "chief-cli" && -n "${CI_CARGO_PARALLEL_TARGETS:-}" && "$run_only" -eq 0 ]]; then
    read -r -a parallel_targets <<< "$CI_CARGO_PARALLEL_TARGETS"
  fi
  if (( ${#parallel_targets[@]} == 0 )); then
    cargo test --package "$package" "${cargo_args[@]}" \
      --manifest-path "$ROOT/apps/chiefd/Cargo.toml" 2>&1 | tee -a "$LOG"
    package_status="${PIPESTATUS[0]}"
  else
    pids=()
    target_logs=()
    for target in "${parallel_targets[@]}"; do
      target_log="$TARGET_LOG_DIR/$target.log"
      target_args=("${cargo_args[@]}" --manifest-path "$ROOT/apps/chiefd/Cargo.toml")
      case "$target" in
        lib) target_args+=(--lib) ;;
        doc) target_args+=(--doc) ;;
        bin:*) target_args+=(--bin "${target#bin:}") ;;
        *) target_args+=(--test "$target") ;;
      esac
      echo "[cargo-test-shard] package=$package target=$target"
      cargo test --package "$package" "${target_args[@]}" >"$target_log" 2>&1 &
      pids+=("$!")
      target_logs+=("$target_log")
    done
    package_status=0
    for index in "${!pids[@]}"; do
      if ! wait "${pids[$index]}"; then
        package_status=1
      fi
      tee -a "$LOG" <"${target_logs[$index]}"
    done
  fi
  if [[ "$package_status" -ne 0 ]]; then
    status=1
  fi
done

if (( run_only )); then
  exit "$status"
fi

if ! node "$ROOT/scripts/cargo-test-workspace-shard-floor.mjs" "$LOG" "${members[@]}"; then
  status=1
fi

exit "$status"
