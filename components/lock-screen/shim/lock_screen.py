#!/usr/bin/env python3
"""Shim: the lock screen is the Rust `lock-screen` binary next to this file.

Kept at the old path for callers that run `lock_screen.py --check` /
`--preview` (install_lock.py, Global Settings). Arguments are passed
through unchanged (the Rust CLI accepts the same argparse prefixes).
"""
import os
import sys

binary = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'lock-screen')
os.execv(binary, [binary] + sys.argv[1:])
