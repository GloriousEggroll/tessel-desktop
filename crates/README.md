# Foundation crates: `suite-core`, `suite-gtk`, `suite-power` and `suite-wayland`

Rust ports of `~/btmanager/shared` (plus Waydock's palette theme, and the
power backend shared by power-manager and battery-manager). Behaviour,
file paths, CSS class names and strings match the Python modules; read this
instead of the source. Depend on them from a component with:

```toml
[dependencies]
suite-core.workspace = true
suite-gtk.workspace = true          # GUI components only
gtk.workspace = true                # = gtk4 0.11, feature v4_22 (system GTK 4.22)
gtk4-layer-shell.workspace = true   # if you use layer shell directly
suite-power = { workspace = true, features = ["gtk"] }   # power/battery components only
suite-wayland = { workspace = true, features = ["glib"] } # + "gtk" for GTK-display use / textures
```

`gio` is built with feature `v2_80` workspace-wide. Unix signal/fd sources come
from `suite_core::glib_compat` (no direct `glib-unix` dependency needed).

Startup order in every `main`: `suite_core::i18n::initialize();` **before**
`gtk::init()` / `Application::run` (GTK calls setlocale too).

Link order: `suite-gtk`'s build script links `-lgtk4-layer-shell` before
`-lwayland-client` (layer shell interposes `wl_*`; Python achieves this by
loading it `RTLD_GLOBAL` first). Do not add your own earlier
`wayland-client` link, or layer surfaces silently fail
("GTK4 Layer Shell may have been linked after libwayland").

---

## suite-core

### `i18n` (suite_i18n.py) and `tr!`
- `tr!("Text")`, `tr!("Saved {n} at {p:.0f}%", n = 3, p = 41.6)` → `String`.
  Values: `&str`/`String`/ints/floats/bools (`pyfmt::Arg`). Python
  `str.format` rules: named fields, specs (`.0f`, `g`, `>5`, `,` …),
  conversions `!s !r !a`, `{{ }}` escapes. Without values the text is **not**
  formatted (`{{` stays), exactly like Python.
- `translate(msg)`, `translate_with(msg, &[("name", Arg)])`,
  `translate_in(&catalog, msg, values)` (pure, for tests).
- A translation is used only if its `{}` fields (name, spec, conversion) and
  `%`-placeholders equal the source's (`same_placeholders`); empty or damaged
  entries fall back to the source; format failures fall back to the formatted
  source (and finally the raw source, where Python would raise).
- `initialize() -> &'static str` (idempotent). With a valid
  `~/.config/desktop-suite/language.json` (`{"version":1,"language":"fr","locale":"fr_FR.UTF-8"}`)
  it removes `LC_ALL` (copying its value, else `LANG`, else `C.UTF-8`, into the
  unset `LC_NUMERIC…LC_IDENTIFICATION`), sets `LANG`/`LC_MESSAGES` to the
  locale and `LANGUAGE` to the code, then `setlocale(LC_ALL, "")`. Without a
  preference the environment is untouched and the language is
  `normalize_language(LANGUAGE|LC_ALL|LC_MESSAGES|LANG)`.
- `language()`, `is_rtl()`, `selected_language()`, `normalize_language(Option<&str>)`,
  `read_preference()`, `preference_path()`, `environment_changes(..)` (pure).
- Catalogs: `<dir>/<code>.json` (flat `{"English": "Translation"}`), loaded
  lazily and cached per code (`clear_catalog_cache()`). `catalog_directory()`:
  1. `$DESKTOP_SUITE_LOCALE_DIR` (Rust-only override for tests/dev runs),
  2. `<directory of the executable>/locale` if it exists,
  3. `$XDG_DATA_HOME/desktop-suite/locale` = `~/.local/share/desktop-suite/locale`.

  **Installer must ship** `~/btmanager/shared/locale/*.json` (31 files) to
  `~/.local/share/desktop-suite/locale/`. The Python installers currently ship
  only the generated `suite_catalogs.py` (base85+zlib bundle) next to each
  component, which the Rust side does not read. Missing catalogs ⇒ English UI.

### `lang` (language_data.py)
`LANGUAGES: [(code, native name, locale); 31]` (Python order), `RTL_LANGUAGES`,
`is_supported`, `language_name`, `locale_name`, `is_rtl`.

