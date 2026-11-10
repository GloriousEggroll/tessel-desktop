#!/bin/sh
# Compile and run tests/upright.c against copy_upright() from the native helper.
# Needs only a C compiler (not the PipeWire headers).
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
W=$(mktemp -d -p "${TMPDIR:-/run/user/$(id -u)}")
trap 'rm -rf "$W"' EXIT
awk '/^void copy_upright/{f=1} f{print} f&&/^}/{exit}' "$HERE/../native/capture_pipewire.c" > "$W/copy_upright.c"
cp "$HERE/upright.c" "$W/"
cc -std=gnu11 -Wall -Wextra -Werror -O2 -I"$W" -o "$W/upright" "$W/upright.c"
"$W/upright"
