#!/usr/bin/env python3
"""Compatibility shim: the Rust `tessel-auth` binary next to this
file now implements authentication_manager.py (Polkit agent + BlueZ pairing
agent, Type=notify READY after registration). No CLI flags; argv is passed
through to the GApplication unchanged, exactly as before.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("tessel-auth")
os.execv(str(BINARY), ["authentication_manager.py", *sys.argv[1:]])
