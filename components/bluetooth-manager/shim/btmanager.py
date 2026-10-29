#!/usr/bin/env python3
"""Compatibility shim for ~/.config/waybar/scripts/btmanager/btmanager.py.

Run as a program (the dock, power/battery managers, keybinds), it execs the
Rust `btmanager` binary next to this file with the same arguments:
"--popup", "--popup-edge", "--popup-offset", "--lang", "--selftest",
"--gen-pot", "--gen-mo". Imported as a module (Global Settings'
applet_pages.py embeds `btmanager.App`), it loads the original Python
implementation, installed beside it as btmanager_gtk.py, unchanged.
"""
import os
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_BINARY = _HERE / "btmanager"
_ORIGINAL = _HERE / "btmanager_gtk.py"

if __name__ == "__main__":
    if os.access(_BINARY, os.X_OK):
        os.execv(str(_BINARY), ["btmanager.py", *sys.argv[1:]])
    import runpy
    sys.argv[0] = str(_ORIGINAL)
    runpy.run_path(str(_ORIGINAL), run_name="__main__")
else:
    __file__ = str(_ORIGINAL)
    exec(compile(_ORIGINAL.read_text(encoding="utf-8"), str(_ORIGINAL), "exec"))
