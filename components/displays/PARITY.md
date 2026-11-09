# tessel-displays — parity with contracts/tessel-displays.md

Crate `components/displays`, binary `tessel-displays` (= `~/.local/share/tessel-displays/tessel-displays`,
GApplication `org.tessel.Displays`). Tests: `cargo test -p tessel-displays` (29 unit tests),
`tests/run_headless.sh` (22 GTK checks in a private nested 2-output headless labwc),
`tests/measure_headless.sh` (Rust vs Python, private labwc + `tests/fake_daemon.py` socket).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Item | | Test |
|---|---|---|
| App id `org.tessel.Displays`, activate → `GET` then a new window each time; argv handled by GApplication only | ✅ | measure_headless (window maps, only `OP: GET` sent) |
| Installer `install_color.py`, launcher, .desktop | ❌ (installer step, not in crate) | — |

## §2 IPC
| Item | | Test |
|---|---|---|
| Socket lookup (`tessel-displays.sock`, `.$XDG_VTNR.sock`, `/tmp`), `OP: <OP>\n<payload>`, read to EOF in 8192 chunks, 16 MiB cap, 5 s timeout, missing socket → message + `""` | ✅ | `ipc_round_trip_with_private_fake_daemon` |
| Payload = `yaml.safe_dump({"CFG": cfg}, sort_keys=False).rstrip()` byte-identical for plain/quoted scalars, floats, `null`, indentless lists | ✅ | `compat::yaml::tests`, cross-checked against PyYAML |
| `get_config_data` (needs `STATE` mapping, else `{}`) | ✅ | ipc test |
| Save: CFG_DEL (transforms→STANDARD, re-enabled, auto-scaled SCALE, VRR desc+name, cleared callback) → CFG_SET → CFG_WRITE → XWayland primary now + 500 ms → VRR-only refresh → layout reload 250 ms; VRR_OFF only for changed monitors; FLIPPED-STANDARD→FLIPPED; ARRANGE/ALIGN/ORDER from shapes; CFG key order | ✅ | `vrr_changes_in_two_stale_editors…`, `automatic_scaling…`, `transforms_disabled_callback_and_mode_plan` (unit + full-window UI versions), `apply_runs_primary_command…` |
| Callback/notify switch (`notify-send …` default, CFG_SET / CFG_DEL) | ☑ | (widgets hidden as in Python) |
| Brightness D-Bus client (`org.tessel.BrightnessManager`, refresh_on_connect, busy accounting, 180 ms debounce keyed by display id) | ✅ | 11 BrightnessControls UI checks (fake client) |
| Layer-shell identify overlay (OVERLAY, exclusive −1, TOP+LEFT, keyboard NONE, monitor by `desc in description`, empty input region) | ✅ | `output_corner_ignores_reserved_top_and_left_and_stays_above_fullscreen` (grim), `window_contract` |

## §3 Files
| Item | | Test |
|---|---|---|
| labwc `environment` block (add after blank line, replace, remove, mode kept / new 0600, `(ok, err, changed)`) | ✅ | `environment_block_add_update_remove`, `environment_parse_…` (bytes match Python) |
| labwc `xinitrc` block (all 6 Python cases incl. adopting standalone `xrandr --output X --primary`, 0700, literal connector, failed replace) | ✅ | `xinitrc_*` (6) |
| rc.xml path from `$LABWC_PID` cmdline; HDR block read/write (`html.escape`, sorted, before root close) | ✅ | `hdr_overrides_read_and_write_only_managed_block` (bytes match Python) |
| `color_config`: parse/read, ICC validation (O_NONBLOCK, size, header, lcms2 via dlopen), status keyfile (uid, pid alive, version, LABWC_PID, `-m`), write_colors (lock, backup, merge, escape, append last, self-closing root), rollback | ✅ | `color_*` (9, all 8 Python cases + self-closing root) |

## §4 UI
Window (title, ≤980×780 default, headerbar, `.page-title/.subtitle`, Apply `.primary` insensitive until a change, scroll, frame 300 px), MonitorLayout (sizes ×0.08, ROW/COLUMN placement, snap 20/30, recenter, flash 150 ms, drag, click→tab, add/remove on enable), MonitorWidget (all rows, rotation radio buttons, flip, scale 1–2.5 step 0.125 with `g×` marks, auto scale, VRR from DESIRED, HDR sensitivity), ColorControls, BrightnessControls, CSS verbatim (`display_style.CSS`, overlay and layout CSS), companion palette: ✅ `ui_headless` (22 checks) and measure_headless screenshot **byte-identical** to the Python window.

## Deviations ⚠
1. YAML: multi-line strings (only a CALLBACK_CMD carrying the primary block) are emitted double-quoted instead of PyYAML's folded single quotes — same value after parsing. Replies are parsed with yaml-rust2 (YAML 1.2: `yes`/`no` stay strings).
2. Python crashes (ValueError) when no monitor is enabled/known (e.g. daemon not running) and shows no window; Rust shows the window with an empty arrangement.
3. The brightness client is built in (copy of tessel-brightness's client), so "Brightness service client is not installed." cannot occur.
4. `Calibrate HDR…` runs `/usr/bin/python3 <exe>/../../tessel-hdr-calibration/hdr_calibration.py` (override `TESSEL_DISPLAYS_HDR_CALIBRATION`).
5. XML/Unicode error message texts come from roxmltree/Rust, not ElementTree/Python.
6. A daemon I/O error during Apply aborts the rest of Apply with a printed error (Python raised a traceback at the same point).

## Gaps ❌
- **Global Settings embeds the Python module** (`TesselDisplaysApp(embedded=True)`, `CONFIG_DATA`, widget tree): the Python files must stay installed until GS is ported. `embedded` exists in the Rust API but nothing consumes it.
- `install_color.py` / `install_waybar.py` steps are not part of this crate.

## Beyond the Python tool

- Apply is a trial: settings go live but are not saved, and a question counts down from 10 seconds. Only Keep Changes saves (`CFG_WRITE`). Revert, Escape, closing the question or the window, or no answer put back the daemon settings and labwc's `rc.xml`, `environment` and `xinitrc`. A failed Apply reverts without asking. Tests: `ui_headless`, `tests/run_trial_headless.sh` (real daemon).
- Brightness, contrast and dimming apply as the slider moves and are not part of the trial.
- `CFG_DEL` with `ORDER` clears the saved order, so a revert can return to having none.

Intentional difference from way-displays: automatic scale aims for 135 dpi under 20 inches and 110 dpi above when `AUTO_SCALE_DPI` is 96, and stops at 2.5 without `AUTO_SCALE_MAX`. way-displays divided by 96, which made laptop panels far too large (2.75 on a 13 inch 2880×1920).
