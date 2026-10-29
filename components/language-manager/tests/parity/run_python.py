"""Apply a language with the Python backend and a fake localed/accounts in HOME=argv[1].

Read-only use of ~/btmanager: imports language_backend and the fake from its tests.
Prints the JSON {result, calls} for comparison with the Rust backend.
"""
import json, os, sys
from pathlib import Path
SOURCE = Path("/home/tcrider/btmanager/language-manager")
sys.path.insert(0, str(SOURCE))
sys.path.insert(1, str(SOURCE.parent / "shared"))
sys.dont_write_bytecode = True
from language_backend import LanguageBackend, LANGUAGES
from test_language_backend import FakeServices

home, code, environment = Path(sys.argv[1]), sys.argv[2], json.loads(sys.argv[3])
services = FakeServices()
backend = LanguageBackend(home=home, services=services, environment=environment,
    locale_provider=lambda: [item.locale.replace("UTF-8", "utf8") for item in LANGUAGES])
result = backend.apply(code)
print(json.dumps({"ok": result.ok, "error": result.error, "warnings": list(result.warnings),
                  "rolled_back": result.rolled_back, "partial": result.partial,
                  "calls": [[kind, value if isinstance(value, list) else [value] if isinstance(value, str) else [f"{k}={v}" for k, v in value.items()]]
                            for kind, value in services.calls],
                  "locale": services.locale}, ensure_ascii=False))
