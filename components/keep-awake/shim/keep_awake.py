#!/usr/bin/env python3
"""Keep Awake shim: the Rust binary now implements this entry point.

Callers keep running `/usr/bin/python3 ~/.config/waybar/scripts/keep-awake/keep_awake.py
<flag>` with the same flags: '--background', '--popup', '--status', '--enable',
'--disable', '--toggle', '--popup-edge', '--popup-offset'. This file execs the
Rust `keep-awake` binary (argv[0] stays this path, so argparse-style messages
still say keep_awake.py). If the binary is missing it falls back to the saved
Python implementation, keep_awake_python.py, next to this file.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("KEEP_AWAKE_BINARY") or HERE / "keep-awake")
FALLBACK = HERE / "keep_awake_python.py"


def main():
    if os.access(BINARY, os.X_OK):
        os.execv(str(BINARY), [sys.argv[0], *sys.argv[1:]])
    if FALLBACK.is_file():
        os.execv(sys.executable, [sys.executable, str(FALLBACK), *sys.argv[1:]])
    print("keep-awake: the Keep Awake binary is not installed", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
