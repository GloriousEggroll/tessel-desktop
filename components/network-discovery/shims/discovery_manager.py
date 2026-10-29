#!/usr/bin/python3
"""Discover Network Devices shim: the Rust `network-discovery` binary now
implements this entry point.

Callers (Waydock panel_actions, ~/.local/bin/network-discovery, the installer
health check, desktop-suite-action) keep running
`/usr/bin/python3 ~/.config/waybar/scripts/network-discovery/discovery_manager.py <flag>`
with the same flags: '--toggle', '--popup', '--settings', '--status',
'--popup-edge', '--popup-offset'. This file execs the binary with argv
[python, this path, *args] so argparse-style messages still say
discovery_manager.py. Waydock keeps importing discovery_backend.py in-process
(its own copy in ~/.local/share/waydock). Falls back to
discovery_manager_python.py when the binary is missing.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("NETWORK_DISCOVERY_BINARY") or HERE / "network-discovery")
FALLBACK = HERE / "discovery_manager_python.py"


def main():
    if os.access(BINARY, os.X_OK):
        os.execv(str(BINARY), [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]])
    if FALLBACK.is_file():
        os.execv(sys.executable, [sys.executable, str(FALLBACK), *sys.argv[1:]])
    print("network-discovery: the discovery binary is not installed", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
