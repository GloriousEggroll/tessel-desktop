# Contract: bluetooth (btmanager)

Source: `~/btmanager/btmanager.py` (2985 LOC), `btpopup.py` (510), `bttransfer.py` (805),
`translations.py` (3514, own catalogs), docs `BLUETOOTH.md`. Refs are `file:line` in `~/btmanager/`.
Not a resident service: a GtkApplication started on demand from the dock (stays alive after first
launch while its hidden window exists; also owns the incoming-file OBEX agent while alive).

## 1. Entry points / install
| Item | Value |
|---|---|
| Installed to | `~/.config/waybar/scripts/btmanager/` = `btmanager.py btpopup.py bttransfer.py translations.py` + `shared/gtk4_utils.py`, `shared/popup_placement.py`, `waydock/companion_theme.py`, language helpers (`shared/install_gtk4.py:69-75`) |
| Launcher (dock) | `waydock/panel_actions.py:19,27-47` → `python3 ~/.config/waybar/scripts/btmanager/btmanager.py --popup --popup-edge E --popup-offset N`, both left- and right-click (bluetooth is in the "no --settings" list, :43), run through `systemd-run --user --quiet --collect --service-type=exec --slice=app.slice` so it outlives the dock |
| Embedded | Global Settings `global-settings/applet_pages.py:242-280` imports `btmanager` and builds `App(application=host_app, embedded=True, host_window=host, backend_factory=…)`; calls `_refresh_ui`, `_on_window_destroyed`, `_unwatch_object`, `_add_window`, `_sub_ids`, `bt.close()`. → Rust port must provide an equivalent embeddable page or Global Settings keeps the Python one. |
| Process-scan | `shared/install_gtk4.py:200-213` finds running instances by `argv[1] == …/btmanager/btmanager.py` (to ask the user to restart after install). A Rust binary changes argv → update that list. |
| systemd unit / .desktop / autostart | none |

### CLI (`launch_arguments` btmanager.py:2774-2783, `main` 2950-2981)
| Flag | Meaning |
|---|---|
| (none) | regular window 820×710 (min 560×440) (849-851) |
| `--popup` | compact layer-shell popup, width 400 (`POPUP_WIDTH` 75), natural height, device lists scroll past 420 px (76) |
| `--popup-edge top\|bottom\|left\|right` | default `bottom` |
| `--popup-offset N` | int ≥ 0, default 76; negative → argparse error exit 2 |
| `--lang CODE` | force UI language (only honoured on first activation, 2809-2810) |
| `--selftest` | headless: system bus, GetManagedObjects (or introspect fallback), adapters ≥1, all catalogs complete; prints `PASS/FAIL name — extra`, `selftest PASSED/FAILED`, exit 0/1 (2888-2947) |
| `--gen-pot [file]` / `--gen-mo [dir]` | write `messages.po` / `po/<lang>/messages.{po,mo}` (2954-2975) |
Args are validated in the launching process before registering (2978); invalid → no activation.

### Single instance (2786-2885)
- `Gtk.Application(application_id="org.btmanager.Bluetooth", flags=HANDLES_COMMAND_LINE)`; `GLib.set_prgname(app_id)` (74, 2789-2795). Session bus name `org.btmanager.Bluetooth` (standard GApplication `org.gtk.Application`/`org.freedesktop.Application` on `/org/btmanager/Bluetooth`).
- `do_local_command_line` returns `(False, argv, 0)` → full argv forwarded to primary (2800-2803).
- Primary `do_command_line`: re-parse; `--popup`/edge/offset update stored options; if popup exists → `PopupController.set_position(edge, offset)`; then `activate()` (2805-2822).
- `do_activate`: first time build `App`, `start_receiver(app.get_dbus_connection())`; popup mode → `popup.activate()` (toggle; see §4); window mode → `unminimize()+present()`, and on relaunch `wlrctl toplevel focus app_id:org.btmanager.Bluetooth title:<window title>` (Wayland only, if `wlrctl` on PATH), retried ≤4 times at 100 ms while it fails (2824-2885).

