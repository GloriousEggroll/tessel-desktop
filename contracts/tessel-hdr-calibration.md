# tessel-hdr-calibration — external contract

Source: `~/tessel-bluetooth/tessel-hdr-calibration` (`hdr_calibration.py` 502 LOC GUI, `calibration.py` 256 storage +
labwc integration, `correction.py` 77 .cube import, `hdr-pattern.c` 279 native Wayland HDR10 test
pattern client built by `build.py`, `install.py`). Uses way-displays' `color_config.py` (status keyfile,
`atomic_write`, `_locked`, `parse_colors`, `rollback`) and `display_style.py` (`style_window`).

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installed files | `~/.local/share/tessel-hdr-calibration/{calibration.py,hdr_calibration.py,correction.py,app_menu.py,gtk4_utils.py}` (0644) + `hdr-pattern` (0755, from `build/hdr-pattern`); also re-installs `~/.local/share/way-displays/color_controls.py` and `~/.local/share/tessel-settings/settings_search.py`. Requires `~/.local/share/way-displays/{color_config.py,display_style.py,suite_i18n.py}` (imports resolve via `sys.path`/`HERE.parent/'way-displays'`). Backup `~/.local/state/tessel/backups/hdr-calibration-<stamp>/` + `manifest.json` | install.py |
| Launcher | `~/.local/bin/tessel-hdr-calibration` (0755) = `#!/bin/sh\nexec /usr/bin/python3 <shlex-quoted ~/.local/share/tessel-hdr-calibration/hdr_calibration.py> "$@"\n` | install.py:24-25 |
| .desktop | `~/.local/share/applications/org.desktop.HDRCalibration.desktop`: `Name=HDR Calibration`, `Comment=Set up HDR highlight limits and SDR reference white for each monitor`, `Exec=<launcher>`, `Icon=video-display`, `Categories=Settings;HardwareSettings;GTK;`, `Keywords=HDR;calibration;display;monitor;luminance;nits;color;`, `StartupNotify=true`, `Terminal=false`. **No `StartupWMClass`**; the Wayland app_id is the GApplication id `org.desktop.HDRCalibration` (= desktop id basename) | org.desktop.HDRCalibration.desktop |
| Also installed by | `shared/install_gtk4.py` (`hdr_installer.runtime_files(home)`), `install_all.py` step `hdr` | |
| Launched by | way-displays `color_controls.py::_calibrate_hdr`: `python3 <way-displays>/../tessel-hdr-calibration/hdr_calibration.py --output <connector>` (`start_new_session=True`; Rust way-displays: same, override `WAY_DISPLAYS_HDR_CALIBRATION`); Global Settings `desktop_pages.hdr_calibration_command`: `~/.local/bin/tessel-hdr-calibration` if a file, else `python3 <repo>/tessel-hdr-calibration/hdr_calibration.py`, plus `--output <first HDR-capable connector>`; settings_search entries `hdr_calibration` ("Calibrate HDR…") and `sdr_editing_hdr` ("SDR editing with HDR…") | |
| GApplication id | `org.desktop.HDRCalibration`, `NON_UNIQUE` (every launch is a new process + window); `app.run([argv0])` (options never reach GApplication); `activate` → `Window(app, args.output)`, present; `--sdr-editing` → preset index 1 + focus; `--smoke-test` → close + quit after 300 ms | hdr_calibration.py:482-498 |
| CLI | argparse: `--output OUTPUT`, `--sdr-editing`, `--smoke-test`, `-h`; errors exit 2 (`usage: hdr_calibration.py [-h] [--output OUTPUT] [--sdr-editing]\n                          [--smoke-test]`) | |

## 2. IPC
No D-Bus. Signals: `SIGHUP` to the compositor PID from the status file (labwc reload) after writing rc.xml, and
again after a rollback. Child process `hdr-pattern CONNECTOR black|peak|fullFrame|white|sdr-srgb|sdr-709 NITS`
(stdout JSON `{"encoding": "BT2020-PQ-10"|"SRGB-10"|"BT709-BT1886-10", "nits": N}` on exit 0; stderr text otherwise;
exit 2 = cancelled); terminated (SIGTERM) on window close / unavailability.

