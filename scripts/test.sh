#!/usr/bin/env bash
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cursor-agent-service-tests.XXXXXX")"
cd "$ROOT_DIR"

run_stage() {
  local name="$1"
  shift
  local log="$LOG_DIR/${name//[^a-zA-Z0-9_-]/_}.log"
  if "$@" >"$log" 2>&1; then
    printf 'PASS %-24s %s\n' "$name" "$log"
  else
    local code=$?
    printf 'FAIL %-24s exit=%s log=%s\n' "$name" "$code" "$log" >&2
    cat "$log" >&2
    exit "$code"
  fi
}

run_stage formatting cargo fmt --all -- --check
run_stage clippy cargo clippy --workspace --all-targets --offline -- -D warnings
run_stage compile cargo check --workspace --offline
run_stage config_check env CURSOR_PROVIDER_API_KEY=offline-test-key cargo run --offline -p cursor-server -- check --config ./config.example.yaml
run_stage local_service ./scripts/smoke-local.sh
run_stage tests cargo test --workspace --all-targets --offline

printf 'All automated checks passed. Logs: %s\n' "$LOG_DIR"
