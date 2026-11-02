# tessel-network (netman) — parity with contracts/tessel-network.md

Rust crate `components/network`: binaries `tessel-network` (= `network_manager.py`)
and `tessel-network-status` (= `network_status.py`), example `fake_network_services` (measurement
fakes), shims in `shim/`. Tests: `cargo test -p tessel-network` (unit + private-bus
end-to-end), `tests/run_headless.sh` (GTK checks in a private nested headless labwc),
`tests/measure_headless.sh` (Rust vs Python timing/RSS + screenshots, private labwc, fake NM).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## Architecture (PLAN.md rule 8)
* NetworkManager is reached over **raw D-Bus via gio** (not zbus, same as every other port):
  `GetNameOwner` → `ObjectManager.GetManagedObjects` on `/org/freedesktop` +
  `Settings.Connection.GetSettings`, then an incremental cache from `InterfacesAdded/Removed`,
  `PropertiesChanged` and `Updated`; failure reports from `Device.StateChanged`,
  `Connection.Active.StateChanged`, `VPN.Connection.VpnStateChanged`. No `NMClient`.
* The secret agent is our own D-Bus object (`org.freedesktop.NetworkManager.SecretAgent`),
  rejects callers other than NetworkManager's unique name (libnm `verify_request`), registers
  with `RegisterWithCapabilities("org.tessel.NetworkManager", 1)`, re-registers on every
  NetworkManager (re)appearance, `Unregister` on shutdown.
* The keyring is a raw Secret Service client (plain session, nm-applet schema, DONT_MATCH_NAME).
* **libnm C FFI** (`src/libnm.rs`, `dlopen("libnm.so.0")` on first use — the status stream and the
  read-only backend never load it) only for libnm-only logic: `NMSimpleConnection` from/to
  `a{sa{sv}}`, `normalize/verify/verify_secrets/need_secrets/update_secrets/clear_secrets`, setting
  GObject introspection (editor schema, secret flags), IP address/route/routing-rule and
  WireGuard peer boxed types, `nm_utils_{wpa_psk_valid,wep_key_valid,ssid_to_utf8,uuid_generate}`,
  ethtool option helpers, VPN plugin metadata (`.name` files) and editor-plugin import/export.
  **Runtime package needed:** `NetworkManager-libnm` (already installed; no `-devel` needed).
  VPN import/export additionally needs the `NetworkManager-<vpn>` plugin packages (as before).
