#!/bin/sh
# Build and run tests/headless.rs inside a private nested headless labwc
# (tests/headless.sh, tools/headless.md).
set -eu
AGENT=${AGENT:-wayland}
export TMPDIR=/run/user/1000/cbt/rust-$AGENT
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
BIN=$(cd "$ROOT" && cargo test -p tessel-wayland --all-features --test headless --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.reason == "compiler-artifact" and .target.name == "headless" and .executable) | .executable')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
exec "$HERE/headless.sh" "$BIN"
