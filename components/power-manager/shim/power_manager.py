#!/usr/bin/env python3
"""Compatibility shim: the Rust `power-manager` binary next to this file now
implements power_manager.py. Same flags: --popup | --settings | --background |
--check | --idle-action A | --idle-resume A | "--power-button", plus
--policy-token, --popup-edge, --popup-offset. desktop-suite-action greps this
file for the quoted "--power-button" flag, so keep it here.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("power-manager")
os.execv(str(BINARY), ["power_manager.py", *sys.argv[1:]])
