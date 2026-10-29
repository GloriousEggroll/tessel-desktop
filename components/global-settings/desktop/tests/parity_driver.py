#!/usr/bin/python3
"""Drive the Python Global Settings desktop pages for byte-parity checks.

Run ONLY by tests/desktop_headless.rs inside components/global-settings/tests/headless.sh
(private headless labwc, private bus, fake HOME/XDG). It refuses to run anywhere else.
Usage: parity_driver.py dock|theme|behavior|layout|lock|login|presets
"""
import ctypes
import json
import os
import sys
import time
from pathlib import Path
from types import SimpleNamespace

runtime = os.environ.get("XDG_RUNTIME_DIR", "")
if os.environ.get("GS_HEADLESS") != "1" or not runtime or runtime == "/run/user/1000" \
        or "/cbt/" not in runtime or not os.environ.get("WAYLAND_DISPLAY") \
        or "/run/user/1000/bus" in os.environ.get("DBUS_SESSION_BUS_ADDRESS", "/run/user/1000/bus") \
        or Path(os.environ["HOME"]).resolve() == Path("/home/tcrider"):
    raise SystemExit("parity_driver.py runs only inside the private headless session")

ROOT = Path(os.environ["BTMANAGER"])
sys.path.insert(0, str(ROOT / "global-settings"))
import runtime_paths  # noqa: E402
runtime_paths.initialize_paths()
for extra in ("tiling-manager", "waydock", "shared", "lock-screen", "login-manager", "power-manager", "weather-manager"):
    path = str(ROOT / extra)
    if path not in sys.path:
        sys.path.append(path)
ctypes.CDLL("libgtk4-layer-shell.so", mode=ctypes.RTLD_GLOBAL)
import gi  # noqa: E402
gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402
from suite_i18n import initialize  # noqa: E402
initialize()

SCENARIO = sys.argv[1]
HOME = Path(os.environ["HOME"])


def settle(seconds=0.1):
    end = time.monotonic() + seconds
    context = GLib.MainContext.default()
    while time.monotonic() < end:
        while context.pending():
            context.iteration(False)
        time.sleep(0.002)


def wait(predicate, seconds=5):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        settle(0.02)
        if predicate():
            return True
    raise SystemExit("timed out waiting")


app = Gtk.Application(application_id="org.waydock.GlobalSettings.ParityDriver", flags=Gio.ApplicationFlags.NON_UNIQUE)
app.register(None)
host = Gtk.ApplicationWindow(application=app)
host.set_default_size(1100, 800)


def pages_module():
    sys.modules.setdefault("window_backend", SimpleNamespace(WindowBackend=object))
    sys.modules.setdefault("desktop_actions", SimpleNamespace(DesktopActions=object))
    import dock_pages
    return dock_pages


def offline_bridge_class():
    dock_pages = pages_module()

    class OfflineBridge(dock_pages.DockSettingsBridge):
        def _running(self):
            return False
    return OfflineBridge


def tiling_backend():
    from tiling_config import TilingConfig
    return TilingConfig(HOME / ".config/labwc/rc.xml", HOME / ".local/state", HOME / ".config",
                        call=lambda _: {}, reload=lambda: None)


if SCENARIO == "dock":
    dock_pages = pages_module()
    page = dock_pages._create_page(app, "dock", bridge_factory=offline_bridge_class())
    controls = page.widget._settings_controller
    controls.transparency.set_value(44)
    controls.frosted.set_active(True)
    controls.auto_hide.set_active(True)
    controls.size.set_value(52)
    controls.mode.set_active_id("bar")
    controls.default_app_switches["network"].set_active(True)
    controls.layout_controls.move("clock", "end")
    controls.tray_choices.add("id:Nextcloud")
    controls.apply()
    page.close()

elif SCENARIO == "theme":
    dock_pages = pages_module()
    from desktop_pages import ThemeWallpaperPage
    from settings_api import APPEARANCE_FIELDS
    from dock_config import ConfigStore
    log = open(HOME / "requests.log", "w")
    monitors = [os.environ["MONITOR"]] + [m for m in [host.get_display().get_monitors().get_item(i).get_connector()
                for i in range(host.get_display().get_monitors().get_n_items())] if m != os.environ["MONITOR"]]

    def request(payload):
        log.write(json.dumps(payload) + "\n")
        log.flush()
        return {"ok": True, "monitors": monitors}
    bridge = offline_bridge_class()(app, fields=APPEARANCE_FIELDS, read=False)
    page = ThemeWallpaperPage(host, bridge=bridge, request=request, color_settings=None)
    host.set_child(page.widget)
    store = ConfigStore()
    page.match.set_active(True)
    wait(lambda: store.load().match_wallpaper)
    page.dock_transparency.set_value(61)
    page.window_shadows.set_active(False)
    page.deactivate()
    wait(lambda: not store.load().window_shadows)
    settle(0.3)
    row = page.rows[os.environ["MONITOR"]]
    row["mode"].set_selected(5)
    color = Gdk.RGBA()
    color.parse("#336699")
    row["color"].set_rgba(color)
    page.apply()
    wait(lambda: not page.busy)
    page.close()
    log.close()