## 3. Files

| Path | Format | Notes |
|---|---|---|
| `$XDG_RUNTIME_DIR/labwc-color-$WAYLAND_DISPLAY.ini` (read) | labwc status keyfile (see way-displays contract): `[compositor] pid, hdr_calibration_supported, hdr_correction_supported`; per output `enabled, hdr, bit_depth, display_id (16 hex, ≠0), description, hdr_peak_nits, hdr_max_frame_average_nits, hdr_reference_white_nits, hdr_target_{black,peak,full_frame}_nits, hdr_calibration_active, hdr_correction_{active,pending,error,path,sha256}` | `color_config.read_status()` |
| `/proc/<pid>/cmdline`, `/proc/<pid>/cwd` (read) | labwc args: rc.xml = `-c/--config[=]/-cFILE`, else `-C/--config-dir[=]/-CDIR` + `rc.xml`, else `$XDG_CONFIG_HOME/labwc/rc.xml`; refuses non-`labwc` argv0 or `-m/--merge-config`; relative paths against cwd | calibration.config_path |
| rc.xml | managed block `<!-- tessel-hdr-calibration begin -->\n<hdrCalibration>\n  <output name="C" displayId="…" black="…" peak="…" fullFrame="…" white="…" pictureMode="…" [correction="…" correctionSha256="…"] />\n</hdrCalibration>\n<!-- tessel-hdr-calibration end -->\n` inserted just before `</labwc_config>`/`</openbox_config>` (self-closing root expanded); values `format(v, '.9g')`; reset writes `<output name="C" enabled="false" />`; other outputs of the block kept; ElementTree serialisation (`ET.indent` 2 spaces, `" />"`, attribute escaping); malformed/duplicate block or invalid XML → ValueError, nothing written; same `.<rc>.color.lock` flock as the colour controls; backup `$XDG_STATE_HOME/tessel/backups/hdr/<time_ns>-rc.xml`; mode kept (new 0600); missing file starts from `<labwc_config>\n</labwc_config>\n` | calibration.write_config |
| `$XDG_CONFIG_HOME/tessel-hdr-calibration/profiles.json` | `json.dumps({"version": 1, "profiles": [...]}, indent=2) + "\n"`, 0600, locked; one entry per (displayId, connector, mode, preset), newest appended with `saved` (UTC ISO 8601); every entry re-validated on read | calibration.save_profile/read_profiles |
| `$XDG_CONFIG_HOME/tessel-hdr-calibration/corrections/<sha256>.cube` | imported correction, 0400 | correction.import_cube |
| `$XDG_STATE_HOME/tessel-hdr-calibration/reports/<time_ns>.json` | calibration summary (`version, created, connector, displayId, pictureMode, requested, compositor, active, software_configuration_matches, correction_file_matches, meter_verification: "not performed", application_preview_verification: "not performed", measured_luminance: null, measured_color_error: null, physical_color_accuracy: "unverified"`), indent 2 | calibration.workspace_report |
| `$XDG_STATE_HOME`/`$XDG_CONFIG_HOME` | Python uses `os.environ.get(X, Path.home()/…)` (set-but-empty is honoured as relative) | |

Profile (`validate`): `version == 1`; `displayId` 16 lowercase hex ≠ 0; `connector` non-empty; `mode` 1–100 chars
(stripped), single line; `method` `visual|meter-readings` (meter needs `instrument`); `black, peak, fullFrame, white`
floats: finite, `0 ≤ black < white`, `20 ≤ white ≤ 500`, `40 ≤ peak ≤ 10000`, `white ≤ fullFrame ≤ peak`; `preset`
`custom` | `sdr-video` (white 100) | `sdr-photo` (white 120); optional `correction {path (absolute), sha256 (64 hex),
encoding "BT2020-PQ", size}`. Messages are the English ValueError texts in calibration.py (shown untranslated).

