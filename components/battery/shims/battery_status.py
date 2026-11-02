#!/usr/bin/env python3
"""Shim: the Rust `tessel-battery-status` stream (installed next to this file) replaces
battery_status.py (Tessel Dock runs `python3 -u battery_status.py`)."""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("tessel-battery-status")
os.execv(BINARY, [str(BINARY), *sys.argv[1:]])
