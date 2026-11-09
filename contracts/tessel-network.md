# Contract: tessel-network (netman)

Source: `~/tessel-bluetooth/tessel-network/` (10,152 LOC incl. tests; runtime ≈ 6.9k). All refs are
`file:line` relative to that directory unless prefixed. GTK4 + libnm (GI `NM 1.0`) + libsecret
(GI `Secret 1`) + gtk4-layer-shell. The Python backend talks to NetworkManager only through libnm;
a Rust port will most likely talk to NM's D-Bus API directly (zbus). The libnm calls are named below
together with the D-Bus calls they become.

## 1. Entry points and processes

| Item | Value | Ref |
|---|---|---|
| Main executable | `/usr/bin/python3 ~/.config/waybar/scripts/tessel-network/network_manager.py` | install_waybar.py:22,103 |
| Status streamer | `/usr/bin/python3 -u …/tessel-network/network_status.py` (no args), started by tessel-dock | network_status.py; tessel-dock/status_backend.py:29,272 |
| GApplication id | `org.tessel.NetworkManager`, flags `HANDLES_COMMAND_LINE`; `GLib.set_prgname(id)`, so the Wayland app_id is also the id | network_manager.py:32,60-61 |
| systemd user unit | `~/.config/systemd/user/tessel-network.service`, **static** (no `[Install]`), `Type=simple`, `Restart=on-failure`, `After=graphical-session.target`, `Description=Waybar Network Manager and authentication agent`, `ExecStart=/usr/bin/python3 %h/.config/waybar/scripts/tessel-network/network_manager.py --background` | live unit; install_waybar.py:73-91 |
| Started by | `~/.config/labwc/autostart:46` → `systemctl --user start tessel-network.service >/dev/null 2>&1 &` | |
| .desktop | `~/.local/share/applications/org.tessel.NetworkManager.desktop`: `Name=Network Settings`, `Comment=Manage network, Wi-Fi, and VPN connections`, `Exec=/usr/bin/python3 <dest>/network_manager.py --settings`, `Icon=network-workgroup`, `Terminal=false`, `Categories=Settings;Network;` | install_waybar.py:24,144-147 |
| Keybinds | none (`tessel-action` and rc.xml have no network actions) | |
| Install step | `install_all.py:81` key `network`, kind `waybar`, `update=False`, destination `.config/waybar/scripts/tessel-network` | |
| Health check used by updaters | `network_manager.py --check` with predicate `data["ready"]` | shared/install_gtk4.py:333-342; install_waybar.py:216 |

### CLI (`arguments()`, network_manager.py:43-55)
Mutually exclusive: `--popup` | `--settings` | `--background` | `--check`; plus
`--popup-edge {top,bottom,left,right}` (default `bottom`) and `--popup-offset INT` (default 76,
`<0` is a parser error). Parsing runs in-process first (`main()` :454) so bad args exit 2 before D-Bus.

| Invocation | Behaviour (primary instance) | Ref |
|---|---|---|
| no mode / `--popup` | `show_popup(edge, offset)`; same (edge,offset) → `activate()` existing popup; different → destroy and rebuild | :137-155 |
| `--settings` | dismiss popup, create/reuse the single settings window, present + raise | :135,157-165 |
| `--background` | nothing (the primary keeps backend + agent alive via `hold()`) | :99,137 |
| `--check` | prints one JSON line `{"ready":bool,"network_ready":bool,"agent_ready":bool,"devices":int,"profiles":int}`; exit 0 if backend.ready && agent.ready, else **2** | :128-134 |
| `--check` with no primary | `do_local_command_line` sets `IS_LAUNCHER`, so a probe **never becomes primary** or registers an agent; exits non-zero, no backend constructed | :116-121; test_network.py:733-752 |
| `do_activate` (plain D-Bus Activate) | `show_popup()` with defaults | :113-114 |

