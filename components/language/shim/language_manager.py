#!/usr/bin/env python3
"""Language Settings shim: the Rust binary now implements this entry point.

Callers keep running `/usr/bin/python3 ~/.local/share/tessel-language/language_manager.py
[--status] [--quit]` (the installer's standalone-instance shutdown sends '--quit').
This file execs the Rust `tessel-language` binary installed next to it (argv[0]
stays this path, so argparse-style messages still say language_manager.py). If the
binary is missing it falls back to the saved Python implementation,
language_manager_python.py, next to this file.

language_backend.py, language_ui.py and language_style.py stay installed: Global
Settings (language_page.py) imports and embeds the Python wizard.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BINARY = Path(os.environ.get("LANGUAGE_MANAGER_BINARY") or HERE / "tessel-language")
FALLBACK = HERE / "language_manager_python.py"


def main():
    if os.access(BINARY, os.X_OK):
        os.execv(str(BINARY), [sys.argv[0], *sys.argv[1:]])
    if FALLBACK.is_file():
        os.execv(sys.executable, [sys.executable, str(FALLBACK), *sys.argv[1:]])
    print("tessel-language: the Language Settings binary is not installed", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
