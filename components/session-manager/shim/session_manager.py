#!/usr/bin/env python3
"""Compatibility shim: the Rust `session-manager` binary next to this file now
implements session_manager.py. Same flags: '--popup', '--menu',
'--popup-edge' {top,bottom,left,right}, '--popup-offset' N.
desktop-suite-action enables its centered menu only when this file contains
the quoted '--menu' flag (grep -E "[\"']--menu[\"']"), so keep it here.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("session-manager")
os.execv(str(BINARY), ["session_manager.py", *sys.argv[1:]])
