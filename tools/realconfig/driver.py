"""Runs INSIDE the private headless labwc (run.sh). For each implementation it
starts tessel-shell then tessel-dock against a fresh copy of the real-config fake
HOME, waits, and records smaps_rollup plus the biggest mappings of both.
Usage: driver.py <root> <template-home> <impl,...> <idle-seconds>
Env: RUST_DOCK, RUST_SHELL (binaries); EXTRA_ENV_<IMPL>="K=V K=V" optional;
WRAP_<IMPL>_<COMP>="valgrind --tool=massif ..." prefixes one program's argv."""
import json, os, shutil, signal, subprocess, sys, time
from pathlib import Path

ROOT, TEMPLATE = Path(sys.argv[1]), Path(sys.argv[2])
IMPLS = sys.argv[3].split(",")
IDLE = float(sys.argv[4])
if not os.environ.get("XDG_RUNTIME_DIR", "").startswith(str(ROOT)) or os.environ.get("WLR_BACKENDS") != "headless":
    sys.exit("refusing to run outside the private headless session")
if not os.environ.get("WAYLAND_DISPLAY"):
    sys.exit("refusing: unexpected WAYLAND_DISPLAY")

PY_DOCK = "/home/tcrider/.local/share/tessel-dock/tessel-dock.py"
PY_SHELL = "/home/tcrider/.local/share/tessel-shell/tessel-shell"
ARGV = {
    "python": {"tessel-shell": ["/usr/bin/python3", PY_SHELL], "tessel-dock": ["/usr/bin/python3", PY_DOCK]},
    "rust": {"tessel-shell": [os.environ["RUST_SHELL"]], "tessel-dock": [os.environ["RUST_DOCK"]]},
}


def rollup(pid):
    out = {}
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()[1:]:
        k, v = line.split(":", 1)
        out[k.strip()] = int(v.split()[0])
    return out


def top_mappings(pid, n=8):
    sizes, name = {}, None
    for line in Path(f"/proc/{pid}/smaps").read_text().splitlines():
        parts = line.split()
        if "-" in parts[0] and len(parts) >= 5 and not parts[0].endswith(":"):
            name = parts[5] if len(parts) > 5 else "[anon %s]" % parts[0]
        elif parts[0] == "Pss:":
            sizes[name] = sizes.get(name, 0) + int(parts[1])
    return sorted(((v, k) for k, v in sizes.items()), reverse=True)[:n]


FAKE_APP = """
import sys, gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gio, Gtk
app = Gtk.Application(application_id=sys.argv[1], flags=Gio.ApplicationFlags.NON_UNIQUE)
def activate(app):
    for n in range(int(sys.argv[2])):
        w = Gtk.ApplicationWindow(application=app, title="%s window %d" % (sys.argv[1], n))
        w.set_default_size(900, 600)
        w.set_child(Gtk.Label(label=w.get_title()))
        w.present()
app.connect("activate", activate)
app.run([])
"""
APP_IDS = ["org.mozilla.firefox", "org.gnome.Nautilus", "com.mitchellh.ghostty", "org.gnome.TextEditor",
           "org.gnome.Ptyxis", "org.gnome.Meld", "org.gnome.Calculator", "org.gnome.Settings"][:int(os.environ.get("ACTIVITY_APPS", "4"))]


def activity(impl, env, procs):
    """Open/close the app menu twice and map 8 windows of 4 apps, then idle."""
    dock = ARGV[impl]["tessel-dock"]
    apps = []
    try:
        for app_id in APP_IDS:
            apps.append(subprocess.Popen([sys.executable, "-c", FAKE_APP, app_id, os.environ.get("ACTIVITY_WINDOWS", "2")], env=env,
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        time.sleep(4)
        for _ in range(2):
            subprocess.run(dock + ["--menu"], env=env, capture_output=True, timeout=20)
            time.sleep(2.5)
            subprocess.run(dock + ["--menu"], env=env, capture_output=True, timeout=20)
            time.sleep(1.5)
        time.sleep(float(os.environ.get("ACTIVITY_IDLE", "10")))
        out = {}
        for comp, p in procs.items():
            if p.poll() is not None:
                out[comp] = {"error": "exited %s" % p.returncode}
            else:
                r = rollup(p.pid)
                out[comp] = {k: r.get(k) for k in ("Rss", "Pss", "Pss_Anon")}
                out[comp]["top"] = top_mappings(p.pid, 3)
                if os.environ.get("KEEP_SMAPS"):
                    shutil.copyfile(f"/proc/{p.pid}/smaps", ROOT / f"{impl}-{comp}-activity.smaps")
        return out
    finally:
        for a in apps:
            a.terminate()
        for a in apps:
            try:
                a.wait(timeout=5)
            except subprocess.TimeoutExpired:
                a.kill()
                a.wait()


def run(impl):
    home = ROOT / "homes" / impl
    shutil.rmtree(home, ignore_errors=True)
    shutil.copytree(TEMPLATE, home, symlinks=True)
    env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_CACHE_HOME=str(home / ".cache"), XDG_DATA_HOME=str(home / ".local/share"),
               XDG_STATE_HOME=str(home / ".local/state"), PYTHONDONTWRITEBYTECODE="1",
               XDG_DATA_DIRS=str(home / ".local/share/flatpak/exports/share") + ":/var/lib/flatpak/exports/share:/usr/local/share/:/usr/share/", XDG_CURRENT_DESKTOP="wlroots:labwc")
    env.pop("LABWC_PID", None)
    for item in os.environ.get("EXTRA_ENV_" + impl.upper(), "").split():
        k, v = item.split("=", 1)
        env[k] = v
    procs = {}
    procs_extra = []
    try:
        for comp in ("tessel-shell", "tessel-dock"):
            wrap = os.environ.get("WRAP_%s_%s" % (impl.upper(), comp.upper()), "").split()
            procs[comp] = subprocess.Popen(wrap + ARGV[impl][comp], env=env, start_new_session=True,
                                           stdout=open(ROOT / f"{impl}-{comp}.log", "w"), stderr=subprocess.STDOUT)
            time.sleep(2)
        tray = int(os.environ.get("TRAY_ITEMS", "0") or 0)
        if tray:
            procs_extra.append(subprocess.Popen([sys.executable, str(Path(__file__).with_name("fake_sni.py")),
                                                 str(home / "tray"), str(tray)], env=env,
                                                stdout=open(ROOT / f"{impl}-tray.log", "w"), stderr=subprocess.STDOUT))
        time.sleep(IDLE)
        result = {"impl": impl}
        for comp, p in procs.items():
            if p.poll() is not None:
                result[comp] = {"error": "exited %s" % p.returncode}
                continue
            r = rollup(p.pid)
            result[comp] = {k: r.get(k) for k in ("Rss", "Pss", "Pss_Anon", "Pss_File", "Anonymous")}
            result[comp]["top"] = top_mappings(p.pid)
            if os.environ.get("KEEP_SMAPS"):
                shutil.copyfile(f"/proc/{p.pid}/smaps", ROOT / f"{impl}-{comp}.smaps")
        if os.environ.get("ACTIVITY"):
            result["after_activity"] = activity(impl, env, procs)
        if os.environ.get("SHOT"):
            subprocess.run(["grim", "-o", "HEADLESS-1", str(ROOT / f"{impl}-shot1.png")], timeout=20)
        print(json.dumps(result), flush=True)
    finally:
        for p in procs_extra:
            p.terminate()
            p.wait(timeout=10)
        for p in procs.values():
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)
        for p in procs.values():
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
        time.sleep(1)


for impl in IMPLS:
    run(impl)
