#!/usr/bin/env python3
"""Compatibility shim: exec the gio-only Rust `tessel-brightness-status` stream next to
this file (same one-JSON-object-per-line stdout protocol as before)."""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("tessel-brightness-status")
os.execv(str(BINARY), [str(BINARY), *sys.argv[1:]])
