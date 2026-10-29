"""Fake StatusNotifierItems for the real-config harness (private bus only).
Usage: fake_sni.py <dir> <count>. Every item gets its own IconThemePath
directory (like libayatana/Electron/Chromium items do) with one icon, and
registers with the dock's StatusNotifierWatcher."""
import os, sys
from pathlib import Path
from gi.repository import Gio, GLib

if not os.environ.get("XDG_RUNTIME_DIR", "").startswith("/run/user/1000/cbt/"):
    sys.exit("refusing to run outside the private harness")
ROOT, COUNT = Path(sys.argv[1]), int(sys.argv[2])
XML = """<node><interface name="org.kde.StatusNotifierItem">
<property name="Category" type="s" access="read"/><property name="Id" type="s" access="read"/>
<property name="Title" type="s" access="read"/><property name="Status" type="s" access="read"/>
<property name="IconName" type="s" access="read"/><property name="IconThemePath" type="s" access="read"/>
<property name="ItemIsMenu" type="b" access="read"/>
<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<signal name="NewIcon"/></interface></node>"""
SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22"><circle cx="11" cy="11" r="9" fill="#%06x"/></svg>'
info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
connections = []
for n in range(COUNT):
    path = ROOT / ("item%d" % n)
    path.mkdir(parents=True, exist_ok=True)
    (path / ("fake-tray-%d.svg" % n)).write_text(SVG % (0x204080 + n * 0x101010))
    props = {"Category": GLib.Variant("s", "ApplicationStatus"), "Id": GLib.Variant("s", "fake%d" % n),
             "Title": GLib.Variant("s", "Fake %d" % n), "Status": GLib.Variant("s", "Active"),
             "IconName": GLib.Variant("s", "fake-tray-%d" % n), "IconThemePath": GLib.Variant("s", str(path)),
             "ItemIsMenu": GLib.Variant("b", False)}
    # One connection per item, like separate applications.
    conn = Gio.DBusConnection.new_for_address_sync(
        Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION),
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION)
    conn.register_object("/StatusNotifierItem", info,
                         lambda c, s, p, i, m, a, inv: inv.return_value(None),
                         lambda c, s, p, i, prop, _props=props: _props[prop], None)
    conn.call_sync("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher",
                   "RegisterStatusNotifierItem", GLib.Variant("(s)", ("/StatusNotifierItem",)), None,
                   Gio.DBusCallFlags.NONE, 5000)
    connections.append(conn)
print("registered", COUNT, flush=True)
GLib.MainLoop().run()
