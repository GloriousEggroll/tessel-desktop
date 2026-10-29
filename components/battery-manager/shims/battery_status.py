#!/usr/bin/env python3
"""Shim: the Rust `battery-status` stream (installed next to this file) replaces
battery_status.py (Waydock runs `python3 -u battery_status.py`)."""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("battery-status")
os.execv(BINARY, [str(BINARY), *sys.argv[1:]])