Secondary launches forward argv to the primary over GApplication D-Bus (`org.gtk.Application.CommandLine`)
and exit with the primary's return code. Tessel Dock launches through `systemd-run --user --quiet --collect
--service-type=exec --slice=app.slice …` (tessel-dock/panel_actions.py:73-80):
left click `… --popup --popup-edge E --popup-offset N`, right click `… --settings` (panel_actions.py:28-47).
In the legacy Waybar module the clicks are the same strings with ` </dev/null >/dev/null 2>&1 &` appended
(install_waybar.py:103-111).

Signals: SIGTERM/SIGINT → `quit()` (network_manager.py:110-111,433). Shutdown order: agent.close →
backend.close → destroy popup → destroy children → release hold (:437-450).

## 2. IPC

### 2.1 Owned (session bus)
| Name/Path | Interface | Notes |
|---|---|---|
| `org.tessel.NetworkManager` @ `/org/tessel/NetworkManager` | `org.gtk.Application`, `org.freedesktop.Application`, `org.gtk.Actions` | Standard GApplication. Exported action: `app.captive-portal` (no parameter) → `open_captive_portal()` (:107-109). Keep it: notifications reference it. |

### 2.2 NetworkManager secret agent (system bus) — secret_agent.py
* Registered through `NM.SecretAgentOld(identifier="org.tessel.NetworkManager", auto_register=True,
  capabilities=VPN_HINTS)` (:403-408). On the wire: calls
  `org.freedesktop.NetworkManager.AgentManager.RegisterWithCapabilities(s identifier="org.tessel.NetworkManager", u caps=0x1 VPN_HINTS)`
  at `/org/freedesktop/NetworkManager/AgentManager`; `Unregister()` on close (test_secret_agent_bus.py asserts
  exactly `['RegisterWithCapabilities']` then `['RegisterWithCapabilities','Unregister']`). libnm also re-registers when NM restarts. **The Rust port must re-register on NameOwnerChanged.**
* Exports `org.freedesktop.NetworkManager.SecretAgent` at `/org/freedesktop/NetworkManager/SecretAgent`:
  * `GetSecrets(a{sa{sv}} connection, o connection_path, s setting_name, as hints, u flags) → a{sa{sv}}`
  * `CancelGetSecrets(o connection_path, s setting_name)`
  * `SaveSecrets(a{sa{sv}} connection, o connection_path)`
  * `DeleteSecrets(a{sa{sv}} connection, o connection_path)`
  * Errors are `org.freedesktop.NetworkManager.SecretAgent.{Failed,UserCanceled,AgentCanceled,InvalidConnection,NoSecrets}` (NM.SecretAgentError via `agent_error()` :43).
  * Flags used: `ALLOW_INTERACTION=0x1`, `REQUEST_NEW=0x2` (:39-40). Secret flags: `AGENT_OWNED=0x1`, `NOT_SAVED=0x2`.
* `ready` = agent `registered` property (:460-463); this is what `--check` reports as `agent_ready`.

**GetSecrets algorithm** (`_get` :470-509):
1. Closed, missing uuid or missing setting → `InvalidConnection`.
2. Clone the connection; look up the keyring (§2.3) with UNLOCK only if ALLOW_INTERACTION.
3. Merge found values. For `vpn` → always queue the VPN helper (step 6).
4. Otherwise `missing` = no known values, or a hint not known, or `need_secrets()` names this setting.
   `always_ask` = any known key has `NOT_SAVED`. If !REQUEST_NEW && !missing && !always_ask → return known values without a prompt.
5. Else, if ALLOW_INTERACTION → queue a prompt; else → `NoSecrets`.
6. One prompt at a time (FIFO `deque`, `_active` token); the next starts on idle after `_finish` (:511-532,724-728).
* Prompt fields (`_connection_fields` :570-596): hints, or else `need_secrets()` hints, or else the defaults
  `802-1x/pppoe/gsm/cdma/adsl→password`, `wireguard→private-key`, `802-11-wireless-security` →
  `wep-key<tx_keyidx>` (key-mgmt none) / `leap-password` (ieee8021x) / `psk`. Only properties flagged
  SECRET are kept. A WireGuard `peers.<pubkey>.preshared-key` gets the caption "Peer preshared key".
  REQUEST_NEW clears the old secrets before the prompt.
* Prompt dialog (`_fields_dialog` :599-623): `Gtk.Dialog` 420px, not resizable, buttons Cancel/Connect (default),
  title "Network authentication", message "Enter credentials for {connection}.", masked entries (`InputPurpose.PASSWORD`),
  check "Remember credentials in the keyring" (initially on unless any field is NOT_SAVED). On OK, every
  entered key gets flag AGENT_OWNED (remember) or NOT_SAVED (ask every time), then `verify_secrets()`; on
  failure → `Failed`, no save. Entries are blanked afterwards (:663-665).
* Return format (`variant_secrets` :95-111): `{setting: {key: s}}`; `password-raw` is `ay`; VPN →
  `{"vpn": {"secrets": a{ss}}}`; WireGuard peers → `"peers": aa{sv}` with `public-key`, `preshared-key`.
* On accept with ALLOW_INTERACTION, only the requested setting is saved to the keyring (old items for that setting are cleared first) (:699-705; keyring `save(setting_names=…)` :201-224).
* `CancelGetSecrets(path, setting)` finishes all matching requests with `AgentCanceled` and destroys their dialog/helper (:730-734).
* `SaveSecrets` → keyring delete-all-for-uuid, then store every AGENT_OWNED && !NOT_SAVED non-empty secret. `DeleteSecrets` → clear by uuid. Both reply `Failed` with a message on keyring error (:736-750).
* close(): finish everything with `AgentCanceled` ("Network authentication is closing."), then unregister (:752-767).

**VPN auth helper** (`vpn_auth_command` :253-277, `VpnProcess` :327-385):
* The executable comes from the plugin `.name` file (`NM.VpnPluginInfo.new_search_file(None, service)` →
  `[GNOME] auth-dialog`, in `/usr/lib/NetworkManager/VPN/*.name` and `/etc/NetworkManager/VPN`). **Trust check:** absolute path,
  regular file, uid 0, not group/other-writable, executable; otherwise refused.
* argv `[path, -u uuid, -n id, -s service, (-i if ALLOW_INTERACTION), (-r if REQUEST_NEW), (-t hint)* if plugin supports-hints, (--external-ui-mode if [GNOME] supports-external-ui-mode ∈ true/yes/1)]`. **No secrets in argv.**
* stdin payload: `DATA_KEY=k\nDATA_VAL=v\n…SECRET_KEY=k\nSECRET_VAL=v\n…DONE\n\nQUIT\n\n` (newlines in values become spaces) (:238-250).
* Runs in a new session (`start_new_session`), `G_MESSAGES_DEBUG` removed from env, stderr→/dev/null, 300 s timeout → `killpg(SIGKILL)`; cancel → killpg; non-zero exit → "cancelled" error.
* Response (`parse_vpn_response` :280-324), max 1 MiB: legacy = pairs of `key\nvalue\n` lines until an empty line.
  External-UI = GKeyFile `[VPN Plugin UI] Version=2, Title, Description` + groups with `Label`, `Value`,
  `ShouldAsk`, `IsSecret`. ShouldAsk fields → our fields dialog (no remember checkbox); otherwise reply directly.
* Before a non-external helper maps its own window, `on_prompt(None)` dismisses our popup (:669-670).

### 2.3 Secret Service (libsecret, session bus `org.freedesktop.secrets`)
Schema `org.freedesktop.NetworkManager.Connection`, flag DONT_MATCH_NAME, string attributes
`connection-uuid`, `setting-name`, `setting-key` (:33-36). This is the nm-applet/libnma schema, which keeps old saved passwords working.
Item label: `Network password for {connection id}`; collection DEFAULT (:196-199). Lookup: `password_search`
with `ALL|LOAD_SECRETS` (+`UNLOCK` if interactive), then `retrieve_secret` per item (Retrievable interface, portal-safe) (:129-165).

### 2.4 NetworkManager client (system bus) — network_backend.py
Read-only on start (`NM.Client.new_async`, :229-252); a failure retries every **3000 ms** (`_later("reconnect")`).
Change notifications are coalesced into one refresh after **60 ms** (:297-298). Watched signals: client
device/connection/active add/remove + notify; Device `state-changed`; DeviceWifi AP add/remove;
Connection `changed`; ActiveConnection `state-changed`; VpnConnection `vpn-state-changed`; IP configs and APs notify (:319-361).
`get_nm_running()==false` → all models emptied, `error="NetworkManager is not running."`, generation bumped (:367-379).

Mutations (all async, keyed in `busy` set; a duplicate key → "That network request is already in progress."):

| Method | busy key | D-Bus effect | Ref |
|---|---|---|---|
| set_networking(b) | `radio:networking` | `org.freedesktop.NetworkManager.Enable(b)` on `/org/freedesktop/NetworkManager`, flag ALLOW_INTERACTIVE_AUTHORIZATION, timeout 2^31-1 ms | :558-568 |
| set_wireless(b) | `radio:wifi` | Set property `WirelessEnabled` (b); refuses enable if hardware-blocked | :570-581 |
| set_wwan(b) | `radio:wwan` | Set property `WwanEnabled` | :583-594 |
| set_device_enabled(id,b) | `device:<path>` | `Device.SetManaged(managed YES/NO, flags RUNTIME\|SET_ADMIN_STATE)` (libnm `set_managed_async`, NM ≥1.48). **No fallback** to the legacy `Managed` property; missing capability → error. Only wired/wifi. | :596-626 |
| scan(id?) | `scan:<path>` | `Device.Wireless.RequestScan({})`; stays busy until `LastScan` advances, or 12 s | :656-687, :470-473 |
| activate_profile(uuid, dev?) | `activate:<uuid>` | `ActivateConnection(o conn, o device or "/", o "/")`; refuses external VPNs | :689-722 |
| activate_connection(conn,dev,ap) | `activate:<uuid\|new>` | RemoteConnection → ActivateConnection; else `AddAndActivateConnection` | :724-756 |
| connect_wifi(net_id, pw?) | `wifi:<net_id>` | saved profile → activate_profile(first profile, device); else AddAndActivate with `make_wifi_connection` | :758-784 |
| deactivate(active_path) | `deactivate:<path>` | `DeactivateConnection(o)`; refuses external VPNs | :786-801 |
| disconnect_device(path) | `disconnect:<path>` | `Device.Disconnect()` | :803-809 |
| delete_profile(uuid) | `delete:<uuid>` | `Settings.Connection.Delete()` | :811-817 |
| ProfileStore.save (new) | — | `Settings.AddConnection2(settings, flags TO_DISK\|BLOCK_AUTOCONNECT, {})` | connection_profiles.py:346-348 |
| ProfileStore.save (edit) | — | `Connection.Update2(settings, TO_DISK\|NO_REAPPLY, {"version-id": t})` (omitted when version 0) | connection_profiles.py:354-357 |
| ProfileStore secrets preservation | — | `Connection.GetSecrets(setting)` for each secret-bearing setting, **only if** the draft carries a new secret; otherwise secrets are omitted (Update2 keeps them) | connection_profiles.py:361-403 |

`make_wifi_connection` (:127-187): id = SSID label, new uuid, type `802-11-wireless`, autoconnect true,
**permissions `user:<getpass.getuser()>:`**, mode infrastructure; key-mgmt map
`wpa-personal→wpa-psk, sae→sae, owe→owe, wep→none`; sae/owe get pmf REQUIRED; psk/wep-key0 flags AGENT_OWNED;
WEP key type KEY, else PASSPHRASE; ipv4/ipv6 method auto; normalize + verify. Validation: WPA psk via
`nm_utils_wpa_psk_valid` (8–63 chars or 64 hex), SAE 1–63 bytes, enterprise → error.

Security classification (`ap_security` :73-86, order matters): 802.1X/SuiteB192 → `wpa-enterprise`;
PSK (also WPA2/3 transition) → `wpa-personal`; SAE → `sae`; OWE/OWE_TM → `owe`; PRIVACY → `wep`; else `open`.
Wi-Fi network key: `"{device_path}|{ssid.hex() or ap_path}|{security}"`; the strongest AP wins and `active`/`profile_ids` are unioned (:481-506).
Saved profiles for a network are sorted by (-timestamp, name.casefold()). Device id = **NM D-Bus object path** (never the udev path, :42-49).
Device kinds: WIFI→wifi, ETHERNET→wired, MODEM→mobile, BT→bluetooth, else the enum nick with `-` (e.g. `wifi-p2p`, `loopback`, `wireguard`, `tun`).
Connectivity map 0..4 → `Unknown, Offline, Sign-in required, Limited connectivity, Online`; `connectivity_uri` = `ConnectivityCheckUri`.
External ownership: `Settings.Connection.Flags & EXTERNAL(0x8)` and `ActiveConnection.StateFlags & EXTERNAL(0x80)` (:52-60).

Error mapping (`friendly_error`, network_model.py:7-39): cancellation → "" (silent); AccessDenied/PermissionDenied/NotAuthorized → "You are not allowed to change this network setting."; ServiceUnknown/NameHasNoOwner/UnknownObject → "NetworkManager is not running."; Timeout → "The network service did not respond in time."; UnknownConnection/UnknownDevice/NotFound → "That connection or adapter is no longer available."; otherwise strip `quark:` / `GDBus.Error:name:` prefixes and a trailing ` (N)`.
Failure reporting: device→FAILED "Connection on {iface} failed: {reason}."; active DEACTIVATED with an abnormal reason (not NONE/UNKNOWN/USER_DISCONNECTED/CONNECTION_REMOVED) → "{id} failed: {reason}."; VPN FAILED (:300-317).

### 2.5 Other services consumed
* **Notifications** through `Gio.Application.send_notification` (GLib fdo backend → `org.freedesktop.Notifications`, tessel-notifications). IDs and content:
  * `network-<last path component of active conn>`: "Network connected"/"Network disconnected" + body name; suppressed by `disable-connected-notifications` / `disable-disconnected-notifications`, or `disable-vpn-notifications` for vpn/wireguard (network_manager.py:270-291). There is no notification for the first snapshot (previous=None) or while not ready.
  * `network-captive-portal`: title "Sign in to network", body "This network requires you to sign in before you can use the internet.", icon `network-wireless-hotspot-symbolic`, default action + button "Sign in" → `app.captive-portal`; only on the **transition** into "Sign-in required"; withdrawn on any other state (:238-252). Gated by `disable-portal-notifications`.
* Captive portal opening: `Gio.AppInfo.launch_default_for_uri_async(uri)`, uri = NM's check URI if it starts with http(s), else `http://nmcheck.gnome.org/check_network_status.txt` (:34-40,254-268).
* **Tessel Dock** `org.tessel.Dock /org/tessel/Settings org.tessel.Settings.GetPopupPlacement(ss connector, group="service_apps") → (s json{edge,align,offset})`, timeout 500 ms, NO_AUTO_START (shared/popup_placement.py:69-90). Invalid or missing → the CLI edge/offset is used.
* Palette theming via `tessel-dock/companion_theme.attach` (watches the dock palette file) plus `background_blur` (ext-background-effect-v1 through `libtessel-dock-background-blur.so`) are part of tessel-gtk.
* polkit: indirectly, through NM's ALLOW_INTERACTIVE_AUTHORIZATION (tessel-auth shows the prompt).

## 3. Files

| Path | Format | R/W | Other users |
|---|---|---|---|
| `${XDG_CONFIG_HOME:-~/.config}/waybar/network-notifications.json` | JSON object of bools, e.g. `{"disable-connected-notifications": true, "disable-portal-notifications": false}`. Keys: `disable-{connected,disconnected,vpn,portal}-notifications`. Only literal `true` counts as true. Re-read on **every** get (no cache); writes are atomic via a tempfile in the same dir. Non-dict or invalid → `{}`. | RW | Global Settings network page (in-process import, tessel-settings/applet_pages.py:285,297) |
| `~/.local/state/tessel/previews/tessel-network.json` | installer preview manifest `{sources:{name:sha256}, changes:{path:{before,after}}}` | installer only | |
| `~/.config/waybar/backups/network-<ts>/…` | installer backups | installer | |
| VPN export target | file chosen by the user, written atomically with **mode 0600**, no secrets; `os.link` (no overwrite) or `os.replace` (overwrite) | W | vpn_editor.py:124-155 |
| Installed runtime files | listed in install_waybar.py:31-57 (incl. shared `tray_popup.py`, `popup_placement.py`, `gtk4_utils.py`, `suite_i18n.py`, `companion_theme.py`, `background_blur.py`, `.so`) | | |

No passwords are written to app config or argv. Translations come from `shared/locale` via `suite_i18n`.

## 4. UI surfaces

### 4.1 Popup (network_popup.py + shared tray_popup.py)
* `Gtk.ApplicationWindow` titled "Network", CSS classes `networkmanager` + `tray-popup`, undecorated, not resizable, width 400 (cap 420).
* Layer shell: namespace **`desktop-applet`**, layer OVERLAY, exclusive zone −1, keyboard ON_DEMAND; anchored to the panel edge with margin = offset, plus side anchor RIGHT/BOTTOM (align `end`) or LEFT/TOP (`start`) with margin 12; `center` uses no side anchor (tray_popup.py:31-34,100-113).
* Click-away: one transparent fullscreen layer surface per monitor, namespace **`desktop-applet-dismiss`**, OVERLAY, anchored on all 4 edges, keyboard NONE, CSS `desktop-applet-backdrop`, a Cairo-painted DrawingArea (needed so the surface gets a buffer), and a GestureClick(button 0, capture) on release → dismiss (:64-98).
* Escape dismisses. Losing focus does **not** dismiss on layer shell (crossing outputs) (:204-228). Monitor is chosen from the pointer position (PointerMonitor) and then the dock placement.
* Scroll max height = clamp(80, monitor_h − offset − 240, 490) (network_popup.py:519-533).
* Content: header (icon `network-wired-symbolic`, title "Network", live subtitle, gear button "Network settings" → settings); conditional buttons "Enable networking", "Enable Wi-Fi", "Sign in to network" (`suggested-action`); sections "Ethernet/LAN", "Wi-Fi" (adapter cards `card adapter-card` with an info icon, name `device-name`, `iface · state` `device-detail`, a scan button for Wi-Fi, an enable Switch); "VPN" cards; "Other connections"; a "Mobile broadband" switch row only when a modem exists; footer "Hidden Network" / "Create Network"; status label `muted` (+`error`).
* Rows are rebuilt only when the `repr` signature of (ready, busy, flags, models) changes; the scroll position is kept (:247-257).
* Wi-Fi adapter: connected networks first; once established (device state 100 or an active wifi conn state 2) **only** the current network is shown; otherwise "Available networks (N)" plus rows sorted by (!active, −strength, label) (:419-447). Row labels: security names `WPA/WPA2, Enterprise, WPA3, Enhanced open, WEP, Open`; state `Saved` when profiles exist.
* Signal icons: `network-wireless-signal-{excellent≥75,good≥50,ok≥25,weak}-symbolic`.
* Connect logic (network_manager.py:343-352): active → disconnect the device; saved profile or open/owe → connect directly; otherwise open the Wi-Fi form (ConnectionWizard). Hidden/hotspot forms (:354-402): hotspot = mode `ap`, ipv4 `shared`, ipv6 `disabled`, key-mgmt `wpa-psk`, button "Create network".

### 4.2 Settings window (network_settings.py)
`Gtk.ApplicationWindow` titled "Network Settings", class `networkmanager`; headerbar with a preferences button
(→ the "Network Notifications" dialog with 4 checkboxes, network_manager.py:293-313); "Add connection" (`primary`);
SearchEntry + type ComboBoxText ids `all,wired,wifi,vpn,mobile,virtual,other` (:241-248); ListBox `device-list`
of ConnectionRow (badge `badge connected`, `connect-action` button, info/edit/remove icon buttons). Count label
"Saved connections · N[ of M]". Removal asks for confirmation in a MessageDialog with a `destructive-action` Remove button and checks the live profile again before deleting.
Esc closes; Ctrl+F focuses search (:413-420).

### 4.3 Other windows
ConnectionInfo (title "Connection Information", 570×520, read-only selectable grid, reused per identifier triple, refreshed live);
ConnectionWizard ("Add Connection"/"Edit Connection", chooser ListBox `device-list connection-types`, StackSidebar
`editor-sidebar`, Import VPN…/Export VPN…, primary "Continue"/save). Editors are cached by uuid or `"new"`. External
profiles cannot be edited. Dialogs are raised by `wlrctl toplevel focus app_id:org.tessel.NetworkManager title:<title>`,
retried up to 4 times every 100 ms when it fails (network_manager.py:198-223).

### 4.4 CSS
Classes to keep: `networkmanager, tray-popup, popup-surface, card, adapter-card, device-list, device-icon, network-tile,
network-row, device-name, device-detail, section-title, title, subtitle, muted, status, error, empty, empty-icon, badge,
connected, primary, connect-action, icon-button, destructive-action, suggested-action, editor-sidebar, connection-types,
desktop-applet-backdrop` + the shared popup classes (`popup-header, popup-icon-tile, popup-title, popup-subtitle,
row-title, row-detail, popup-footer, popup-header-button, popup-flat, popup-flat-icon`). The full sheet is in network_style.py:20-148
(priority APPLICATION) and tray_popup.py:288-325 (APPLICATION+5 / +35). Colors: `@define-color network_accent/surface/muted/line/green/error`.

### 4.5 Status stream (network_status.py) — consumed by tessel-dock
One compact JSON line per change (deduplicated, `ensure_ascii=True`), flushed; exit on BrokenPipe/SIGTERM:
`{"text": glyph, "tooltip": str, "class": [..]}` (:15-51).
* text: unavailable/disabled/disconnected `󰤭`; wifi `󰤟/󰤢/󰤥/󰤨` (<25/<50/<75/≥75); mobile `󰒧`; wired `󰈀`; connecting `󰤡`; VPN appends ` 󰌾`.
* class: `unavailable` | `disabled` | `connected` + kind | `connecting` | `disconnected`; + `vpn`; + `limited` (Limited or Sign-in).
* tooltip: description, `\nVPN: a, b`, connectivity line, `\n{iface}: {state}` per device except loopback/wifi-p2p, final line "Click for network controls · Right-click for settings".
* primary = first active state==2 non-loopback with default/default6 and not vpn, else the first non-vpn.
Tessel Dock parses this into an indicator (strips Pango, keeps classes `[A-Za-z0-9_-]+`, lowercased; tessel-dock/status_backend.py:44-58). The dock icon is its own glyph ``.

## 5. External commands spawned
`wlrctl` (optional, raising windows); VPN `auth-dialog` helpers (trusted root-owned); the default browser via GIO for the portal URI.
The installer only runs `systemctl --user daemon-reload/restart/is-active/stop tessel-network.service`, sends SIGUSR2 to waybar, and runs itself with `--check`.
It **never** runs `nm-applet`, `nm-connection-editor` or libnma (test_network.py:481).

## 6. Global Settings coupling (high risk for the port)
`tessel-settings/applet_pages.py:281-400` **imports the Python classes** (`NetworkApplication`, `NetworkPopup(embedded=True)`,
`NetworkSettingsWindow`, `NotificationPreferences`, `style`) and builds a second NON_UNIQUE app
`org.tessel.GlobalSettings.NetworkDialogs` with its own NetworkBackend but **no secret agent**. It uses tabs "Connections"/"Saved
connections" and reaches into widget internals (`rows_box`, `subtitle`, `popup-surface`). Search entries are in
tessel-settings/settings_search.py:72-76,158 (targets `search_entry`, `add_button`). A Rust port must either provide a
replacement embedding (e.g. a page crate shared with tessel-settings) or keep the Python modules available until tessel-settings is ported.

## 7. Parity test cases to port
Backend (test_network_backend.py):
1. External WireGuard profile + activation + device → one card (:192); the native VPN underlay is not claimed (:208); generic tun/veth are not VPN (:219); explicit VPN claims its tun (:228).
2. Orphan WireGuard device: shown, read-only, external (:240); orphan activations with a missing profile cannot reconnect (:251); compatible devices are not "owned" by inactive profiles (:263); Disconnecting state preserved, external flag wins (:278).
3. Wi-Fi profiles keep secrets AGENT_OWNED and pass verify; invalid psk/enterprise rejected (:290,306); security classification incl. transition/OWE (:313).
4. refresh() performs **zero** mutations; strongest AP groups; per-adapter+security grouping; hidden beacons separate (:344,356).
5. Virtual devices with the same missing udev path stay separate (:364,384).
6. External VPN connect/disconnect rejected before any native call, re-checked after the snapshot (:429,441).
7. Saved Wi-Fi reuses its profile (no duplicate); a new profile targets the requested adapter+AP (:469,478); stale ids rejected (:490).
8. Radio actions are async and non-optimistic (:499); adapter switch = SetManaged RUNTIME|SET_ADMIN_STATE on the canonical path only, never touching global radio/networking, no legacy fallback, permission failure clears busy (:514-633).
9. Signal disconnect never calls `NM.Device.disconnect` (the network action) (:657); scan stays busy until LastScan advances (:663); stale replies after daemon loss/close are ignored (:679); VPN failures reported (:698).
Secret agent (test_secret_agent.py): non-interactive requests never prompt or unlock (:185); saved secret returns with no prompt (:190); REQUEST_NEW prompts even when stored (:197), and without interaction never reuses (:204); queued concurrent prompts, cancel once (:210); a late keyring callback after cancel is ignored (:226); accept saves only the owned secret and clears the clone (:234); close cancels all (:243); flag rules (:251-281); PSK/WEP/LEAP/802.1X/peer prompts (:288-391); helper protocol escaping (:394); no password in argv (:404); user-owned helper rejected (:421); legacy and external-UI replies (:432,439); pipe + reaping, cancel/timeout kill the process group (:449-489); Retrievable lookup (:490); save clears only the requested setting (:512); ask-every-time stores nothing (:541). Bus: Register/Unregister sequence (test_secret_agent_bus.py).
Profiles (test_connection_profiles.py): AddConnection2 TO_DISK|BLOCK_AUTOCONNECT (:166); Update2 omits secrets unless changed, then fetches and merges all existing secrets (:190-243); a failed preservation never submits a partial update (:218); external/deleted/uuid-changed/version-changed rejected (:264-275); duplicate save not queued (:284); close or backend restart suppresses writes/callbacks (:293,303).
UI (test_network.py): multi-adapter identity (:141); per-adapter scan (:177); a secure new network prompts, saved/open connect directly (:196); a connected adapter shows only its network (:278); VPN switch restores the confirmed state on failure (:333); the mobile row appears only with a modem (:440); portal button + single notification + action + withdraw (:650); friendly errors (:679); dismiss by click-away/Escape, settings reuse, 1 backend + 1 agent (:703); remote launches reuse the primary, `--check` exit codes 0/2/absent≠0 (:733).
Forms/VPN (test_connection_forms.py, test_vpn_editor.py): existing passwords are never displayed or erased; unknown VPN options round-trip exactly; export omits secrets, is 0600 and atomic (:160).
Installer (test_install_waybar.py): the replacement unit drops `[Install]` (:17); rollback restores the old service (:70-101).

## 8. Port notes / risks
* libnm features used that have no pure-D-Bus equivalent and must be reimplemented: `connection_compatible`, `ap.connection_valid`,
  `normalize()/verify()/verify_secrets()/need_secrets()`, `utils_wpa_psk_valid/wep_key_valid/ssid_to_utf8`, and GObject property
  introspection for the generic editor (`setting_schema` over all `NM.Setting*` classes, connection_profiles.py:89-141).
  Alternative: use libnm via gir-generated Rust bindings (the `nm`/`libnm` crates are thin/unmaintained) — decide up front.
* VPN import/export loads the **native editor plugin .so** (`NM.VpnPluginInfo.load_editor_plugin()`, vpn_editor.py:93,116,137). This is C FFI to `libnm-vpn-plugin-*-editor.so` in Rust, or keep a helper.
* Wayland: only layer-shell (gtk4-layer-shell crate) + ext-background-effect-v1 (shared tessel-gtk blur). No foreign-toplevel; raising goes through the external `wlrctl`.
