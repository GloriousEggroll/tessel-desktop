#!/bin/sh
# Build wayclip and tests/headless.rs, then run the checks inside a private
# nested headless labwc (crates/suite-wayland/tests/headless.sh: own
# XDG_RUNTIME_DIR, own D-Bus, no input devices; never the live session).
set -eu
export AGENT=${AGENT:-clipboard}
export TMPDIR=/run/user/1000/cbt/rust-$AGENT
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
cd "$ROOT"
cargo build -q -p clipboard-manager --bin wayclip
BIN=$(cargo test -p clipboard-manager --test headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
exec "$ROOT/crates/suite-wayland/tests/headless.sh" "$BIN" "$ROOT/target/debug/wayclip" "$TMPDIR/scratch"
