#!/bin/sh
# Build tests/ui_headless.rs and run it in a private headless labwc with two outputs.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export TMPDIR=/run/user/1000/cbt/rust-ws-prompt
mkdir -p "$TMPDIR"
BIN=$(cd "$ROOT" && cargo test -p "${PKG:-wayshell-theme}" --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
exec "$HERE/headless.sh" "${OUTPUTS:-2}" "$BIN"
