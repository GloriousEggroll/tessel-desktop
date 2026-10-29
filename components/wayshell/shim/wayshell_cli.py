#!/usr/bin/env python3
"""Lightweight keyboard launcher for the existing Wayshell daemon.

Compatibility shim: run as a program it execs the Rust `wayshell-cli` next to
this file (same subcommands, flags, output and exit codes). waydock.py's hot
corners and installers call this path with python3; labwc's A-Tab keybind
should call `wayshell-cli` directly (no Python startup). The functions stay
importable because wayshell_switcher.py and tests import `monitor_name`.
"""

import argparse
import json
import os
from pathlib import Path
import socket
import sys

BINARY = Path(__file__).resolve().with_name("wayshell-cli")


def monitor_name(value):
    if (not isinstance(value, str) or not value or len(value) > 128
            or not value.isprintable() or value.strip() != value):
        raise argparse.ArgumentTypeError("Monitor must be a valid output connector name.")
    return value


def request(payload, timeout=2):
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        raise RuntimeError("No desktop session runtime directory is available.")
    path = Path(runtime) / "wayshell.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(timeout)
        stream.connect(str(path))
        stream.sendall(json.dumps(payload).encode() + b"\n")
        data = bytearray()
        while b"\n" not in data:
            chunk = stream.recv(4096)
            if not chunk:
                raise RuntimeError("Wayshell closed the command connection.")
            data.extend(chunk)
            if len(data) > 65536:
                raise RuntimeError("Wayshell returned an oversized response.")
    reply = json.loads(data.split(b"\n", 1)[0])
    if not isinstance(reply, dict):
        raise RuntimeError("Wayshell returned an invalid response.")
    return reply


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    switch = sub.add_parser("switch-window", help="advance the Alt+Tab switcher")
    switch.add_argument("--reverse", action="store_true")
    activities = sub.add_parser("show-activities", help="show the persistent window overview")
    activities.add_argument("--monitor", type=monitor_name, default=None)
    sub.add_parser("switcher-status", help="check the running switcher without opening it")
    args = parser.parse_args(argv)
    payload = {"cmd": args.command}
    if args.command == "switch-window":
        payload["reverse"] = args.reverse
    elif args.command == "show-activities" and args.monitor is not None:
        payload["monitor"] = args.monitor
    try:
        reply = request(payload)
        if args.command == "switcher-status":
            print(json.dumps(reply))
        if not reply.get("ok"):
            print(reply.get("msg", "The desktop shell could not handle the request."), file=sys.stderr)
            return 1
    except (OSError, ValueError, RuntimeError) as error:
        print("Wayshell: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    if BINARY.is_file() and os.access(BINARY, os.X_OK):
        os.execv(str(BINARY), ["wayshell_cli.py", *sys.argv[1:]])
    raise SystemExit(main())
