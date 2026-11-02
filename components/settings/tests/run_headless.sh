#!/bin/sh
# Build the Global Settings shell tests and run them in the private headless
# labwc (tests/headless.sh). Extra arguments filter check names.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export TMPDIR=/run/user/1000/cbt/rust-gs
mkdir -p "$TMPDIR"
BIN=$(cd "$ROOT" && cargo test -p tessel-settings ${GS_FEATURES:-} --test shell_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="shell_headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
AGENT=${AGENT:-gs-shell} "$HERE/headless.sh" "$BIN" "$@"
