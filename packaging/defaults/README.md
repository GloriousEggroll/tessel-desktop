# Packaged defaults

System-wide defaults the desktop suite package ships. They apply to new users and to
anyone who never changed the setting; per-user choices made in Global Settings
(gsettings, `~/.config/gtk-*/settings.ini`, `~/.config/mimeapps.list`) win.

Paths below mirror the install root.

| Default | File | Notes |
|---|---|---|
| Icon theme Papirus-Dark | `usr/share/glib-2.0/schemas/90_desktop-suite.gschema.override` | `org.gnome.desktop.interface icon-theme`; run `glib-compile-schemas` in `%posttrans`. Requires `papirus-icon-theme`. |
| Icon theme Papirus-Dark (GTK fallback) | `etc/gtk-3.0/settings.ini`, `etc/gtk-4.0/settings.ini` | Read by GTK when no settings portal/gsettings value applies. |
| Login repair of a missing icon theme | `etc/xdg/autostart/desktop-suite-icon-theme.desktop` | Runs `desktop-suite-icon-theme` (session-startup crate; install it on `PATH`, e.g. `/usr/bin`) through `labwc-xdg-autostart.target`. |

Session startup also repairs a missing icon theme at login (falls back to Papirus-Dark when the
configured theme is not installed), so removing a theme package never leaves the desktop without icons.

When adding a new suite-wide default, add its file here and a row to this table.
