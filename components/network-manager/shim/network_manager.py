#!/usr/bin/env python3
"""Compatibility shim for the Rust `network-manager` binary next to this file.

Run as a program (netman.service, waydock, the .desktop entry, installer
health checks) it execs the Rust binary with the same flags:
--popup | --settings | --background | --check, --popup-edge, --popup-offset.

Imported as a module (Global Settings' network page imports
NetworkApplication and friends), it re-exports the original Python module,
which the installer keeps as network_manager_impl.py until Global Settings is
ported. Importing never execs anything.
"""
import os
import sys
from pathlib import Path

if __name__ == "__main__":
    BINARY = Path(__file__).resolve().with_name("network-manager")
    os.execv(str(BINARY), ["network_manager.py", *sys.argv[1:]])
else:
    from network_manager_impl import *  # noqa: F401,F403
    from network_manager_impl import (APPLICATION_ID, FALLBACK_PORTAL_URI,  # noqa: F401
                                      NetworkApplication, arguments, portal_uri)
