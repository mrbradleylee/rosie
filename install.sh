#!/usr/bin/env sh
set -eu

REPO_URL="${ROSIE_REPO_URL:-https://github.com/mrbradleylee/rosie.git}"
TMP_ROOT="${TMPDIR:-/tmp}"
WORK_DIR="$(mktemp -d "${TMP_ROOT%/}/rosie-install.XXXXXX")"

cleanup() {
    rm -rf "$WORK_DIR"
}

trap cleanup EXIT INT TERM HUP

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'rosie installer error: missing required command: %s\n' "$1" >&2
        exit 1
    fi
}

require_command git
require_command cargo

printf 'Cloning %s\n' "$REPO_URL"
git clone --depth 1 "$REPO_URL" "$WORK_DIR/rosie"

cd "$WORK_DIR/rosie"

printf 'Building Rosie\n'
cargo build --release --locked

printf 'Installing Rosie\n'
"$WORK_DIR/rosie/target/release/rosie" --install
