"""Python half of the gs-system byte-parity test (tests/python_parity.rs).

Run ONLY inside components/settings/tests/headless.sh (fake HOME,
fake XDG dirs, logging fakes on PATH). It imports the read-only Global
Settings modules from $BTMANAGER, applies the same operations the Rust port
applies to every fixture, and writes each result to OUT/<fixture>.<op>.

    python3 -B -s parity_driver.py FIXTURES OUT
"""
import json
import os
from pathlib import Path
import shutil
import sys

if os.environ.get('GS_HEADLESS') != '1' or not os.environ.get('HOME', '').startswith(os.environ.get('GS_ROOT', '/nonexistent')):
    raise SystemExit('parity_driver.py must run inside the private headless session')

BT = Path(os.environ['BTMANAGER'])
sys.path[:0] = [str(BT / 'tessel-settings'), str(BT / 'shared'), str(BT / 'tessel-tiling')]

from xml.parsers.expat import ExpatError  # noqa: E402
import labwc_settings  # noqa: E402
import workspace_config  # noqa: E402
import input_config  # noqa: E402
import keyboard_config  # noqa: E402
import night_light  # noqa: E402
import shortcuts_model  # noqa: E402
from bindings import BINDINGS  # noqa: E402

FIXTURES, OUT = Path(sys.argv[1]), Path(sys.argv[2])
OUT.mkdir(parents=True, exist_ok=True)


def error(exc):
    kind = ('Xml' if isinstance(exc, ExpatError) else 'Value' if isinstance(exc, ValueError)
            else 'Runtime' if isinstance(exc, RuntimeError) else 'Os' if isinstance(exc, OSError) else type(exc).__name__)
    return f'ERROR:{kind}:{exc}'.encode()


def save(name, function):
    try:
        data = function()
    except Exception as exc:  # noqa: BLE001 - every failure is part of the output
        data = error(exc)
    if isinstance(data, str):
        data = data.encode()
    (OUT / name).write_bytes(data)


def b(value):
    return '1' if value else '0'


def keyboard_summary(data):
    section = labwc_settings.read_keyboard(data)
    lines = [f'span={section.span[0]},{section.span[1]}' if section.span else 'span=None',
             f'open_end={section.open_end}', f'self_closing={b(section.self_closing)}', f'root_end={section.root_end}',
             f'has_default={b(section.has_default)}']
    for name in sorted(section.values):
        text, (start, end) = section.values[name]
        lines.append(f'value {name}={text}@{start},{end}')
    for bind in section.keybinds:
        lines.append(f'keybind {bind.key}|{bind.start}|{bind.end}|{b(bind.tiling)}|{b(bind.on_release)}')
        for action in bind.actions:
            lines.append('  action ' + action.name + ' ' + ';'.join(f'{k}={v}' for k, v in action.attributes))
    return '\n'.join(lines) + '\n'


def input_summary(data):
    state = input_config.read_state(data)
    lines = []
    for key, profile in state.profiles.items():
        lines.append(f'profile {key}|{profile.name}')
        lines.extend(f'  {k}={v}' for k, v in profile.values.items())
        lines.append('  xml=' + profile.xml)
    return '\n'.join(lines) + '\n'


def workspace_summary(data):
    state = workspace_config.read_state(data)
    return f'count={state.count}\nsignature={state.signature}\n'


TILING = [(item[0], item[1], item[2], item[3]) for item in BINDINGS]


def shortcuts_summary(data):
    items = shortcuts_model.collect(data, tiling=TILING)
    return ''.join(f'{i.key}|{i.description}|{";".join(f"{k}={v}" for k, v in i.values)}|{i.group}|{i.source}|{b(i.active)}|'
                   f'{i.start}|{i.overrides}\n' for i in items)