elif SCENARIO == "behavior":
    dock_pages = pages_module()
    from workspace_config import WorkspaceConfig
    from workspace_settings import WorkspaceControls
    from tiling_settings import GaplessControls
    rc = HOME / ".config/labwc/rc.xml"
    page = dock_pages._create_page(
        app, "desktop", bridge_factory=offline_bridge_class(),
        workspace_factory=lambda: WorkspaceControls(WorkspaceConfig(rc, backup_dir=HOME / ".local/state/backups",
                                                                    reload=lambda: None)),
        tiling_factory=lambda: GaplessControls(tiling_backend()),
        authentication_factory=lambda: dock_pages.AuthenticationStatus(("true",)))
    controls = page.widget._settings_controller
    output = os.environ["MONITOR"]
    controls.menu_hotkey_fullscreen.set_active(True)
    controls.hot_corner_delay.set_value(300)
    controls.hot_corner_switches[output]["top_left"].set_active(True)
    controls.workspace_count.set_value(6)
    controls.apply()
    page.close()

elif SCENARIO == "layout":
    pages_module()
    import global_style
    from layout_page import LayoutSettings
    from layout_presets import PRESET_FIELDS
    from workspace_config import WorkspaceConfig
    from clock_preferences import ClockPreferencesStore
    global_style.CHOOSER = lambda _m, _d, _b, callback, _default, _cancel: callback(1)
    rc = HOME / ".config/labwc/rc.xml"
    bridge = offline_bridge_class()(None, fields=PRESET_FIELDS, read=False)
    page = LayoutSettings(None, bridge=bridge,
                          workspace_backend=WorkspaceConfig(rc, backup_dir=HOME / ".local/state/backups", reload=lambda: None),
                          tiling_backend=tiling_backend(), clock_store=ClockPreferencesStore())
    window = Gtk.Window(child=page.widget)
    window.present()
    wait(lambda: not page._refreshing)
    page.flow.select_child(next(card for card in page.cards if card.preset.key == "mac"))
    page.apply()
    wait(lambda: not page.busy and "Mac" in page.status.get_text())
    page.close()

elif SCENARIO == "lock":
    from lock_settings import LockSettings

    class FakePower:
        def __init__(self, changed):
            self.changed, self.ready, self.state, self.error = changed, False, {}, ""

        def start(self):
            self.changed()

        def close(self):
            pass
    settings = LockSettings(host, power_factory=FakePower, assistant_detector=lambda: True)
    settings.mode.set_selected(2)
    settings.slides.extend(["/tmp/b.png", "/tmp/a.jpg", os.environ["SLIDE"]])
    settings.sync_pictures()
    settings.interval.set_value(90)
    settings.follow.set_active(False)
    settings.transparency.set_value(33)
    settings.name.set_text("Ümlaut “quoted”")
    settings.show_forecast.set_active(True)
    settings.notification_names.set_active(True)
    settings.apply()
    assert settings.status.get_text() == "Lock screen settings saved.", settings.status.get_text()
    settings.close()

elif SCENARIO == "login":
    import login_page
    import login_model
    login_page.Appearance = type("Appearance", (login_model.Appearance,), {
        "load": classmethod(lambda cls, path=None: login_model.Appearance())})
    login_page.sessions = lambda: login_model.sessions((Path(os.environ["SESSIONS"]),))
    settings = login_page.LoginSettings(host)
    settings.picture.set_text("/usr/share/wallpapers/ä.png")
    settings.placement.set_selected(2)
    settings.color.set_text("#AbCdEf")
    settings.scales["transparency"].set_value(12.5)
    settings.idle.set_value(65)
    settings.session.set_selected(0)
    settings.save()
    wait(lambda: not settings.busy)
    print(settings.status.get_text())

elif SCENARIO == "presets":
    from dock_config import DockConfig
    from layout_presets import describe_changes, load_presets
    presets, problems = load_presets()
    assert not problems, problems
    for preset in presets:
        print(f"{preset.key}|{preset.name}|" + ";".join(describe_changes(preset, DockConfig(), 2, False)))

else:
    raise SystemExit(f"unknown scenario {SCENARIO}")
host.destroy()
settle(0.05)
