"""Python reference for the overview-open measurement in tests/switcher_ui.rs
(real_window_capture_and_overview_cleanup): time from SwitcherView.show() in
Activities mode to the window being mapped and to the first window preview.
Run only inside the private headless labwc (run_headless_switcher.sh EXTRA=…)."""
import os, sys, time
src = "/home/tcrider/tessel-bluetooth/tessel-shell"
sys.path[:0] = [src, src + "/../shared", src + "/../tessel-dock"]
from tessel-shell_switcher_ui import SwitcherView, _gtk_modules
Gdk, Gio, GLib, Gtk, Layer, Pango = _gtk_modules()
Gtk.init()
def settle(s):
    end = time.monotonic() + s
    ctx = GLib.MainContext.default()
    while time.monotonic() < end:
        while ctx.pending(): ctx.iteration(False)
        time.sleep(.002)
app = Gtk.Application(application_id="org.tessel.PyOverview", flags=Gio.ApplicationFlags.NON_UNIQUE)
app.register(None)
view = SwitcherView(app, lambda i: None, lambda: None)
target = Gtk.Window(application=app, title="Unique overview native capture target")
target.set_default_size(640, 400); target.set_child(Gtk.Label(label="A real native window preview")); target.present()
settle(.3)
mon = Gdk.Display.get_default().get_monitors().get_item(0)
view.set_mode("activities")
view.render([dict(id="native", name="Test", title=target.get_title(), app_id="org.tessel.PyOverview")], "native")
t0 = time.monotonic(); mapped = preview = None
view.show(mon)
while time.monotonic() - t0 < 2.5:
    settle(.005)
    if mapped is None and view.window.get_mapped(): mapped = time.monotonic() - t0
    if preview is None and view.cards["native"]["preview"].get_paintable() is not None: preview = time.monotonic() - t0
    if mapped and preview: break
print("python overview_open_ms mapped=%s first_preview=%s" % (mapped and round(mapped * 1000), preview and round(preview * 1000)))