### `paths` (XDG exactly as Python: non-empty env var, else `$HOME/…`)
`home, config_home, cache_home, data_home, state_home, runtime_dir,
suite_config_dir (~/.config/desktop-suite), suite_data_dir
(~/.local/share/desktop-suite), component_config_dir/cache_dir/state_dir(name)`.

### `clock` (clock_preferences.py)
- `ClockPreferences { stacked, date_first, use_24_hour, date_order ("mdy"…),
  date_separator ("/","-","."), show_date, show_year, show_weekday, month_name,
  short_names }` with Python defaults; `from_map`, `to_map`,
  `format_time/format_date/format_weekday/ordered_parts(&impl ClockTime)`.
  `ClockTime` is implemented for `glib::DateTime` (locale-aware `%p %a %A %b %B`).
- `ClockPreferencesStore::new(path)` / `default()` (`~/.config/desktop-suite/clock.json`):
  `load()` (re-reads on inode/mtime/size change; keeps last good state on a bad
  file), `update(&Map)` → `Result<_, StoreError::{Invalid, Io}>` (validates,
  merges into the current file preserving unknown keys, writes `version: 1`,
  sorted 2-space JSON, atomic temp+fsync+rename).
- `atomic_write(path, bytes, prefix)` — reusable atomic writer.

### `service_settings` (service_settings.py) — D-Bus contract
Interface on the GApplication's object path: `GetState() -> s` (JSON),
`<Method>(<sig>) -> (b ok, s error)`, signal `Changed(s)`.
- `SettingsService::new(&app, interface, method, signature, snapshot: Fn() -> serde_json::Value,
  apply: Fn(&Variant) -> Result<(bool, String), String>)` (call after
  registration, e.g. in `startup`). `Err` replies `<interface>.Error`. `.changed()`
  emits `Changed`; it is also emitted after every setter call. `.close()`.
- `SettingsClient::new(on_changed, bus_name, interface, unavailable_text)`,
  `.start()` (object path `/` + name with `.`→`/`; never auto-starts),
  `.state() -> Map`, `.ready()`, `.error()`, `.busy()/set_busy()`,
  `.call(method, Option<&Variant>, Option<Box<dyn FnOnce(ok, error, Option<Variant>)>>) -> bool`
  (replies arriving after an owner change report `unavailable`), `.close()`.
- JSON text is Python's `json.dumps` (`", "`/`": "`, ASCII escapes).

### `single_instance`
Components are unique GApplications with `HANDLES_COMMAND_LINE`:
`application_flags(launcher)` adds `IS_LAUNCHER` for processes that must never
become the primary (status queries, tray clicks; only `--background` may).
`object_path(app_id)`, `name_has_owner(&conn, name)`. Window raising:
`suite_gtk::utils::raise_window`.

### `pyfmt`
`format`, `parse` (= `string.Formatter().parse`), `format_value`, `float_repr`,
`json_dumps(&Value, &JsonStyle)` with `JsonStyle::{DEFAULT, COMPACT}` or custom
(`ensure_ascii`, `sort_keys`, `indent`, separators). serde_json is built with
`preserve_order`, so maps keep insertion order like Python dicts.


