#!/usr/bin/env python3
"""Compatibility shim: the Rust `media-manager` binary next to this file now
implements media_manager.py (--popup, --popup-edge, --popup-offset)."""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("media-manager")
os.execv(str(BINARY), ["media_manager.py", *sys.argv[1:]])
