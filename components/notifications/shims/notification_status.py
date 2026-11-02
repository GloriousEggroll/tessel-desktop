#!/usr/bin/env python3
"""Shim: Tessel Dock's notification indicator stream, now the Rust `tessel-notifications-status`."""
import os
import sys

BINARY = os.path.join(os.path.dirname(os.path.realpath(__file__)), "tessel-notifications-status")

if __name__ == "__main__":
    os.execv(BINARY, ["notification_status.py", *sys.argv[1:]])
