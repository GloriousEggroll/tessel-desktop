#!/bin/sh
# Build tests/welcome_headless.rs and run it in the private headless labwc
# (../tessel-settings/tests/headless.sh). Extra arguments filter check names.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export TMPDIR=/run/user/1000/cbt/rust-welcome
mkdir -p "$TMPDIR"
BIN=$(cd "$ROOT" && cargo test -p welcome ${WELCOME_FEATURES:-} --test welcome_headless --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.reason == "compiler-artifact" and .target.name == "welcome_headless" and .executable) | .executable')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
AGENT=${AGENT:-welcome} "$ROOT/components/settings/tests/headless.sh" "$BIN" "$@"
