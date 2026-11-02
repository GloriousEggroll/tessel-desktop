"""Reference outputs of the Python tiling_config.edit_bindings / tiling_gaps.with_gap.

argv: <tessel-tiling source dir> <tessel-settings dir> <input file>...
Prints one JSON object per input: {"on", "off", "onoff", "gap0", "gap7"} as
base64 bytes or {"error": text}. Read-only: imports from the source trees
with -B (no bytecode written).
"""
import base64, json, sys
sys.path[:0] = [sys.argv[1], sys.argv[2]]
from tiling_config import edit_bindings
from tiling_gaps import with_gap

def attempt(function):
    try:
        value = function()
        if isinstance(value, tuple):
            return [base64.b64encode(value[0]).decode(), value[1]]
        return base64.b64encode(value).decode()
    except Exception as error:
        return {'error': type(error).__name__}

for name in sys.argv[3:]:
    data = open(name, 'rb').read()
    on = None
    try:
        on = edit_bindings(data, True, '/tmp/tessel-tiling')
    except Exception:
        pass
    print(json.dumps({
        'on': attempt(lambda: edit_bindings(data, True, '/tmp/tessel-tiling')),
        'off': attempt(lambda: edit_bindings(data, False, '/tmp/tessel-tiling')),
        'onoff': attempt(lambda: edit_bindings(on, False, '/tmp/tessel-tiling')) if on is not None else None,
        'gap0': attempt(lambda: with_gap(data, 0)),
        'gap7': attempt(lambda: with_gap(data, 7)),
    }))
