#!/usr/bin/env python3
"""Compatibility shim for ~/.config/waybar/scripts/tessel-bluetooth/tessel-bluetooth.py.

Run as a program (the dock, power/battery managers, keybinds), it execs the
Rust `tessel-bluetooth` binary next to this file with the same arguments:
"--popup", "--popup-edge", "--popup-offset", "--lang", "--selftest",
"--gen-pot", "--gen-mo". Imported as a module (Global Settings'
applet_pages.py embeds `tessel-bluetooth.App`), it loads the original Python
implementation, installed beside it as tessel-bluetooth_gtk.py, unchanged.
"""
import os
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_BINARY = _HERE / "tessel-bluetooth"
_ORIGINAL = _HERE / "tessel-bluetooth_gtk.py"

if __name__ == "__main__":
    if os.access(_BINARY, os.X_OK):
        os.execv(str(_BINARY), ["tessel-bluetooth.py", *sys.argv[1:]])
    import runpy
    sys.argv[0] = str(_ORIGINAL)
    runpy.run_path(str(_ORIGINAL), run_name="__main__")
else:
    __file__ = str(_ORIGINAL)
    exec(compile(_ORIGINAL.read_text(encoding="utf-8"), str(_ORIGINAL), "exec"))
