#!/usr/bin/env python3
"""Compatibility shim: the Rust `weather-manager` binary next to this file now
implements weather_manager.py. Same flags: --popup | --settings | --tray,
--popup-edge {top,bottom,left,right}, --popup-offset N. The other weather_*.py
modules stay installed: Global Settings (weather_settings) and the lock screen
(weather_backend) still import them.
"""
import os
import sys
from pathlib import Path

BINARY = Path(__file__).resolve().with_name("weather-manager")
os.execv(str(BINARY), ["weather_manager.py", *sys.argv[1:]])
