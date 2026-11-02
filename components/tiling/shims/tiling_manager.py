#!/usr/bin/env python3
"""Tessel Tiling shim: the Rust `tessel-tiling` binary now implements this entry point.

Callers (Tessel Dock panel_actions, ~/.local/bin/tessel-tiling, labwc autostart,
keybinds, tessel-action) keep running
`/usr/bin/python3 ~/.config/waybar/scripts/tessel-tiling/tiling_manager.py <flag>`
with the same flags: '--toggle', '--enable', '--disable', '--restore',
'--status', '--watch', '--settings', '--popup', '--popup-edge', '--popup-offset'.
This file execs the binary with argv [python, this path, *args]: the process
still looks like a Python status script to the installers' Tessel Dock child
checks, and argparse-style messages still say tiling_manager.py.
Global Settings and tessel-shell keep importing tiling_settings, tiling_config,
tiling_client and bindings, which stay installed next to this file.
If the binary is missing it falls back to tiling_manager_python.py.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("WAYTILE_BINARY") or HERE / "tessel-tiling")
FALLBACK = HERE / "tiling_manager_python.py"


def main():
    if os.access(BINARY, os.X_OK):
        os.execv(str(BINARY), [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]])
    if FALLBACK.is_file():
        os.execv(sys.executable, [sys.executable, str(FALLBACK), *sys.argv[1:]])
    print("tessel-tiling: the Window Tiling binary is not installed", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