### `glib_compat` (APIs gtk-rs 0.22 dropped from glib/gio)
`unix_signal_add_local(signum, FnMut() -> ControlFlow) -> SourceId` and
`unix_fd_add_local(fd, IOCondition, FnMut(fd, cond) -> ControlFlow)` (re-exported
from `glib-unix`; default main context, main thread), `source_remove(SourceId)`
(only if the source still exists, Python's `find_source_by_id` guard),
`desktop_app_info(desktop_id) -> Option<gio::AppInfo>` (`Gio.DesktopAppInfo.new`).

### `timer` (thread-default main context)
`once(ms, FnOnce) -> Timeout`, `repeat(ms, FnMut() -> bool) -> Timeout`
(`Timeout::cancel()`, `is_pending()`) attach to the *thread-default* context
(the global default unless one was pushed), so tests can run each case on a
private `MainContext::with_thread_default`. `settle(&ctx, dur)`,
`wait_for(&ctx, timeout, predicate) -> bool` iterate a context; `now()` = `time.time()`.

### `scheduler` (injectable `GLib.timeout_add`)
`trait Scheduler { timeout(ms, Box<dyn FnOnce()>) -> u64; cancel(id) }`;
`GlibScheduler` (real, via `timer::once`), `FakeScheduler` for tests
(`pending`, `cancelled`, `fire(id)`, `delays()`, `ids()`, `created()`).

### `bus` (gio D-Bus client behind a trait; Python `FakeConnection` tests)
`trait Bus { call, call_with_fd ((h) reply + one fd), subscribe(sender, iface,
member, path, arg0, handler) -> u32, unsubscribe, cancel_reads, on_closed }`.
`Call::new(name, path, iface, method, params)` (`.mutation()` = interactive
write: never cancelled, 120 s; reads 10 s, cancellable), `Signal`, `Reply`,
`BusError {name, message}` (`from_glib` splits the remote error name, `local`,
`remote`), `PROPERTIES`. `GioBus::new(connection)`, `connect(BusType, cancellable,
done)` (honours `DBUS_SYSTEM_BUS_ADDRESS`). `FakeBus` records `calls`, defers
`pending` replies: `reply(method, result, path, name, index)`, `has_pending`,
`called`, `last_call`, `last_pending`, `methods`, `signals`, `unsubscribed`,
`cancelled`, `closed_handler`.

### `values` (GVariant → Python-like JSON, PyGObject `unpack()`)
`to_value(&Variant)` (`a{sv}` → object, `ay`/tuples → arrays, non-finite → null),
`first(&reply)`, and checks on optional dict entries: `number` (int/float, not
bool), `int` (`type(v) is int`), `is_true`/`is_false` (`v is True/False`),
`truthy`, `text` (`str(v or "")`), `py_eq` (`1 == 1.0 == True`, missing = None),
`int_in`.

### `pyjson` (Python value semantics on `serde_json::Value`)
`truthy`, `first_truthy`, `py_int(Option<&Value>)` (`int()`: `" -1_0 "` → -10),
`py_float` (`float()`, inf/nan), `py_str` (`str(v)`), `py_str_or_empty`
(`str(v or "")`), `py_str_opt`, `py_repr_value`, `eq_int`, `contains_int`,
`is_true_value` (`v in (True, "true", 1)`), `py_strip`, `is_py_space`, `casefold`.
(`service_settings::truthy` re-exports `pyjson::truthy`.)

### `pyerr` (Python exception text)
`python_error_text(&glib::Error)` = PyGObject `str(GLib.Error)`
(`"g-io-error-quark: gone (1)"`), `strerror(&io::Error)` (`exc.strerror`),
`os_error_text` (`"[Errno 2] No such file or directory"`),
`os_error_path(&err, path)` (`"…: '/path'"`).

### `base64`, `secrets`, `shell`, `fsutil`
- `base64::encode`, `decode_strict` (`b64decode(validate=True)`),
  `encode_urlsafe_unpadded`.
- `secrets::token_urlsafe(n)` (getrandom), `compare_digest(a, b)` (constant time).
- `shell::shell_quote`/`shell_join`/`shell_split` (`shlex`, POSIX),
  `which(name)` (`shutil.which`: executable regular file on `PATH`), `is_executable`.
- `fsutil::write_private(dir, name, prefix, bytes)` — `mkstemp(prefix)` +
  `os.replace`: directory created 0700, file 0600, temporary removed on failure,
  errors as Python `OSError` text.

### `service_settings::SettingsClient::proxy()`
`Option<gio::DBusProxy>` once connected (Python `self._proxy`), for raw calls on
another interface of the same object (keep-awake's `SetDuration`).

---

## suite-gtk (`suite_gtk::gtk` re-exports gtk4)

### `utils` (gtk4_utils.py)
`add_css(css, Option<&Display>, priority) -> CssProvider` (also installs once the
search-entry text margins at APPLICATION+40), `remove_css`, `css_once(display, key, css, prio)`,
`has_css`, `PRIORITY_APPLICATION` (600) / `PRIORITY_USER` (800),
`children`, `remove_children`, `set_margins`, `ICON_FALLBACKS`,
`icon_fallbacks(name)`, `themed_icon(name)`, `image(name, size)` (Python default
size 16), `accessible(widget, label)`, `append(box, child, expand, end)`,
`pack(box, child, expand, fill, padding, end)` (GTK3 pack_start/pack_end),
`headerbar(title)` (`.suite-headerbar`, `.title` label, CSS at APP+10 once),
`SIDEBAR_CLASS`/`SIDEBAR_CSS`/`sidebar_style(widget)` (APP+5 once),
`key_controller(widget, capture, cb)`,
`raise_window(app_id, &window, still_valid)` (`wlrctl toplevel focus app_id:… title:…`,
≤4 retries every 100 ms).

### `popup` (tray_popup.py)
- `PopupController::new(&window, apply_state, edge, offset, group)`
  (Python defaults `"bottom", 76, "service_apps"`). Adds `.tray-popup`, layer
  namespace `desktop-applet` (OVERLAY, exclusive −1, keyboard ON_DEMAND);
  click-away backdrops per monitor (`desktop-applet-dismiss`,
  `.desktop-applet-backdrop`); Escape / close-request / monitor change dismiss.
- `show()`: `begin_show()` starts a new generation, a `PointerMonitor` finds the
  pointer's output, `dock_placement` asks Waydock, then (only if the generation
  is still current) moves to that monitor (hiding first if it changed),
  applies `edge/offset/align`, shows backdrops, calls `apply_state`, presents.
- `show_pending()` is true from `begin_show()` until the popup is presented
  or `dismiss()`. **Owners that close/destroy the popup on hide must ignore
  hides while `show_pending()`** (the monitor move hides it first) — this is
  the hide/re-show race fix.
- `activate()` (tray click: raise suspended child via `set_raise_window`, else
  toggle), `dismiss()`, `set_position(edge, offset)`, `suspend_for_child(&w)`,
  `close()`, getters `edge/offset/align/group`, `set_group`, `backdrops()`.
- Layout: `POPUP_WIDTH` 400, `POPUP_MAX_WIDTH` 420, `POPUP_PADDING` 16,
  `POPUP_LAYOUT_CSS` (APP+5) / `POPUP_FLAT_CSS` (APP+35) via
  `install_popup_css(display)`, `set_popup_width(&window, width) -> i32`,
  `header_button(icon, tooltip, Option<Box<dyn Fn()>>, toggle) -> Button`,
  `popup_header(&[icon names], title, Option<subtitle>, Vec<HeaderAction>) -> PopupHeader
  {root, tile, icon, title_label, subtitle_label, actions_box, set_subtitle()}`,
  `section_title(text)`, `footer_button(label, cb, primary, Option<icon>)`,
  `popup_footer(&[Option<Widget>])`, `set_symbolic_icon(&image, &[names], glyph)`,
  `adwaita_symbolic(name)`,
  `friendly_status(message, fallback, &[(needle, friendly)])` (raw errno /
  exception / path text → fallback; logged as `tray-popup` warning).

### `placement` (popup_placement.py)
`PointerMonitor::new(FnOnce(Option<Monitor>))` (1×1 input-transparent probe,
namespace `suite-output-probe`, 1 s timeout; `.cancel()`),
`dock_placement(monitor, group, FnOnce(Option<Placement>))` — D-Bus
`org.waydock.Waydock /org/waydock/Settings org.waydock.Settings.GetPopupPlacement(ss) -> s`,
500 ms, no auto-start; `Placement {edge, align, offset}` / `Placement::from_json`,
`anchor_popup(&window, edge, offset, align)` (12 px side margin unless center),
`edge_from_name`, `pass_input(&native)` (empty input region).

### `app_menu` (app_menu.py)
`install_app_menu(&app, &window, MenuTarget::{New, Model(Menu), Button(MenuButton)},
AboutDetails {name, icon, version, comments, website}, Vec<ShortcutSection>,
Option<&HeaderBar>, Option<Rc<dyn Fn()>> quit) -> MenuButton`. Appends section
"Keyboard Shortcuts / About {name} / Quit" (translated), actions
`app.shortcuts` (`<Control>question`), `app.about`, `app.quit` (`<Control>q`;
default closes every window via close-request). Hamburger from Adwaita's
`open-menu-symbolic.svg` (`menu_gicon`, `use_menu_icon`, `has_menu_icon`),
classes `flat suite-app-menu`. `ShortcutSection = (String, Vec<Shortcut>)`,
`Shortcut::new("<Control>o", "Open")`, `Shortcut::any(&["Left","Page_Up"], …)`;
`set_shortcuts`, `shortcuts_window(parent, sections, title) -> ShortcutsWindow
{window, search, rows}`, `about_dialog(parent, &details)`, `accelerator_label`,
`restyle_when_mapped(&window)`.

### `osd` (adjustment_osd.py)
`AdjustmentOsd::new(on_dismiss, duration_ms /*1500*/)`,
`.show(Adjustment::{Volume, Brightness}, Option<f64>, muted, Option<&Monitor>, detail)`,
`.resolve_monitor(FnOnce(Option<Monitor>))`, `.close()`, `.window()`;
namespace `desktop-adjustment-osd`, input passes through, centered, `CSS`,
helpers `icon_name`, `value_text`.

### `theme` (waydock/companion_theme.py) — how every app gets the dock palette
- `attach(&window, None) -> WindowTheme` (idempotent; call for every toplevel,
  dialog and popup window) or `attach_application(&app)` (all present and
  future windows). Follows `~/.cache/waydock/palette.json` (directory monitor
  + 2 s poll), picks the entry whose geometry equals the window's monitor,
  adds class `waydock-companion-<n>` and a provider at APPLICATION+30
  (square corners: extra provider at USER+3). No palette ⇒ app CSS untouched.
  Also drives compositor blur (`blur` module) when `apps_frosted`.
- Pure functions: `validate_palette(&Value) -> PaletteFile`, `palette_for_geometry`,
  `theme_css(scope, &MonitorPalette, is_dark, &ThemeOptions)`, `vivid_accent`,
  `on_accent`, `frost_background`, `window_controls_css(window_sel, fg, colored)`,
  `window_frame_css`, `window_glyph_svg`, `window_light_svg`, `square_corners_css`,
  `is_dialog` (dialogs ≥ 0.97 opacity), `PaletteClient` (shared watcher).

### `palette` (waydock/dock_style.py + gtk_color_scheme.color_css)
`color_palette(is_dark, Option<wallpaper rgb>) -> Palette {background, foreground, accent}`,
`surface_palette(.., Option<manual bg>)`, `wallpaper_accent`, `contrast`,
`relative_luminance`, `mix` (Python half-even rounding), `rgb_to_hls/hls_to_rgb`,
`surface_alpha` (light surfaces ≥ 0.85), `background_css(is_dark, &PanelStyle)`,
`popup_css(..)`, `color_css(is_dark, &palette, colored_buttons)` (GTK named +
libadwaita colours), `parse_hex`, `ROUNDED_CONTROLS`.
Tokens: pt fonts, 12 px surfaces / 8 px controls, white knobs, accent HLS
saturation ≥ 0.45 with ≥ 4.5:1 contrast, blue fallback for colourless input.

### `blur` (background_blur.py) = re-export of `suite_wayland::blur`
`attach(&widget, target_class, include_root, include_popovers) -> BlurBinding`,
`.configure(enabled, strength 0..100, rounded, target, include_root)`,
`.refresh()`, `.surface_count()`, `.geometry(i)`, `.close()`. The former C
bridge (native/background_blur.c) is gone; there is one implementation, in
suite-wayland (see below). suite-gtk's build.rs now only fixes the link order.

---

## suite-power (power_backend.py, power_charge_limits.py; `gtk` feature: power_style.py, power_battery_ui.py)

Used by power-manager (`Mode::Full`) and battery-manager (`Mode::BatteriesOnly`).
Without the `gtk` feature it links only GLib/GIO (status-stream binaries).

- `backend::PowerBackend::new(on_changed, on_error, Options {mode, connector,
  scheduler, inspect, session_id})` (`Options::default()`: system bus,
  `GlibScheduler`, sysfs/KAuth inspection, `$XDG_SESSION_ID`), or
  `PowerBackend::batteries_only(on_changed, on_error)` (= Python
  `batteries_only=True`: UPower only, 120 s fallback refresh). `Mode::ProfilesOnly`
  (Rust-only) serves the power status stream.
- `trait PowerSystem` (`state() -> Ref<BackendState>`, `start`, `close`,
  `set_profile`, `idle_inhibited`, `sleep(action, interactive, guard)`,
  `explicit_power`). `BackendState`: ready/profiles/power/sleep readiness, busy,
  error, profiles, active_profile, performance_degraded, has_battery, on_battery,
  percentage, warning_level, batteries (normalized dicts), power_source_known,
  capabilities, charge_limit.
- `trait BatteryModel` (what battery UIs/indicators read: `power_ready`,
  `power_source_known`, `on_battery`, `has_battery`, `percentage`, `busy`, `error`,
  `batteries`, `charge_limit`, `set_charge_thresholds(path, enabled)`,
  `set_charge_limit(&Value)`), implemented by `PowerBackend` and UI fakes.
- Inherent: `ready()`, `warning_level()`, `attach(bus)`, `refresh*()`,
  `subscribe()`, `has_source(key)`, `owner_changed(name)`, `power_signal(&Signal)`,
  `bus_closed()`, `publish_batteries()`, `power_devices()`/`set_power_device()`,
  `state_mut()`, `set_charge_thresholds(&Value, &Value)` (Python argument checks),
  `set_charge_limit`. Constants `PPD_ENDPOINTS`, `UPOWER`, `UPOWER_DEVICE`, `LOGIN`,
  `SLEEP_METHODS`, `CAPABILITIES`; `inhibited(rows, subjects)`.
- `devices` — `device_batteries(devices, busy_path)`, `is_system_battery`,
  `system_batteries`, `battery_percentage` (energy-weighted), `device_identity`,
  `device_kind`/`battery_state`/`battery_level`/`battery_warning`, `bounded`.
- `charge_limits` — `inspect_limits(devices, sysfs_root, helper_files)` →
  (UI state, `Records`), `initial_state`, `arguments_for`, `identity`,
  KAuth `parameters(action, args)`, `encode_arguments`, `decode_reply`,
  `reply_blob`, `qstring`, `parse_int` (Python `int()`), `ENDPOINT`, `SUPPLIES`.
- `ui::style` (`STYLE_CSS`, `style`, `margins`, `icon`, `set_icon`, `icon_names`,
  `label`, `install_style`, `ICON_FALLBACKS`) and `ui::battery_ui`
  (`BatterySection`, `BatteryRow`, `ChargeLimitControls`, `time_label`,
  `level_label`, `status_label`, `weak_fn`).

---

## suite-wayland (waydock toplevel_helper.c / workspace_helper.c, wayshell overview_capture.c, shared/background_blur.c)

wayland-client 0.31 on the **system backend** (libwayland-client), so the same
code can run on its own connection or on GTK's `wl_display`. Protocols come from
wayland-protocols / wayland-protocols-wlr (XMLs identical to ~/btmanager's
vendored copies); the private `waydock_blur_manager_v1` is generated with
wayland-scanner from `protocols/waydock-blur-v1.xml`. Features: `glib`
(default, GSource), `capture-png` (default, cairo PNG thumbnails for
`wayshell-overview`), `gtk` (gdk4-wayland: blur, GTK-display clients, textures).
Build needs wayland-client (and cairo for `capture-png`) dev files.

### `model` — pure state, unit-testable with mocked events
`Model` holds outputs, foreign-toplevel windows, workspace groups/workspaces and
ext-foreign-toplevel handles, fed by methods named after the protocol events
(`output_added/geometry/mode/scale/name/description/done`, `logical_position/size/done`,
`output_removed`, `toplevel_new/title/app_id/output_enter/leave/state/parent/done/closed`,
`group_new/…`, `workspace_new/stable_id/name/coordinates/state/capabilities/removed`,
`workspaces_done`, `publish_workspaces`, `listed_new/title/app_id/identifier/done/closed/forget`).
Ordering is the C helpers': windows/outputs/membership newest first, workspaces
and groups oldest first; one id counter for windows, one shared by groups and
workspaces (`u64`, never reused within a connection).
- Queries: `outputs()`, `output(global)`, `output_by_name(name)`, `Output::geometry()`
  (xdg-output logical size, else pixels/scale with 90°/270° swap), `windows()`
  (published only), `all_windows()`, `window(id)`, `window_outputs(&w)`,
  `workspaces()`, `workspace(id)`, `groups()`, `group_outputs(&g)`,
  `workspace_outputs(&ws)`, `listed()`, `listed_toplevel(key)`.
  `Window {id, app_id, title, activated, fullscreen, minimized, maximized, parent (0 = none), outputs, published}`,
  `Workspace {id, stable_id, name, state, capabilities, coordinates, group}` + `active()/urgent()/hidden()/can_activate()`.
- `take_events() -> Vec<model::Event>`: `Output(global)` (then a `Window` per
  published window), `OutputRemoved`, `Window(id)`, `WindowClosed(id)`,
  `Workspaces` (atomic, after manager `done`), `Listed(key)`, `ListedClosed(key)`,
  `Error {message, fatal}`.
- `render = Some(Render::Toplevel | Render::Workspaces)` renders helper JSON lines
  at event time (`take_lines()`); `window_json`, `output_json`, `workspaces_json`,
  `error_json`, `json_string` reproduce the C byte format (only `"`/`\` escaped,
  < 0x20 as `\u00XX`, strings cut at 16384 bytes).

### `client` — protocol glue
- `Client::connect(Protocols) -> Result<Client, ConnectError>` (`connect()` follows
  `wl_display_connect(NULL)`: `WAYLAND_SOCKET`, else `WAYLAND_DISPLAY` default
  `wayland-0` under `XDG_RUNTIME_DIR`), `Client::new(Connection, Protocols)` (own
  event queue on any connection), `Client::for_gdk_display(&gdk::Display, …)` (`gtk`).
  `Protocols::{TOPLEVELS, WORKSPACES, CAPTURE, OUTPUTS, ALL}` or a struct literal
  (`toplevels` = wlr manager ≤ v3 + wl_seat ≤ v7; `workspaces` = ext-workspace v1;
  `xdg_output` ≤ v3; `capture` = wl_shm + ext list + copy-capture + both source managers;
  wl_output ≤ v4 is bound whenever any is set).
- Blocking drive (helpers, tests): `roundtrip()`, `dispatch_pending()`,
  `read_and_dispatch()`, `wait(Option<Duration>)`, `wait_until(timeout, pred)`,
  `flush()`, `fd()`, `take_events() -> Vec<Event>` with
  `Event::{Changed(model::Event), Captured(CaptureId, Result<Frame, CaptureError>), Disconnected(String)}`.
- `Client` derefs to `State`: `model`, `has_toplevels/has_seat/toplevel_version/has_workspaces/has_xdg_output`,
  `can_capture_toplevels/outputs`, `wl_output(global)`.
  Windows: `activate(id)` (unminimizes first; needs a seat), `set_minimized(id, bool)`,
  `set_maximized(id, bool)`, `set_fullscreen(id, bool, Option<output name>)` (v2+),
  `close(id)`, `set_rectangle(id, &WlSurface, x, y, w, h)` (surface must be on the
  same connection: use a GTK-display client + `gtk::gtk_wl_surface`).
  Workspaces (each followed by `commit`): `activate_workspace(id)` (only
  activatable, non-hidden), `deactivate_workspace`, `remove_workspace`,
  `assign_workspace(id, group)`, `create_workspace(group, name)`.
  Errors are `ActionError` whose `Display` is the helpers' English text
  ("That window is no longer available.", …) — translate with `tr!` in UIs.
- Capture: `listed_for_window(window id) -> Option<HandleKey>` (unique
  app_id + title match, the overview's identity rule), `capture_listed(key, paint_cursors)`,
  `capture_output(name, paint_cursors)` → `CaptureId`; one shm buffer per
  capture (ARGB8888 preferred over XRGB8888, `prefer_format`; ≤ 16384², ≤ 32 Mpx),
  result `Frame {width, height, stride, format, transform, data}` (copied out,
  little-endian BGRA), `Frame::to_texture()` (`gtk`), `cancel_capture(id)`,
  `capture_pending(id)`. `keep_closed_listed` / `listed_limit` tune the handle list.

### `source::GlibClient` — GLib main loop
`GlibClient::attach(client, Option<&MainContext>, |&GlibClient, Event| …)`;
`with(|c| c.activate(id))` (runs + flushes), `borrow()`, `borrow_mut()`,
`set_handler`, `close()`. A custom GSource on the display fd: prepare flushes and
dispatches already-queued events into the model (registering a waker that wakes
the context when another reader routes events to us), dispatch reads the socket
non-blocking and then calls the handler per event **with no borrow held**.
Reading only in the dispatch phase makes sharing GTK's display deadlock-free
(GDK prepares/reads within prepare→check).

### `gtk` (feature `gtk`)
`gdk_connection(&gdk::Display) -> Option<Connection>` (gdk4-wayland's cached
connection on GTK's wl_display), `gtk_wl_surface(&impl IsA<Native>) -> Option<WlSurface>`,
`Client::for_gdk_display`, `Frame::to_texture`.

### `blur` (feature `gtk`; re-exported as `suite_gtk::blur`)
Rust port of background_blur.c/.py with the same API (see suite-gtk above).
Per GdkDisplay it binds `ext_background_effect_manager_v1` v1 and
`waydock_blur_manager_v1` v1 on a private queue of GTK's connection; per mapped
native (root via `target_class`/whole window, popovers via their `contents`) it
creates one effect object (one owner per wl_surface, data key
`waydock-background-blur-owner`), a rounded region (`rounded_region_rects`) and
radius `round(strength·0.4)`; never commits — `queue_render()` lets GTK's next
frame carry it. `compositor_supports_blur(&display)`, `compositor_supports_radius`.

### Helper-compat binaries (`compat`, `src/bin/`)
Byte-for-byte drop-ins for the C helpers (verified against them in the headless
test): `waydock-toplevel [--snapshot]` and the identical `wayshell-toplevel`
(events `ready/output/output-removed/window/closed/error`; stdin
`activate|minimize|unminimize|close <id>\n`, sscanf rules, 4096-byte lines),
`waydock-workspaces [--snapshot]` (`state`, `ready`, `unsupported`, `error`; stdin
strict `activate <id>\n`, 128-byte lines, NUL rejected), `wayshell-overview [connector]`
(`desktop` frame, `identities`, `window` frames; cairo-bilinear PNG ≤ 640×400 /
1600×1000, base64; 1.2 s per frame, 10 s total). Same exit codes. Install them
where the C builds went (installer step, not done here):
`~/.local/share/waydock/{waydock-toplevel,waydock-workspaces}`,
`~/.local/share/wayshell/{wayshell-toplevel,wayshell-overview}`,
`~/.config/waybar/scripts/{notification-manager,on-screen-keyboard}/waydock-toplevel`.
Consumers unchanged: waydock window_backend.py/workspace_backend.py, wayshell
switcher/overview.py/capture_output.py/capture_icons.py (`--snapshot`), Rust
notification-manager (read-only) and session-manager (`close N`).

### labwc notes
labwc 0.20.2 offers everything above. Its pixman renderer offers only XRGB8888
for captures (so headless never exercises the ARGB path), and after a window
leaves fullscreen its toplevel capture keeps the output's size (the C helper
sees the same).

---

## Tests
`cargo test` (suite-power: test_power_backend / test_power_charge_limits for both
modes; suite-core: unit tests ported from test_suite_i18n / test_clock_preferences /
test_service_settings (private `gio::TestDBus`) / test_color_palette /
test_companion_theme / test_gtk4_utils icon fallbacks; byte-for-byte parity of
generated CSS/SVG with Python in `suite-gtk/tests/python_parity.rs`).
suite-wayland: `tests/model.rs` (mocked-event state tracking, JSON bytes,
request parsing, PNG thumbnails) under plain `cargo test`;
`crates/suite-wayland/tests/run_headless.sh` builds the original C helpers from
~/btmanager into `$TMPDIR/c-helpers` and runs `tests/headless.rs` in a private
labwc (`tests/headless.sh <cmd>` runs any command there, 3 desktops): GTK
windows observed/activated/minimized/maximized/fullscreened/closed, toplevel and
output capture, workspaces, GTK-display client + `set_rectangle`, blur, and C vs
Rust helper output parity.
Widget checks (popup, anchors, app menu, live theme, OSD, CSS parsing):
`crates/suite-gtk/tests/run_headless.sh` — private nested headless labwc per
`tools/headless.md`; under plain `cargo test` that binary skips itself.

Not ported (not needed by components yet or tool/installer-only): wallpaper_chooser,
wallpaper_import, application_audio, capture_selection, the `install_*.py`
scripts, Waydock's static `dock_style.CSS/LIGHT_CSS`, and the dock-side writers
(`palette_state.PaletteState`, `gtk_color_scheme.GtkColorScheme` file management).
