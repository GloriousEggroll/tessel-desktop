#!/usr/bin/env python3
"""Compatibility shim: the Rust `tessel-calendar` binary next to this file now
implements calendar_manager.py. Same flags: "--popup" | "--settings",
"--popup-edge" {top,bottom,left,right}, "--popup-offset" N (and -h/--help).
Tessel Dock's panel_actions launches this path; tessel-action greps
component sources for quoted flags, so they are kept here.

Global Settings still imports date_settings.DateSettingsWindow and
date_backend.DateBackend (and their helpers) from this directory: keep those
Python modules installed beside this shim until Global Settings is ported.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("tessel-calendar")
os.execv(str(BINARY), ["calendar_manager.py", *sys.argv[1:]])
