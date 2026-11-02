#!/bin/sh
# Build tests/ui_headless.rs and run it in a private headless labwc (one output).
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=tessel-prompt OUTPUTS=1 exec "$HERE/../../tessel-theme/tests/run_headless.sh"
