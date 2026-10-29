#!/usr/bin/env python3
"""Waytile status stream shim (Waydock runs `/usr/bin/python3 -u <this> `).

Execs the Rust `tiling-status` binary (the `--watch` loop without GTK
linked; one JSON line per change, 1 s cadence) as [python, this path], so
the installers' Waydock child checks still see a Python status script.
Falls back to the saved Python implementation if it is missing.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("TILING_STATUS_BINARY") or HERE / "tiling-status")
FALLBACK = HERE / "tiling_status_python.py"

if __name__ == "__main__":
    if os.access(BINARY, os.X_OK):
        os.execv(str(BINARY), [sys.executable, str(Path(__file__).resolve())])
    if FALLBACK.is_file():
        os.execv(sys.executable, [sys.executable, "-u", str(FALLBACK)])
    print("waytile: the Window Tiling binary is not installed", file=sys.stderr)
    raise SystemExit(1)