## 2. D-Bus
### Owned / exported
| Bus | Name / path | Interface |
|---|---|---|
| session | `org.btmanager.Bluetooth` | GApplication |
| session | object `/org/btmanager/obex/agent` (bttransfer.py:358) | `org.bluez.obex.Agent1`: `Release()`, `AuthorizePush(o transfer) → s filename`, `Cancel()` (361-370). Errors: `org.bluez.obex.Error.Rejected "Not Authorized"`, `org.freedesktop.DBus.Error.UnknownMethod`. Calls from anyone but current `org.bluez.obex` owner are rejected (599-602). |

### Consumed — BlueZ, **system bus** `org.bluez`
- `/org/bluez` `ObjectManager.GetManagedObjects` (timeout 8 s) + signals `InterfacesAdded/Removed` (270-285). Fallback when OM fails: `Introspectable.Introspect` on `/org/bluez` and each adapter + `Properties.GetAll` per object, re-polled every **2000 ms** async, never overlapping (350-463, 1719-1746).
- `Properties.PropertiesChanged` per watched object path (1599-1656). Interfaces: `org.bluez.Adapter1` (Powered, Discovering, Alias/Name, Address), `org.bluez.Device1` (Address, Adapter, Alias, Connected, Paired, Bonded, Trusted, Pairing, Icon, Class, UUIDs), `org.bluez.Battery1` (Percentage).
- Methods: `Properties.Set(Adapter1.Powered b)` / `Set(Device1.Trusted b)` (15 s); `Adapter1.StartDiscovery/StopDiscovery`; `Device1.Pair` (60 s) then `Connect` if not paired, else `Connect`; `Device1.Disconnect`; `Adapter1.RemoveDevice(o)` (20 s), on error fallback `Device1.Unpair` (1748-1988). Default call timeout 30 s.
- Pairing agent is **not** here: `authentication-manager/bluetooth_agent.py` owns the BlueZ Agent1.

### Consumed — session bus
- `org.bluez.obex` `/org/bluez/obex` `Client1.CreateSession(s dest, a{sv}{Target:"opp", Source:<adapter addr>}) → o`, `ObjectPush1.SendFile(s path) → (o, a{sv})`, `Client1.RemoveSession(o)`; signals `PropertiesChanged` (arg0 `org.bluez.obex.Transfer1`), `ObjectManager.InterfacesRemoved`, `NameOwnerChanged(arg0 org.bluez.obex)` (bttransfer.py:167-353). Sender uses a **private** session-bus connection (`Gio.DBusConnection.new_for_address`) closed after cleanup (93-150, 337-353).
- `org.bluez.obex` `AgentManager1.RegisterAgent(o)/UnregisterAgent(o)`; `GetNameOwner(org.bluez.obex)`; re-registers after obexd restart (540-577, 677-698); reads `Transfer1` (Name/Filename, Size, Type, Session, Status, Transferred) and `Session1.Destination`.
- `org.freedesktop.Notifications` (`Notifier`, 713-805): `GetCapabilities` (actions?), `Notify(susssasa{sv}i)` with hints `urgency y`, `desktop-entry s "btmanager"`, optional `value i` (progress %), `CloseNotification(u)`, signals `ActionInvoked`, `NotificationClosed`. App name = translated "Bluetooth".
- `org.freedesktop.FileManager1.ShowItems(as uris, s "")` for "Show in folder", fallback `launch_default_for_uri(dir)` (btmanager.py:1376-1397).
- `org.waydock.Waydock` `/org/waydock/Settings` `org.waydock.Settings.GetPopupPlacement(s connector, s group="service_apps") → s JSON {edge, offset 0..4096, align start|center|end}` (NO_AUTO_START, 500 ms) via `shared/popup_placement.py:69-90`; invalid/missing → use CLI edge/offset.

