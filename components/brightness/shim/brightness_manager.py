#!/usr/bin/env python3
"""Compatibility shim: the Rust `tessel-brightness` binary next to this file
now implements brightness_manager.py. Same flags: "--popup" | "--settings" |
"--background" | "--check" | "--brightness-up" | "--brightness-down" |
"--idle-dim" TOKEN | "--idle-restore" TOKEN, plus "--popup-edge" and
"--popup-offset". Keybinds, Tessel Dock and tessel-action call this path.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("tessel-brightness")
os.execv(str(BINARY), ["brightness_manager.py", *sys.argv[1:]])
