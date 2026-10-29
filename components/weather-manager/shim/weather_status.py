#!/usr/bin/env python3
"""Compatibility shim: exec the gio-free Rust `weather-status` stream next to
this file (same one-JSON-object-per-line stdout protocol, --watch, --config,
--cache-dir; shared cache and fetch.lock)."""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("weather-status")
os.execv(str(BINARY), ["weather_status.py", *sys.argv[1:]])
