#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TUI_FILE="$ROOT_DIR/src/tui.rs"
README_FILE="$ROOT_DIR/README.md"
MAN_FILE="$ROOT_DIR/man/rosie.1"

if ! grep -q "command_shortcuts_line()" "$TUI_FILE"; then
  echo "docs parity check failed: help_rows is not using command_shortcuts_line()" >&2
  exit 1
fi

palette_line="$(grep -E '^const PALETTE_COMMANDS: \[&str; [0-9]+\] = \[.*\];$' "$TUI_FILE")"
if [[ -z "$palette_line" ]]; then
  echo "docs parity check failed: unable to locate PALETTE_COMMANDS constant" >&2
  exit 1
fi

commands_csv="$(echo "$palette_line" | sed -E 's/.*=[[:space:]]*\[(.*)\][[:space:]]*;.*/\1/' | tr -d '"[:space:]')"
IFS=',' read -r -a commands <<< "$commands_csv"

if [[ "${#commands[@]}" -eq 0 ]]; then
  echo "docs parity check failed: parsed command list is empty" >&2
  exit 1
fi

for cmd in "${commands[@]}"; do
  if ! grep -q ":${cmd}" "$README_FILE"; then
    echo "docs parity check failed: README missing :${cmd}" >&2
    exit 1
  fi
  if ! grep -q ":${cmd}" "$MAN_FILE"; then
    echo "docs parity check failed: man page missing :${cmd}" >&2
    exit 1
  fi
done

if rg -n ':[mM]odel(\b|\s|`|$)' "$README_FILE" "$MAN_FILE" >/dev/null; then
  echo "docs parity check failed: found deprecated :model command reference" >&2
  rg -n ':[mM]odel(\b|\s|`|$)' "$README_FILE" "$MAN_FILE" >&2
  exit 1
fi

echo "command/docs parity checks passed"
