#!/usr/bin/env python3
"""Shim: the Rust `battery-manager` binary (installed next to this file) replaces
battery_manager.py. Same flags: --popup [--popup-edge {top,bottom,left,right}]
[--popup-offset N] | --settings | --background | --check."""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("battery-manager")
# argv[0] keeps argparse's prog name ("battery_manager.py") in usage/errors.
os.execv(BINARY, ["battery_manager.py", *sys.argv[1:]])
