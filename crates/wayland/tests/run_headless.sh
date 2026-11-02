#!/bin/sh
# Build and run tests/headless.rs inside a private nested headless labwc
# (tests/headless.sh, tools/headless.md). Also builds the original C helpers
# from ~/tessel-bluetooth (read-only) into $TMPDIR for byte-for-byte comparisons;
# without them the C parity checks fail with a clear message.
set -eu
AGENT=${AGENT:-wayland}
export TMPDIR=/run/user/1000/cbt/rust-$AGENT
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
BTM=${BTMANAGER:-$HOME/tessel-bluetooth}
C="$TMPDIR/c-helpers"
if [ -d "$BTM/tessel-dock" ] && [ ! -x "$C/tessel-overview" ]; then
  mkdir -p "$C"
  PYTHONDONTWRITEBYTECODE=1 python3 -I -c "
import sys; sys.path[:0] = ['$BTM/tessel-dock', '$BTM/tessel-shell']
from build_helper import build_all; build_all('$C')
from build_overview import build; build('$C/tessel-overview')" >/dev/null
fi
BIN=$(cd "$ROOT" && cargo test -p tessel-wayland --all-features --test headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
exec env SUITE_WAYLAND_C_HELPERS="$C" "$HERE/headless.sh" "$BIN"
