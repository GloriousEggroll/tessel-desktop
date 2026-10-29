#!/usr/bin/env python3
"""Shim: Waydock's notification indicator stream, now the Rust `notification-status`."""
import os
import sys

BINARY = os.path.join(os.path.dirname(os.path.realpath(__file__)), "notification-status")

if __name__ == "__main__":
    os.execv(BINARY, ["notification_status.py", *sys.argv[1:]])