* Device/profile compatibility, AP matching and device names use **libnm itself, exactly as
  Python did**: the daemon keeps a read-only libnm `NMClient` view (created on attach, never
  mutating) and calls `nm_device_connection_compatible(device, remote)`,
  `nm_access_point_connection_valid(ap, remote)` and `nm_device_get_description(device)` on the
  same objects Python used (`backend::LibnmMatcher`). Results libnm's cache does not have yet are
  treated as unknown and re-checked every 250 ms (bounded). The status stream uses `NoMatcher`
  and never loads libnm (asserted via /proc/<pid>/maps in private_bus). ✅ backend_tests
  (`libnm_cache_lag_…`, `device_names_come_from_libnm_descriptions`, Python's fake matching),
  private_bus, screenshots byte-identical (incl. libnm's "not saved" decision for the fixture AP).
* nm-connection-editor / libnma: never run or loaded (same as Python).

## §1 Entry points / CLI
| Contract item | | Test |
|---|---|---|
| GApplication `org.tessel.NetworkManager`, `HANDLES_COMMAND_LINE`; prgname = app id | ✅ | private_bus |
| argparse: modes mutually exclusive, `--popup-edge` choices, `--popup-offset` int ≥ 0 (exit 2 before D-Bus), abbreviations, `--help` text | ✅ | `args::tests::*`, private_bus (`--popup-offset -3`) |
| `--check` JSON `{"ready","network_ready","agent_ready","devices","profiles"}` (json.dumps spacing), exit 0/2 | ✅ | private_bus (exact line) |
| `--check` without primary: IS_LAUNCHER, never primary, no agent, no backend | ✅ | private_bus |
| secondary launches forward to the primary | ✅ | private_bus (`--background` twice, one agent) |
| `--popup` same position → activate, different → rebuild; `--settings` dismisses popup, reuse + raise (`wlrctl` ×4/100 ms); `do_activate` → popup | ✅/☑ | measure_headless screenshots, ui_headless (settings reused) |
| SIGTERM/SIGINT → quit; shutdown order agent → backend → windows → release | ✅ | private_bus (clean exit, `Unregister` seen) |
| `app.captive-portal` action | ✅ | private_bus (notification action `app.captive-portal` / "Sign in") |

## §2 IPC
| Item | | Test |
|---|---|---|
| Secret agent methods/errors (`Failed, PermissionDenied, InvalidConnection, UserCanceled, AgentCanceled, NoSecrets`) | ✅ | agent_tests, private_bus |
| GetSecrets algorithm (keyring lookup with UNLOCK only when interactive, merge, VPN always via helper, missing/always_ask/REQUEST_NEW rules, NoSecrets) | ✅ | `agent::tests::*` (non-interactive, saved secret, REQUEST_NEW, no-interaction REQUEST_NEW, queue+cancel once, late keyring callback, close cancels all) |
| Prompt fields (hints / need_secrets / defaults, SECRET-only, WEP key index, LEAP, 802.1X, WireGuard peer PSK caption), remember checkbox rule, AGENT_OWNED/NOT_SAVED, verify_secrets, entries blanked | ✅ | `agent::tests::*`, ui_headless (masked prompt) |
| Return format incl. `password-raw` ay, VPN `secrets` a{ss}, WireGuard `peers` aa{sv} | ✅ | `peer_preshared_key_prompt_updates_peer_flags`, `vpn_helper_reply_*` |
| Save only the requested setting on accept; SaveSecrets/DeleteSecrets | ✅ | agent_tests, `keyring::tests::*` |
| VPN auth helper: trust check, argv (no secrets, -i/-r/-t/--external-ui-mode), stdin protocol, new session, 300 s timeout, killpg, response parsing (legacy + external UI v2) | ✅ | `vpn_auth::tests::*` |
| Register/Unregister sequence; re-register on NameOwnerChanged | ✅ | private_bus |
| Secret Service schema/attributes/label/default collection, Retrievable-style lookup | ✅ | `keyring::tests::*`, private_bus (saved synthetic psk returned) |
| Backend read-only start, 3000 ms reconnect, 60 ms coalesced refresh, not-running state | ✅ | backend_tests (`refresh_reads_without_mutations…`, `daemon_loss…`, `incremental_signals…`) |
| Mutations + busy keys (Enable, Set WirelessEnabled/WwanEnabled, SetManaged RUNTIME\|SET_ADMIN_STATE, RequestScan + LastScan/12 s, ActivateConnection, AddAndActivateConnection, DeactivateConnection, Disconnect, Delete) | ✅ | backend_tests (all BackendTests cases) |
| `make_wifi_connection` (permissions `user:<getuser()>:`, PMF, WEP key type, AGENT_OWNED, validation) | ✅ | `native_wifi_profiles_…`, `invalid_wifi_credentials_…`, `new_wifi_profile_targets…` |
| Security classification, network grouping, profile sort, external flags, connectivity map, friendly errors, failure messages | ✅ | `nm::tests`, backend_tests, `model::tests::friendly_errors_match_python` |
| ProfileStore AddConnection2 / Update2 (version-id) / secret preservation via GetSecrets | ✅ | `profiles::tests::*` (all StoreTests cases) |
| Notifications (connected/disconnected/VPN ids `network-<n>`, portal transition + withdraw, preference gating) | ✅ | private_bus (portal once + withdraw, "Network disconnected") |
| Captive portal URI (`ConnectivityCheckUri` http(s) else nmcheck.gnome.org) | ✅ | `app::tests::portal_uri_prefers_networkmanager_check` |
| Tessel Dock popup placement, theme, blur | ☑ | via tessel-gtk PopupController/theme (screenshots identical) |
| ⚠ Mutations use `ALLOW_INTERACTIVE_AUTHORIZATION` and `G_MAXINT` timeout (`compat::NmBus`); reads use `NO_AUTO_START` (FakeBus cannot observe flags/timeouts) | ☑ | |

## §3 Files
`network-notifications.json` (re-read every get, literal `true`, merged atomic writes with
Python's `json.dumps` spacing): ✅ `preferences::tests::literal_true_only_and_merged_atomic_writes`.
VPN export 0600/atomic/no secrets: see forms/VPN section. ⚠ an *empty* `XDG_CONFIG_HOME` falls
back to `~/.config` (tessel-core rule; Python used the empty string).

## §4 UI
| Item | | Test |
|---|---|---|
| Popup (title, classes, layer shell via PopupController, scroll clamp, header/gear, enable buttons, portal button, adapter cards, Wi-Fi list rules, VPN cards, other connections, mobile row only with modem, footer, status texts, repr-signature rebuild) | ✅ | measure_headless: popup PNG **byte-identical** to Python; ui_headless |
| Settings window (headerbar, preferences button, Add connection, search, type filter ids, ListBox rows, badges, count label, empty states, confirm remove with `destructive-action`, Esc/Ctrl+F) | ✅ | measure_headless: settings PNG **byte-identical**; ui_headless |
| Connection information window (reused per identifier triple, live refresh) | ✅ | ui_headless |
| Network Notifications dialog (4 checkboxes) | ☑ | |
| CSS sheet verbatim (`STYLE_CSS`) + shared popup CSS | ✅ | screenshots |
| Status stream glyphs/classes/tooltip, dedup, ensure_ascii, EPIPE/SIGTERM | ✅ | `status::tests::*`, private_bus |
| Connection wizard, forms, VPN editor | see below | |

## Connection editor (connection_wizard.py, connection_forms.py, vpn_editor.py)
Ported in `src/ui/wizard.rs`, `src/ui/forms.rs`, `src/ui/vpn_editor.rs`: chooser with search,
StackSidebar editor (`editor-sidebar`, `connection-types`), optional settings, SettingPage/PropertyField
(libnm GObject introspection), RowsEditor, IPPage/IPAttributes, WireGuard peers, Ethtool, 802.1X CA
decision, WirelessForm, VPN form (protocol fields/secrets, unknown options kept), catalog + plugin
import/export (0600, atomic, secrets cleared), Saving… / close blocked, activate mode.
✅ 8 forms + 10 vpn_editor unit tests (`ui::forms::tests::*`, `ui::vpn_editor::tests::*`) and 8 GTK
checks in ui_headless (existing passwords never displayed/erased, unknown VPN options round-trip, …).
✅ Structured list properties are editable one item per line (`src/ui/structured.rs`): tc
`qdiscs`/`tfilters` and SR-IOV `vfs` through libnm's own string parsers/renderers, OVS `trunks`,
bridge/bridge-port `vlans` and team/team-port `link-watchers` in Python's formats, dcb `au`
arrays and any `ai/au/ax/at/ab/as` array; everything is validated before the setting changes.
Only unparsed structures (none writable besides IP addresses/routes, which have their own page)
keep the "preserved unchanged" caption. ✅ `ui::structured::tests::*`, ui_headless (bridge VLANs).
✅ Real plugin tests: `vpn_auth::tests::real_plugin_name_file_drives_the_helper_command` (installed
`.name` file read-only via libnm `new_from_file`, fixture fallback) and
`ui::vpn_editor::tests::installed_openvpn_plugin_imports_and_exports_without_secrets` (real
openvpn editor plugin: import into memory, private 0600 export without secrets, no overwrite).
☑ The wizard's own window/file-dialog flow is exercised only through headless_checks.

## Deviations ⚠
1. gio D-Bus instead of zbus (consistent with the other ports; no executor thread).
2. Lazy GTK: `--background` does not initialize GTK (no display needed); windows are plain
   `gtk::Window`s (no `GtkApplication`, so no `org.gtk.Actions` window actions); the Wayland
   app_id is still `org.tessel.NetworkManager` (prgname).
3. Argparse errors of a forwarded command line go to the caller's stderr.
4. Locked keyring items are skipped for non-interactive lookups (libsecret returned an error
   per locked item; the result — no secret, NoSecrets — is the same).
5. A secret-agent request from any bus name other than NetworkManager's owner is rejected
   (libnm did the same inside `NMSecretAgentOld`).

## Gaps ❌
- Global Settings imports the Python classes (`NetworkApplication`, `NetworkPopup(embedded=True)`,
  `NetworkSettingsWindow`, …): the Python modules must stay installed until Global Settings is
  ported. The Rust `NetworkPopup::new(.., embedded=true)` exists for a future Rust embedding.
- Python version could not reach `ready` against the fake NetworkManager in the measurement
  (libnm's secret agent does not register with the minimal fake), so its time-to-ready is n/a.
- Installer pieces (unit, .desktop, Waybar module, `--check` poll + rollback; test_install_waybar.py)
  are not part of this crate.

## Measurements (headless labwc, fake NM, release build)
Rust `tessel-network` 3.4 MB, `tessel-network-status` 1.9 MB. `--background` time-to-ready 47 ms
(Python: never ready against the fake — libnm's agent did not register — so n/a). Idle RSS
28.9 MB / PSS 9.7 MB vs Python 84.2 / 52.8 MB; after popup+settings 75.9 / 44.7 MB vs
100.6 / 61.8 MB. Status stream: first line 12 ms vs 56 ms, RSS 7.7 / PSS 2.4 MB vs 36.3 / 18.9 MB.
Popup and settings screenshots are byte-identical to Python.