Correction cube (`read_cube`): O_NONBLOCK regular file 128 B–16 MiB, no NUL, UTF-8; `# labwc-hdr-pq-correction-v1`,
`# display-id: <id>`, `# picture-mode: <mode>` (must match, once each), optional `TITLE` before samples,
`LUT_3D_SIZE 2|17|33|65` once, optional `DOMAIN_MIN 0 0 0` / `DOMAIN_MAX 1 1 1` before samples, exactly size³ finite
RGB triplets in [0,1]; sha256 of the file bytes.

## 4. Apply transaction (`calibration.apply`)
`session()` (needs `hdr_calibration_supported=true`, else "Start a session with the updated labwc compositor to use HDR
calibration.") → validate → correction: compositor `hdr_correction_supported` + file sha unchanged ("…has changed.
Import it again.") → `check_monitor` (enabled, hdr, bit_depth 10, stable identity, same identity) → write rc.xml →
SIGHUP → poll status every 0.1 s for 8 s: pid change, identity change, `hdr_correction_error` abort; success when
`matches()` (calibration active + targets within rel 1e-6/abs 1e-5, correction committed or absent; reset = inactive +
white 203) → save profile → return backup. Any failure → `rollback` (refuses if rc.xml changed meanwhile) + SIGHUP,
re-raise; timeout → "The compositor did not apply the HDR setup. The previous configuration has been restored.".

## 5. UI
- Window `Gtk.ApplicationWindow` title `HDR Calibration`, default 760×840, plain `Gtk.HeaderBar`, `display_style.style_window`
  (class `way-displays`, way-displays CSS at APPLICATION+10, companion theme), class `tessel-hdr-calibration`, STYLE at APPLICATION+31
  (`.hdr-section .hdr-section-title .hdr-field-title .hdr-detail .hdr-empty .hdr-empty-title .hdr-empty-icon`, plus
  `.settings-card .page-title .primary`), `install_app_menu(name='HDR Calibration', icon='preferences-desktop-display',
  comments='Set HDR brightness limits and SDR white for each monitor')`.
- Stack `empty` (icon `monitor-symbolic|video-display-symbolic|preferences-desktop-display-symbolic` 64 px, "HDR calibration is
  unavailable", reason) / `main` (scrolled: title, intro, sections Monitor (DropDown + capability), Monitor picture mode
  (Entry "Current HDR mode", saved DropDown "New setup" + `mode — preset`), usage preset DropDown, HDR output correction
  (label, Import HDR correction… [FileChooserNative, filter `*.cube`], Clear correction), Reference patterns (sRGB / Rec.709),
  Brightness limits (method DropDown Visual estimates / Measured luminance — manual readings, instrument Entry, 4 spin rows
  with ranges black 0–19 step .001 4 digits, peak 40–10000 step 10, fullFrame 20–10000 step 10, white 20–500 step 5; "nits";
  "Show pattern"), actions Save a calibration summary / Restore display defaults (confirm AlertDialog) / Save and apply
  (`.primary`), selectable outcome label.
- `refresh()` now and every 2 s: enabled outputs sorted; selection kept / `--output` preferred; identity change reloads;
  sensitivity off while a worker or pattern runs; correction import needs `hdr_correction_supported`; capability text
  `{display}\nHDR is on · SDR white is {white:g} nits · {state}`; any error → empty page + controls insensitive + pattern child terminated.
- Close: refused with "Finishing the HDR configuration update…" while applying; otherwise stop timer, destroy chooser,
  terminate pattern child (wait 2 s).
- All user-visible strings via `suite_i18n.translate` (list in hdr_calibration.py).

## 6. Parity tests
`test_calibration.py` (config path precedence; validate limits/provenance; check_monitor; independent monitors keep
SDR/VRR/comments; rollback + concurrent change; saved modes separate + 0600; malformed config writes nothing; failed
apply restores + 2 SIGHUPs; success requires committed values), `test_sdr_workspace.py` (cube import + 9 strict
rejections; presets separate; correction must be committed / reset; mutated import cannot apply; report records
"not performed"/unverified), `test_gui.py` (private labwc: selection, saved modes, presets, method provenance,
apply via mock, SDR gating, correction kept, unknown active correction blocks, old compositor gating, `--smoke-test`),
`test_patterns.py` (hdr-pattern against labwc-color's wlroots test server — C helper only).