## 3. Files
| Path | Format | R/W |
|---|---|---|
| `${XDG_CONFIG_HOME:-~/.config}/btmanager/settings.ini` (78-81) | INI `[receive]\nmode = off\|ask` (default `ask`; invalid → ask). Written atomically via `.tmp` + replace, preserving other sections (86-114). | primary app & Global Settings embedded page write; primary **monitors file** (`Gio.FileMonitor WATCH_MOVES`) and starts/stops the OBEX agent on change (1167-1214) |
| `~/.config/desktop-suite/language.json` | shared language (`suite_i18n.selected_language`) | read only; old per-app language override ignored (1554-1556) |
| Downloads (`G_USER_DIRECTORY_DOWNLOAD`, fallback `~/Downloads`) (117-119) | received files, created `O_EXCL` mode 0600 before answering AuthorizePush; partial file deleted on failure (bttransfer.py:417-433, 700-710) |
| temp `btmanager-note-*/note.vnt` | note: `BEGIN:VNOTE\r\nVERSION:1.1\r\nBODY;CHARSET=UTF-8:<lines joined by space>\r\nDCREATED:%Y%m%dT%H%M%S\r\nLAST-MODIFIED:…\r\nCLASS:PUBLIC\r\nX-IRMC-LUID:000001000000\r\nEND:VNOTE\r\n`, mode 0600, removed after transfer (btmanager.py:2566-2590) |

i18n: **own catalogs** in `translations.py` (`MESSIDS`, `catalog_for`, 31 langs) compiled to gettext in-memory (123-200), *not* `shared/locale/*.json`. Port: convert to shared JSON or embed.

## 4. UI surfaces
- Popup (`btpopup.PopupController`, btpopup.py:15-270): layer-shell namespace `desktop-applet`, layer OVERLAY, exclusive zone -1, keyboard ON_DEMAND; anchor panel edge with margin=offset plus side anchor (`align` end→RIGHT/BOTTOM, start→LEFT/TOP) margin 12 (100-113). One transparent full-screen **click-catcher per monitor**: namespace `desktop-applet-dismiss`, OVERLAY, all 4 anchors, keyboard NONE, Cairo-painted transparent DrawingArea (an empty Box gets no buffer), click released (any button, capture) → dismiss (64-98). Output selection: `PointerMonitor` 1×1 input-transparent OVERLAY surface namespace `suite-output-probe`, waits for `wl_surface.enter` (timeout 1 s) then `GetPopupPlacement` (132-148; popup_placement.py:10-66).
- Popup behaviour: `activate()` = if a visible owned child window → dismiss popup and raise child; elif visible → dismiss; else show (158-170). Escape / close-request / outside click / monitor list change → dismiss (hide, not quit). Focus loss does **not** dismiss under layer-shell (214-222); without layer-shell dismiss after 150 ms if no toplevel active. Backdrops destroyed in idle when unrealized from another surface's handler (239-263). `show_pending` generation guards async show.
- Child windows (Add device 480×460, Info 600×480, Send File, Send Note 520×360, transfer 520×260, receive prompt 420) suspend the popup; in popup mode they are not transient (1408-1420).
- CSS: window class `btmanager` (+`tray-popup` in popup); classes `page-title page-subtitle power-card power-title secondary-label bluetooth-tile(.off) section-title trust-column count-badge connected-count device-list device-icon(.connected) device-name device-address connection-status empty-label empty-icon off-title off-symbol connect-action icon-button destructive-action suggested-action event-bar(.error) popup-surface popup-icon-tile(.off) device-actions add-device-action`, `desktop-applet-backdrop` (STYLE_CSS 639-790; btpopup.py:74, 289-325). `companion_theme.attach(window)` from waydock applies suite palette.
- Lists: Connected / Known (paired∨bonded∨trusted) / Nearby (add window only); only devices whose owning adapter (`Adapter` prop, else parent path) is the selected adapter; sorted by casefolded alias (2098-2130).
- Icons: battery `battery-level-N0-symbolic` → legacy full/good/low/caution → `battery-symbolic`; device `Icon` prop (+`-symbolic`), keyboard → `preferences-desktop-keyboard-symbolic`, major class 1/2/4/5 fallbacks, then `bluetooth-symbolic` (2706-2739).

