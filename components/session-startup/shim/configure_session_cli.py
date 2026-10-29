#!/usr/bin/env python3
"""Run the Rust `configure-session` (same --check | --apply output) from the suite tree.
configure_session.py itself must stay Python while install_all.py and
shared/install_gtk4.py import it (edit_rc, edit_menu, AUTOSTART_TEMPLATE,
ENVIRONMENT_TEMPLATE, runtime_files); this wrapper is for running the step only.
"""
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = os.environ.get("CONFIGURE_SESSION_BINARY", str(Path.home() / ".local/bin/configure-session"))
os.environ.setdefault("DESKTOP_SUITE_SOURCE", str(ROOT))
os.execv(BINARY, ["configure_session.py", *sys.argv[1:]])
