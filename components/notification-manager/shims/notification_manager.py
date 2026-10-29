#!/usr/bin/env python3
"""Shim: the notification manager now runs as the Rust binary next to this file.

Flags (kept here for desktop-suite-action's grep): '--daemon', '--check',
'--settings', '--popup', '--popup-edge', '--popup-offset'.
"""
import os
import sys

BINARY = os.path.join(os.path.dirname(os.path.realpath(__file__)), "notification-manager")

if __name__ == "__main__":
    os.execv(BINARY, ["notification_manager.py", *sys.argv[1:]])
