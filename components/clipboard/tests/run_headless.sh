#!/bin/sh
# Build tessel-clipboard and tests/headless.rs, then run the checks inside a private
# nested headless labwc (crates/wayland/tests/headless.sh: own
# XDG_RUNTIME_DIR, own D-Bus, no input devices; never the live session).
set -eu
export AGENT=${AGENT:-clipboard}
export TMPDIR=/run/user/1000/cbt/rust-$AGENT
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
cd "$ROOT"
cargo build -q -p tessel-clipboard --bin tessel-clipboard
BIN=$(cargo test -p tessel-clipboard --test headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
exec "$ROOT/crates/wayland/tests/headless.sh" "$BIN" "$ROOT/target/debug/tessel-clipboard" "$TMPDIR/scratch"