INPUT_UPDATES = [
    [(('non-touch', 'non-touch'), {'pointerSpeed': '0.5'})],
    [(('touchpad', 'touchpad'), {'tap': 'no', 'naturalScroll': 'yes', 'clickMethod': 'clickfinger'})],
    [(('Mouse A & B', 'non-touch'), {'leftHanded': 'yes'}), (('non-touch', 'non-touch'), {'pointerSpeed': '-1'})],
    [(('Mouse', 'non-touch'), None)],
    [(('touchpad', 'touchpad'), {'scrollMethod': None, 'tap': None})],
    [(('non-touch', 'non-touch'), {'sendEventsMode': 'no'})],
    [(('Pad <x>', 'touchpad'), {'sendEventsMode': 'disabledOnExternalMouse', 'scrollFactor': '1.5'})],
]
KEYBOARD_VALUES = [{'repeatRate': '40'}, {'repeatRate': '40', 'repeatDelay': '300'}, {'repeatDelay': None},
                   {'repeatRate': None, 'repeatDelay': None}, {'repeatRate': '5 & <6>'}]
COUNTS = [1, 2, 3, 5, 12, 101]

for fixture in sorted(FIXTURES.glob('*.xml')):
    data = fixture.read_bytes()
    stem = fixture.stem
    save(f'{stem}.keyboard', lambda: keyboard_summary(data))
    save(f'{stem}.input', lambda: input_summary(data))
    save(f'{stem}.workspace', lambda: workspace_summary(data))
    save(f'{stem}.shortcuts', lambda: shortcuts_summary(data))
    for count in COUNTS:
        save(f'{stem}.count{count}', lambda: workspace_config.with_count(data, count))
    for index, updates in enumerate(INPUT_UPDATES):
        save(f'{stem}.profiles{index}', lambda: input_config.with_profiles(data, dict(updates), input_config.read_state(data)))
    for index, values in enumerate(KEYBOARD_VALUES):
        save(f'{stem}.kbvalues{index}', lambda: labwc_settings.set_keyboard_values(data, values))
    try:
        binds = labwc_settings.read_keyboard(data).keybinds
    except Exception:  # noqa: BLE001
        binds = []
    for index, bind in enumerate(binds):
        save(f'{stem}.rebind{index}', lambda: labwc_settings.rebind(data, bind.start, bind.key, 'W-C-x\t1'))

ENVIRONMENTS = json.loads((FIXTURES / 'environments.json').read_text())
for index, (text, values) in enumerate(ENVIRONMENTS):
    save(f'env{index}.edit', lambda: labwc_settings.edit_environment(text, dict(values)))
    save(f'env{index}.parse', lambda: ''.join(f'{k}={v}\n' for k, v in sorted(labwc_settings.parse_environment(text).items())))

# File-level flows in the fake HOME: update() backups, keyboard save, night light.
work = OUT / 'work'
shutil.rmtree(work, ignore_errors=True)
work.mkdir()
os.environ['XDG_STATE_HOME'] = str(work / 'state')
os.environ['XDG_CONFIG_HOME'] = str(work / 'config')
for index, (rc_name, env_text, state) in enumerate(json.loads((FIXTURES / 'keyboard_saves.json').read_text())):
    rc, env = work / f'kb{index}.rc.xml', work / f'kb{index}.environment'
    rc.write_bytes((FIXTURES / rc_name).read_bytes())
    env.write_text(env_text)
    reloads = []
    config = keyboard_config.KeyboardConfig(rc, env, reload=lambda done: reloads.append(1))
    state = keyboard_config.KeyboardState(tuple(tuple(item) for item in state[0]), tuple(state[1]), state[2], state[3])
    save(f'kbsave{index}.result', lambda: repr(config.save(state)) + f' reloads={len(reloads)}')
    save(f'kbsave{index}.rc', lambda: rc.read_bytes())
    save(f'kbsave{index}.env', lambda: env.read_bytes())
    backup = work / 'state/tessel/backups/labwc-rc' / (rc.name + '.orig')
    save(f'kbsave{index}.rcbackup', lambda: backup.read_bytes() if backup.exists() else b'(none)')
for index, config in enumerate(json.loads((FIXTURES / 'night.json').read_text())):
    store = night_light.NightLightStore(work / f'night{index}.json', work / 'weather.json')
    save(f'night{index}.result', lambda: repr(store.save(night_light.NightLightConfig(**config))))
    save(f'night{index}.file', lambda: (work / f'night{index}.json').read_bytes() if (work / f'night{index}.json').exists() else b'(none)')
    save(f'night{index}.command', lambda: repr(night_light.command(store.load(), (('X', 59.91, 10.75)), ['DP-1'], ['DP-1', 'HDMI-A-1'])))
print('python parity outputs:', len(list(OUT.iterdir())))
