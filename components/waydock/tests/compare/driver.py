"""Scenario driver for run.sh (runs INSIDE the private headless labwc).

For each scenario and implementation (python, rust) it builds a fresh fake
HOME, starts the dock with a private NOTIFY_SOCKET, and records:
  ready_ms      launch -> READY=1 datagram
  rss_kb/pss_kb after 3 s idle
  menu_ms       `waydock --menu` invocation -> first changed frame (grim poll)
and screenshots <out>/<scenario>-<impl>-{panel,menu,context,preview}.png,
then compares python vs rust pixelwise (diff.py rules).
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(sys.argv[1])
RUST = sys.argv[2]
SCENARIOS = sys.argv[3:] or ["dock", "bar"]
OUT = ROOT / "out"
PY_DOCK = "/home/tcrider/btmanager/waydock/waydock.py"
HERE = Path(__file__).resolve().parent

if not os.environ.get("XDG_RUNTIME_DIR", "").startswith(str(ROOT)) or os.environ.get("WLR_BACKENDS") != "headless":
    sys.exit("refusing to run outside the private headless session")

STATUS = {
    "power-manager/power_status.py": {"text": "\U000f0425", "tooltip": "Balanced", "class": "balanced"},
    "battery-manager/battery_status.py": {"text": "", "class": "hidden"},
    "brightness-manager/brightness_status.py": {"text": "\U000f00df", "tooltip": "Brightness 60%"},
    "network-manager/network_status.py": {"text": "\U000f0928", "tooltip": "Wired", "class": "ethernet"},
    "capslock.py": {"text": ""},
    "notification-manager/notification_status.py": {"text": "\U000f009a", "tooltip": "No notifications"},
    "weather-manager/weather_status.py": {"text": "21°", "tooltip": "Clear"},
    "on-screen-keyboard/osk_status.py": {"text": "\U000f030c", "tooltip": "Keyboard"},
    "tiling-manager/tiling_status.py": {"text": "\U000f0570", "tooltip": "Tiling off", "class": "off"},
}

BASE_CONFIG = {
    "version": 1,
    "pins": ["org.mozilla.firefox.desktop", "org.gnome.Ptyxis.desktop", "org.gnome.Meld.desktop"],
    "group_visibility": {"clock": False},
}
# Pointer targets measured on the Python screenshots (1280x720 output):
# first pinned app (firefox) and the applets expander.
TARGETS = {"dock": {"pin": (187, 676), "applets": (1071, 676), "away": (640, 300)},
           "bar": {"pin": (588, 34), "applets": (1243, 34), "away": (640, 400)},
           "custombar": {"pin": (624, 670), "applets": (1208, 670), "away": (640, 300)},
           # Python shows only the menu button in the center of a 1280x720 left bar.
           "customleft": {"pin": (52, 350), "applets": (52, 678), "away": (640, 300)},
           "customdock": {"pin": (447, 675), "applets": (914, 675), "away": (640, 300)}}
FAKE_APP = """
import sys, gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gio, Gtk
app = Gtk.Application(application_id="org.mozilla.firefox", flags=Gio.ApplicationFlags.NON_UNIQUE)
def activate(app):
    for title in ("Fake Window One", "Fake Window Two"):
        w = Gtk.ApplicationWindow(application=app, title=title)
        w.set_default_size(420, 260)
        w.set_child(Gtk.Label(label=title))
        w.present()
