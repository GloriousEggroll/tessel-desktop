# Foundation crates: `suite-core` and `suite-gtk`

Rust ports of `~/btmanager/shared` (plus Waydock's palette theme). Behaviour,
file paths, CSS class names and strings match the Python modules; read this
instead of the source. Depend on them from a component with:

```toml
[dependencies]
suite-core.workspace = true
suite-gtk.workspace = true          # GUI components only
gtk.workspace = true                # = gtk4 0.11, feature v4_22 (system GTK 4.22)
gtk4-layer-shell.workspace = true   # if you use layer shell directly
```

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

### `blur` (background_blur.py; C bridge compiled in by build.rs)
`attach(&widget, target_class, include_root, include_popovers) -> BlurBinding`,
`.configure(enabled, strength 0..100, rounded, target, include_root)`,
`.refresh()`, `.surface_count()`, `.geometry(i)`, `.close()`. Needs
`wayland-scanner` + gtk4/wayland-client dev files at build time only.

---

## Tests
`cargo test` (unit tests ported from test_suite_i18n / test_clock_preferences /
test_service_settings (private `gio::TestDBus`) / test_color_palette /
test_companion_theme / test_gtk4_utils icon fallbacks; byte-for-byte parity of
generated CSS/SVG with Python in `suite-gtk/tests/python_parity.rs`).
Widget checks (popup, anchors, app menu, live theme, OSD, CSS parsing):
`crates/suite-gtk/tests/run_headless.sh` — private nested headless labwc per
`tools/headless.md`; under plain `cargo test` that binary skips itself.

Not ported (not needed by components yet or tool/installer-only): wallpaper_chooser,
wallpaper_import, application_audio, capture_selection, the `install_*.py`
scripts, Waydock's static `dock_style.CSS/LIGHT_CSS`, and the dock-side writers
(`palette_state.PaletteState`, `gtk_color_scheme.GtkColorScheme` file management).
