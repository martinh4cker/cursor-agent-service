#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
server_bin="$project_root/target/debug/cursor-server"
temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/cursor-agent-service-local.XXXXXX")"
service_pid=""

cleanup() {
  if [[ -n "$service_pid" ]] && kill -0 "$service_pid" 2>/dev/null; then
    kill -TERM "$service_pid" 2>/dev/null || true
    wait "$service_pid" 2>/dev/null || true
  fi
  rm -rf "$temporary_root"
}
trap cleanup EXIT

if [[ ! -x "$server_bin" ]]; then
  printf 'FAIL local service: binary not found at %s\n' "$server_bin" >&2
  exit 1
fi

read -r local_service_port local_proxy_port < <(python3 - <<'PY'
import socket
sockets = []
try:
    for _ in range(2):
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        sockets.append(sock)
    print(*(sock.getsockname()[1] for sock in sockets))
finally:
    for sock in sockets:
        sock.close()
PY
)

cat > "$temporary_root/config.yaml" <<YAML
version: 1
server:
  listen: 127.0.0.1:${local_service_port}
  proxy_listen: 127.0.0.1:${local_proxy_port}
  data_dir: ${temporary_root}/data
cursor:
  user_dir: ${temporary_root}/Cursor/User
models:
  - id: local-smoke
    name: Local service smoke
    protocol: anthropic
    base_url: http://127.0.0.1:9
    model: unused-offline-model
    api_key_env: CURSOR_AGENT_API_KEY
YAML

CURSOR_AGENT_API_KEY=offline-test-key "$server_bin" serve --config "$temporary_root/config.yaml" >"$temporary_root/service.log" 2>&1 &
service_pid=$!
health_url="http://127.0.0.1:${local_service_port}/healthz"
ready=0
for _ in $(seq 1 50); do
  if curl -fsS "$health_url" >"$temporary_root/health.json" 2>/dev/null; then
    ready=1
    break
  fi
  if ! kill -0 "$service_pid" 2>/dev/null; then
    cat "$temporary_root/service.log" >&2
    exit 1
  fi
  sleep 0.1
done

if [[ "$ready" != 1 ]]; then
  printf 'FAIL local service: health endpoint did not become ready\n' >&2
  cat "$temporary_root/service.log" >&2
  exit 1
fi

python3 - "$temporary_root/health.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    health = json.load(f)
assert health["configured_models"] == 1, health
assert health["enabled_models"] == 1, health
assert health["integration"] == "disabled", health
assert health["settings_applied"] is False, health
print("PASS isolated service startup and health endpoint")
PY
