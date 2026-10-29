#!/bin/sh
# Build tests/welcome_headless.rs and run it in the private headless labwc
# (../global-settings/tests/headless.sh). Extra arguments filter check names.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export TMPDIR=/run/user/1000/cbt/rust-welcome
mkdir -p "$TMPDIR"
BIN=$(cd "$ROOT" && cargo test -p welcome ${WELCOME_FEATURES:-} --test welcome_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="welcome_headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
AGENT=${AGENT:-welcome} "$ROOT/components/global-settings/tests/headless.sh" "$BIN" "$@"
