#!/usr/bin/env python3
"""Compatibility shim: exec the gio-only Rust `tessel-network-status` stream next to
this file (same one-JSON-object-per-line stdout protocol as before)."""
import os
import sys
from pathlib import Path

if __name__ == "__main__":
    BINARY = Path(__file__).resolve().with_name("tessel-network-status")
    os.execv(str(BINARY), [str(BINARY), *sys.argv[1:]])
else:
    from network_status_impl import *  # noqa: F401,F403
