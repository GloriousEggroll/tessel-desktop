#!/bin/sh
# Build tests/ui_headless.rs and run it in a private headless labwc (one output).
HERE=$(cd "$(dirname "$0")" && pwd)
PKG=wayshell-prompt OUTPUTS=1 exec "$HERE/../../wayshell-theme/tests/run_headless.sh"
