#!/usr/bin/env python3
"""Compatibility shim: the Rust `desktop-suite-failed` binary next to this file now
implements desktop_suite_failed.py (usage: desktop_suite_failed.py UNIT.service).
desktop-suite-failed@.service still runs `python3 <this file> %i`.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("desktop-suite-failed")
os.execv(str(BINARY), ["desktop_suite_failed.py", *sys.argv[1:]])