## 5. External commands
`wlrctl toplevel focus app_id:… title:…` (optional). Nothing else (URIs via GIO default handler).

## 6. Subtle behaviours → parity tests
Backend (test_backend.py): device keeps adapter path (116); duplicate MAC on two adapters independent (124, 147); manual adapter selection survives polls/OM adds, even unpowered (156, 192); unselected adapter removal keeps selection, selected removal → first powered else first sorted (167-240; `_pick_default_adapter` 1676-1683); OM add/remove use payload path (303, 319); unrelated interface removal keeps device (332); Battery1 merged & survives Device1 refresh (344, 356); other-interface PropertiesChanged ignored (382); watched objects not re-fetched by polls (394); polls never overlap, stop after close (404); OM failure falls back without blocking (419); **failed introspection/GetAll never removes objects; only successful empty listing does** (263-283); error advice table `ERROR_ADVICE` first-match on lowercase name+message (443; btmanager.py:585-631) with "Details: …" tooltip; receive-mode INI round-trip keeps other sections (470).
Transfers (test_transfer.py): explicit Source adapter (117); sequential progress dict {filename,index,count,transferred,size,completed,total,status}, cleanup once (127); completion before SendFile reply not lost (146); stale file signals ignored (164); cancel never cancels in-flight CreateSession — removes late session (191); cancel idempotent (210); remote failure aborts rest (221); missing service → setup hint `is_unavailable_error` (231, 324); cleanup failure ≠ success (261); transfer vanishing = error (270, 277); obexd shutdown fails & releases watchers (286); invalidated Status re-read via GetAll (295); unreadable/empty/missing source never opens session (305-316); private connection closed before payload released (337-369); cancel from progress callback (385, 393). Receiver: register/unregister (473); reject foreign sender (481); unique sanitized path `name (N).ext`, leading `.`→`_`, ≤200 chars, `file` fallback (488, 535; bttransfer.py:373-393); failed transfer deletes partial (505); decline & obex Cancel (513); obexd restart fails active + re-registers (524).
Receive UX (btmanager.py:1249-1365): notification with Accept/Decline (urgency 2, timeout 0) if server supports actions, else Gtk dialog; closing notification = decline; progress notification throttled to ≥500 ms or % change with `value` hint; "File received" with Open/Show in folder. Agent only in primary; only when mode=ask (test_popup.py:313, test_ui.py:1368-1428).
UI (test_ui.py): adapter warnings/controls on add/remove (408-469); actions target selected adapter only (568); forget uses device's owner adapter after selection change (596); stale power callback can't affect new selection (608); **scan session state machine** `_drive_scan_session` 1805-1873 — one own discovery session, release old before starting on new adapter, failed old start doesn't cancel new request, failed stop never starts second session, stop on power-off, external Discovering doesn't count as ours (667-770, 1081-1137); send dialogs revalidate owner power/busy (928, 950); Object Push availability: no UUIDs yet ⇒ allowed, else needs `00001105-…` (2325-2328); notes private UTF-8 vnote cleaned on every outcome (972); trust switch reverts on error (1308; 1938-1951); battery level shown (1337); popup width 400 natural height (1429).
Popup/instance (test_popup.py:178, 326; test_popup_routing.py:32-65; test_single_instance.py:357): repeated launches reuse controller & language, update placement (clear old anchors), invalid placement doesn't activate, owned dialogs kept, relaunch refocuses window, closing allows fresh launch.
Never run `test_actions.py` (drives real radio).

## 7. Wayland
Only layer-shell via `gtk4-layer-shell` (works with gtk4-rs crate `gtk4-layer-shell`). `wl_surface.enter` output probing is achievable via GDK (`Gdk.Surface::enter-monitor`), no custom bindings. Foreign-toplevel activation is delegated to `wlrctl`; a Rust port could keep that or use `zwlr_foreign_toplevel_manager_v1` (custom wayland-client binding) — keep `wlrctl` for parity.
