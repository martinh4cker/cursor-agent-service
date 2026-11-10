#!/usr/bin/env bash
set -euo pipefail

config_path="${1:-./config.yaml}"
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ -n "${CURSOR_SERVER_BIN:-}" ]]; then
  server_bin="$CURSOR_SERVER_BIN"
elif [[ -x "$project_root/target/release/cursor-server" ]]; then
  server_bin="$project_root/target/release/cursor-server"
else
  cargo build --release --locked -p cursor-server --bin cursor-server
  server_bin="$project_root/target/release/cursor-server"
fi

"$server_bin" check --config "$config_path"

if [[ -f "/Applications/Cursor.app/Contents/Info.plist" ]] && [[ -x /usr/libexec/PlistBuddy ]]; then
  cursor_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' /Applications/Cursor.app/Contents/Info.plist)"
  printf 'INFO Cursor version: %s\n' "$cursor_version"
else
  printf 'SKIP Cursor version: Cursor.app was not found at /Applications/Cursor.app\n'
fi

"$server_bin" doctor --config "$config_path"
"$server_bin" smoke --config "$config_path"

printf '%s\n' 'SKIP Cursor Agent UI: restart Cursor, open a new Agent chat, then verify file read/edit, terminal, follow-up, cancellation, MCP, and subagent behavior.'
