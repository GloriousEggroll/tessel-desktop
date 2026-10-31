# Packaged defaults

System-wide defaults the desktop suite package ships. They apply to new users and to
anyone who never changed the setting; per-user choices made in Global Settings
(gsettings, `~/.config/gtk-*/settings.ini`, `~/.config/mimeapps.list`) win.

Paths below mirror the install root.

| Default | File | Notes |
|---|---|---|
| Icon theme Papirus-Dark | `usr/share/glib-2.0/schemas/90_desktop-suite.gschema.override` | `org.gnome.desktop.interface icon-theme`; run `glib-compile-schemas` in `%posttrans`. Requires `papirus-icon-theme`. |
| Archive Manager opens archives | `usr/share/applications/labwc-mimeapps.list` | `[Default Applications]` for every archive mime type of `org.waydock.ArchiveManager.desktop` (its `MimeType=` line is the source; keep both in sync). Read for `XDG_CURRENT_DESKTOP=wlroots:labwc` sessions; requires the archive-manager package (`/usr/local/share/applications/org.waydock.ArchiveManager.desktop` or the packaged path). Per-user equivalent: `tools/install.py --apply --only archive-manager-mime`. |
| Archive Manager settings | none shipped (built in) | Defaults live in code (`components/archive-manager/settings/src/lib.rs`, `Settings::default`): info panel on, preserve paths on, extract into subfolder on, open destination off, close after extraction off, all plugins enabled, internal preview on, preview limit 50 MB. `~/.config/desktop-suite/archive-manager.json` is written only when the user changes a setting (Archive Manager or Global Settings > Archive Manager). |
| Icon theme Papirus-Dark (GTK fallback) | `etc/gtk-3.0/settings.ini`, `etc/gtk-4.0/settings.ini` | Read by GTK when no settings portal/gsettings value applies. |
| Login repair of a missing icon theme | `etc/xdg/autostart/desktop-suite-icon-theme.desktop` | Runs `desktop-suite-icon-theme` (session-startup crate; install it on `PATH`, e.g. `/usr/bin`) through `labwc-xdg-autostart.target`. |
| Clipboard kept after apps close | `usr/lib/systemd/user-preset/90-desktop-suite.preset` | `enable clipboard-manager.service` (WantedBy=graphical-session.target). Package `components/clipboard-manager/clipboard-manager.service` into `usr/lib/systemd/user/` with `ExecStart=` pointing at the packaged `wayclip`, and run `systemctl --global preset clipboard-manager.service` in `%post` (or use `%systemd_user_post`). Per-user equivalent: `tools/install.py --apply --only clipboard-manager` then `systemctl --user enable --now clipboard-manager.service`. |
| Clipboard settings | none shipped (built in) | Defaults live in code (`components/clipboard-manager/settings/src/lib.rs`, `Settings::default`): keep the clipboard on, keep the middle-click selection off, skip password-manager secrets on, largest kept copy 64 MB. `~/.config/desktop-suite/clipboard.json` is written only when the user changes a setting (Global Settings > Clipboard). |

Session startup also repairs a missing icon theme at login (falls back to Papirus-Dark when the
configured theme is not installed), so removing a theme package never leaves the desktop without icons.

When adding a new suite-wide default, add its file here and a row to this table.
