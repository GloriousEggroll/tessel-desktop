"""Drive one Global Settings applet page (Python, ~/tessel-bluetooth, read-only) with
the same actions as the Rust driver, so the files both write can be compared.

Runs ONLY inside components/settings/tests/headless.sh (private
headless labwc, private bus, fake HOME/XDG dirs); refuses otherwise.

    python3 -I parity_driver.py <scenario>
"""
import os
import sys
import time

if os.environ.get("GS_HEADLESS") != "1" or not os.environ.get("XDG_RUNTIME_DIR", "").startswith("/run/user/1000/cbt/"):
    raise SystemExit("parity_driver.py runs only inside the private headless session")
if not os.environ.get("HOME", "").startswith("/run/user/1000/cbt/"):
    raise SystemExit("parity_driver.py needs a fake HOME")

B = os.environ["BTMANAGER"]
sys.path[:0] = [B, os.path.join(B, "shared"), os.path.join(B, "tessel-settings"),
                os.path.join(B, "tessel-calendar"), os.path.join(B, "tessel-network"),
                os.path.join(B, "tessel-capture"), os.path.join(B, "tessel-weather")]

import gi  # noqa: E402
gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402


def settle(seconds=0.2):
    deadline = time.monotonic() + seconds
    context = GLib.MainContext.default()
    while time.monotonic() < deadline:
        while context.pending():
            context.iteration(False)
        time.sleep(0.002)


def descendants(widget):
    child = widget.get_first_child()
    while child is not None:
        yield child
        yield from descendants(child)
        child = child.get_next_sibling()


def main(scenario):
    app = Gtk.Application(application_id=None)
    app.register(None)
    host = Gtk.ApplicationWindow(application=app, title="Global Settings parity")
    host.show_section = lambda section: None
    page = None
    if scenario == "clock":
        from applet_pages import build_page
        from fake_date_backend import FakeDateBackend
        page = build_page("datetime", host, factories={"datetime": FakeDateBackend})
        host.set_child(page.widget)
        host.present()
        page.activate()
        settle()
        c = page.controller
        c.clock_stack_switch.set_active(False)
        c.clock_reverse_switch.set_active(True)
        c.clock_hour_combo.set_selected(1)
        c.clock_date_combo.set_selected(3)
        c.clock_show_year_switch.set_active(False)
        c.clock_show_weekday_switch.set_active(True)
        c.clock_show_date_switch.set_active(False)
        c.week_start_combo.set_selected(1)
    elif scenario == "capture":
        from capture_page import build_page
        page = build_page("capture", host)
        host.set_child(page.widget)
        host.present()
        settle()
        page.search_targets["copy_to_clipboard"].set_active(False)
        page.search_targets["open_editor"].set_active(True)
    elif scenario == "weather":
        from weather_page import build_page
        page = build_page("weather", host)
        host.set_child(page.widget)
        host.present()
        page.activate()
        settle()
        c = page.controller
        c.location_name.set_text("Springfield")
        c.latitude.set_value(39.7817)
        c.longitude.set_value(-89.6501)
        c.timezone.set_text("America/Chicago")
        c.manual_apply.emit("clicked")
        settle()
        c.units.set_selected(1)
        c.week.set_selected(1)
        c.refresh_minutes.set_value(45)
    elif scenario == "bluetooth":
        from applet_pages import build_page
        from test_ui import FakeBT

        class Bluetooth(FakeBT):
            def __init__(self):
                super().__init__("multi-adapter")

            def close(self):
                pass

        page = build_page("bluetooth", host, factories={"bluetooth": Bluetooth})
        host.set_child(page.widget)
        host.present()
        page.activate()
        settle()
        page.controller._receive_combo.set_selected(0)
    elif scenario == "network":
        from applet_pages import build_page
        from fake_network import FakeNetworkBackend
        page = build_page("network", host, factories={"network": FakeNetworkBackend})
        host.set_child(page.widget)
        host.present()
        page.activate()
        settle()
        page.application.show_preferences()
        settle()
        dialog = page.application._children[-1]
        checks = [w for w in descendants(dialog) if isinstance(w, Gtk.CheckButton)]
        checks[0].set_active(False)
        checks[3].set_active(False)
    else:
        raise SystemExit("unknown scenario " + scenario)
    settle(0.3)
    page.close()
    host.destroy()
    settle(0.1)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1]))