app.connect("activate", activate)
app.run([])
"""
# The real-config custom layout whose center section once rendered empty.
CUSTOM_LAYOUT = {"start": ["weather", "workspaces", "focused_title"],
                 "center": ["app_menu", "activities", "pinned", "running", "files"],
                 "end": ["media", "system_tray", "service_apps", "clock", "session"]}
SCENARIO_CONFIG = {
    "dock": {"mode": "dock", "edge": "bottom", "alignment": "center"},
    "bar": {"mode": "bar", "edge": "top"},
    "custombar": {"mode": "bar", "edge": "bottom", "group_layouts": {"bar": CUSTOM_LAYOUT}},
    "customleft": {"mode": "bar", "edge": "left", "group_layouts": {"bar": CUSTOM_LAYOUT}},
    "customdock": {"mode": "dock", "edge": "bottom", "alignment": "center",
                   "group_layouts": {"dock": {"start": ["workspaces"], "center": ["app_menu", "pinned", "running", "files"],
                                              "end": ["system_tray", "session"]}}},
}


def make_home(name, scenario):
    home = ROOT / "homes" / name
    shutil.rmtree(home, ignore_errors=True)
    for sub in (".config/waydock", ".config/labwc", ".cache", ".local/share", ".local/state", ".config/waybar/scripts"):
        (home / sub).mkdir(parents=True, exist_ok=True)
    (home / ".config/labwc/rc.xml").write_text("<labwc_config/>\n")
    config = dict(BASE_CONFIG, **SCENARIO_CONFIG[scenario])
    (home / ".config/waydock/config.json").write_text(json.dumps(config, indent=2) + "\n")
    for path, payload in STATUS.items():
        script = home / ".config/waybar/scripts" / path
        script.parent.mkdir(parents=True, exist_ok=True)
        script.write_text("import json, sys, time\nprint(json.dumps(%r), flush=True)\n"
                          "while True: time.sleep(3600)\n" % (payload,))
    return home


def environment(home):
    env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_CACHE_HOME=str(home / ".cache"), XDG_DATA_HOME=str(home / ".local/share"),
               XDG_STATE_HOME=str(home / ".local/state"), XDG_CONFIG_DIRS=str(ROOT / "none"))
    env.pop("LABWC_PID", None)  # no labwc --reconfigure from window_buttons
    return env


def grim(path, region=None):
    argv = ["grim"] + (["-g", region] if region else []) + [str(path)]
    subprocess.run(argv, check=True, timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def raw_frame(region=None):
    argv = ["grim", "-t", "ppm"] + (["-g", region] if region else []) + ["-"]
    return subprocess.run(argv, check=True, timeout=10, capture_output=True).stdout


def memory(pid):
    rss = pss = 0
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1])
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
        if line.startswith("Pss:"):
            pss = int(line.split()[1])
    return rss, pss


def start(argv, env, notify_path):
    if notify_path.exists():
        notify_path.unlink()
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    listener.bind(str(notify_path))
    env = dict(env, NOTIFY_SOCKET=str(notify_path))
    t0 = time.monotonic()
    process = subprocess.Popen(argv, env=env, stdout=open(notify_path.with_suffix(".log"), "w"),
                               stderr=subprocess.STDOUT, start_new_session=True)
    listener.settimeout(30)
    ready = None
    try:
        while ready is None:
            data = listener.recv(4096).decode()
            if "READY=1" in data:
                ready = (time.monotonic() - t0) * 1000
    except socket.timeout:
        pass
    listener.close()
    return process, ready


def run_cli(argv, env, *args):
    return subprocess.run(argv + list(args), env=env, capture_output=True, text=True, timeout=20)


def menu_latency(argv, env):
    before = raw_frame()
    t0 = time.monotonic()
    subprocess.Popen(argv + ["--menu"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    while time.monotonic() - t0 < 10:
        if raw_frame() != before:
            # wait for the frame to settle (menu fully drawn)
            first = (time.monotonic() - t0) * 1000
            last = raw_frame()
            while True:
                time.sleep(0.05)
                frame = raw_frame()
                if frame == last:
                    return first, (time.monotonic() - t0) * 1000 - 50
                last = frame
    return None, None


def pointer(x, y, click=None):
    subprocess.run(["wlrctl", "pointer", "move", "-100000", "-100000"], capture_output=True, timeout=3)
    subprocess.run(["wlrctl", "pointer", "move", str(x), str(y)], capture_output=True, timeout=3)
    if click:
        subprocess.run(["wlrctl", "pointer", "click", click], capture_output=True, timeout=3)


def scenario_run(scenario, name, argv):
    home = make_home(name + "-" + scenario, scenario)
    env = environment(home)
    result = {"scenario": scenario, "impl": name}
    process, ready = start(argv, env, ROOT / f"n-{name}-{scenario}.sock")
    result["ready_ms"] = ready
    try:
        time.sleep(3)
        if process.poll() is not None:
            result["error"] = f"exited {process.returncode}"
            return result
        result["rss_kb"], result["pss_kb"] = memory(process.pid)
        check = run_cli(argv, env, "--check")
        result["check_rc"] = check.returncode
        (OUT / f"{scenario}-{name}-check.json").write_text(check.stdout)
        pointer(5000, 5000)
        time.sleep(0.5)
        grim(OUT / f"{scenario}-{name}-panel.png")
        first, settled = menu_latency(argv, env)
        result["menu_first_frame_ms"], result["menu_settled_ms"] = first, settled
        time.sleep(0.5)
        grim(OUT / f"{scenario}-{name}-menu.png")
        run_cli(argv, env, "--menu")
        time.sleep(0.8)
        grim(OUT / f"{scenario}-{name}-after-menu.png")
        result["rss_after_menu_kb"], result["pss_after_menu_kb"] = memory(process.pid)
        target = TARGETS[scenario]
        pointer(*target["pin"])
        time.sleep(0.3)
        subprocess.run(["wlrctl", "pointer", "click", "right"], capture_output=True, timeout=3)
        time.sleep(0.6)
        grim(OUT / f"{scenario}-{name}-context.png")
        pointer(*target["away"], click="left")
        time.sleep(0.5)
        pointer(*target["applets"])
        time.sleep(0.3)
        subprocess.run(["wlrctl", "pointer", "click"], capture_output=True, timeout=3)
        time.sleep(0.8)
        grim(OUT / f"{scenario}-{name}-applets.png")
        pointer(*target["away"], click="left")
        time.sleep(0.5)
        app = subprocess.Popen([sys.executable, "-c", FAKE_APP], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(1.5)
            pointer(*target["pin"])
            time.sleep(1.5)
            grim(OUT / f"{scenario}-{name}-preview.png")
            pointer(*target["away"])
        finally:
            app.terminate()
            app.wait(timeout=5)
        result["rss_end_kb"], result["pss_end_kb"] = memory(process.pid)
    finally:
        process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        time.sleep(0.5)
    return result


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    impls = [("python", ["/usr/bin/python3", PY_DOCK])]
    if RUST and Path(RUST).exists() and os.environ.get("ONLY_PYTHON") != "1":
        impls.append(("rust", [RUST]))
    results = []
    for scenario in SCENARIOS:
        for name, argv in impls:
            results.append(scenario_run(scenario, name, argv))
            print(json.dumps(results[-1]), flush=True)
    (OUT / "results.json").write_text(json.dumps(results, indent=2))
    if len(impls) > 1:
        subprocess.run([sys.executable, "-I", str(HERE / "diff.py"), str(OUT)] + SCENARIOS)


main()
