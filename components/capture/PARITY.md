# tessel-capture — parity with contracts/tessel-capture.md

Crate `components/capture`. It builds the binary `tessel-capture` (GApplication `org.tessel.Capture`) and the library `capture_manager`.

Tests:
- `env -u TMPDIR cargo test -p tessel-capture` runs 30 unit tests.
- `tests/run_headless.sh` runs 21 GTK/D-Bus checks. They run in a private nested labwc with two headless outputs and a private bus, and they use fakes: the ScreenCast portal (which also stands in for the chooser and calls the broker), the Notifications server, Tessel Dock, FileManager1 and the PipeWire feeds. pactl, wpctl, wl-copy and labwc are logging fakes. ffmpeg is wrapped so that it refuses `pulse`. The private bus has no service directories, so nothing can be D-Bus-activated.
- `tests/measure_headless.sh` compares Rust against Python.

Legend: ✅ implemented and tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Item | | Test |
|---|---|---|
| GApplication `org.tessel.Capture`, HANDLES_COMMAND_LINE, prgname = app_id, primary returns the exit status to remotes | ✅ | `test_command_line_binary_contract`, `test_workspace_and_armed…` (`--lock-shot`/`--stop` from a second process) |
| Launcher `/usr/local/bin/tessel-capture`, .desktop files, StartupWMClass | ❌ installer step (contract §1); `measure_headless` checks that the window maps with app_id `org.tessel.Capture` | — |
| Global Settings `capture_settings.build_page` | ☑ Rust `settings::build_page` (switch rows, `.subtitle .dim-label`, `.title-2`). **Global Settings still imports the Python module**, so keep `capture_settings.py` installed | `test_settings_merge_…` |

## §2 CLI
| Item | | Test |
|---|---|---|
| argparse flags, abbreviations, `=` values, usage/help text, error texts, exit 2 | ✅ | `args::tests::parses_like_argparse`, `test_command_line_binary_contract` |
| `--check` JSON (`{"ready": …}` key order, dlerror text when the library is missing), exit 0/1 | ✅ | `test_command_line_binary_contract` |
| `--capture X` (quick, never presents or hides, screenshot folder while the recorder is shown, refuses while busy) | ✅ | `test_shortcut_dispatch_never_presents_controls`, `test_screenshot_shortcut_preserves_open_recorder…` |
| `--start-recording [--audio-preset]`, `--toggle-recording`, `--stop` | ✅ | `test_ui_recording_finishes_a_portal_stream` |
| `--record` / `--screenshot` / none | ✅ | `test_shortcut_dispatch_never_presents_controls` |
| `--lock-shot` (0 if armed, else 1) | ✅ | `test_workspace_and_armed_capture_reopen_without_new_consent` |

## §3 Actions
| Item | | Test |
|---|---|---|
| `lock-shot(d)` over `org.gtk.Actions` on `/org/tessel/Capture` (the tessel-lock call) | ✅ | `test_workspace_and_armed…` |
| `open-saved` / `reveal-saved` / `copy-saved` / `edit-saved` (s); none of them create the capture window | ✅ | `test_notice_has_buttons…`, `test_missing_capture…`, `test_screenshot_notice_copies_and_edits…` |
| `show`, `stop` | ☑ (`app.stop` is used by recording GNotifications) | — |
| App menu `shortcuts` / `about` / `quit` (quit refuses while busy) | ✅ | `test_capture_window_has_standard_menu_and_quit` |

## §4 D-Bus
| Item | | Test |
|---|---|---|
| `org.tessel.Capture.Status` GetState `(sb)` and `Changed`; idle → preparing → recording → stopping → idle; armed flag | ✅ | `test_ui_recording…` (signals and GetState), `test_workspace_and_armed…` (`("idle", true)`), `tray::tests::states` |
| CaptureSelection broker: `Select` target/token rules, vanished output → `(true, "")`, `Selected` limits and observer, one registration per connection | ✅ | `compat::capture_selection::tests`; end to end in `test_direct_monitor_area_and_delay` (chooser preselected, no "unhandled") |
| ScreenCast flow: tokens, request/session paths, renamed handles, Response codes and messages, Session.Closed, 120 s timeout, `OpenPipeWireRemote` async plus a fresh sync fd per consumer, Close on Request and Session | ✅ (except the timeout ☑ and renamed handles ☑) | `test_portal_cancel_and_immediate_close`, `test_portal_frame_and_window_source` |
| Notifications `Notify` (app name, icon, actions, `desktop-entry`, `x-tessel-dock-output`, `x-tessel-dock-workspace`, 10 s), hold/release, ActionInvoked routing, NotificationClosed, 60 s expiry, GNotification fallback | ✅ (expiry ☑) | `test_routed_capture_notice…`, `test_screenshot_notice_copies…`, output and workspace hints in the portal checks |
| Tessel Dock `prepare-capture(b)` / `finish-screenshot` | ✅ | `test_shortcut_dispatch…`, `test_screenshot_shortcut…` |
| FileManager1 `ShowItems` via `Gtk.FileLauncher` for other file managers; `org.tessel.Files.desktop` gets `--select <uri>` | ✅ | `test_other_file_manager_receives_show_items`, `test_folder_starts_files_and_selects_screenshot` |

## §5 Files
| Item | | Test |
|---|---|---|
| `config.json` window preferences: key order, indent 2 + newline, `config.tmp` rename, legacy encoder migration, type checks | ✅ | `test_preferences_round_trip_and_legacy_encoder_migration` |
| `capture_settings` merge-write (mkstemp `.config-`, 0600) and type-checked get | ✅ | `settings::tests`, `test_settings_merge_…` |
| `shortcuts.json`, rc.xml backups, mode-preserving atomic writes | ✅ | `shortcuts::tests::test_transaction_and_reload_failure_rollback` |
| Default folders (special dirs + Screenshots/Recordings), absolute check, per-mode memory, file names with microseconds | ✅ | portal UI checks (private user-dirs.dirs) |
| No overwrite and private modes: screenshots `create_new` + 0600; recordings `.recording-XXXXXXXX` + umask 077 + hard link | ✅ | `test_no_overwrite`, `test_recording_finalizes…` (second recording to the same name fails and keeps the partial file), UI mode checks |
| PNG/JPEG with the embedded lcms2 sRGB profile (588 bytes, only the creation date differs), JPEG q95 4:4:4 | ✅ | `imaging::tests`, `test_ui_screenshot_and_area_selection` (PNG and JPEG) |
| Workspace composition with Lanczos | ✅ **byte-identical to Pillow** | `imaging::tests::lanczos_matches_pillow_byte_for_byte`, `test_workspace_negative_coordinates…`, workspace UI check |
| ⚠ PNG bytes | The pixels and the ICC profile match Python; the zlib stream and filters differ from Pillow's encoder | — |

## §6 External commands
| Item | | Test |
|---|---|---|
| ffmpeg argument lists (`color_filter`, `raw_input`, `encoder_arguments`, `recording_command`, probe) | ✅ exact text | `test_options_validation…`, `test_hdr_requires…`, `test_probe…` |
| Screenshot conversion, tone mapping, HLG, packed 10-bit, crop | ✅ (real ffmpeg on synthetic stdin) | `test_tonemap…`, `test_hlg…`, `test_native_packed_rgb…` |
| Encoder choice: Vulkan, then software fallback; diagnostics; timeouts | ✅ | `test_probe…`, `test_automatic_falls_back…` |
| All software codecs plus HDR metadata | ✅ | `test_all_codecs_and_hdr_metadata_with_real_encoders` |
| Recorder: wall clock, non-blocking frame writes, signature change, finalise, SIGINT/kill on error | ✅ | `test_recording_finalizes…`, `test_ui_recording…` |
| Vulkan encode device enumeration (dlopen libvulkan) | ✅ logic; hardware test is opt-in (`CAPTURE_TEST_VULKAN=1`) | `vulkan::tests`, `test_vulkan_codecs…` |
| pactl sources, presets, default devices | ✅ | `test_presets_use_default_devices…`, pactl fake in the UI |
| wl-copy (`--type image/png`) and the GTK clipboard fallback with a 120 s hold | ✅ wl-copy; ☑ fallback | `test_screenshot_completion…`, `test_screenshot_notice_copies…` |
| `labwc --reconfigure` after shortcuts are applied | ✅ (fake) | `test_shortcuts_apply_in_private_config_reloads_labwc` |
| `libcapture-pipewire.so` (dlopen next to the binary, then `/usr/local/lib/tessel/tessel-capture/`) | ☑ symbols resolve (`--check`) | ❌ **no real PipeWire stream was read**: feeds are faked in the tests |
| xkbcommon key validation (dlopen) | ✅ | `shortcuts::tests` |

## §7 Shortcuts
| Item | | Test |
|---|---|---|
| normalize/display, conflicts, removal of owned bindings, legacy `--screenshot` migration, idempotence, **rc.xml bytes identical to Python** | ✅ | `shortcuts::tests::*` |
| Window card (entries, Defaults, Apply, error class, app-menu shortcut refresh) | ✅ | `test_shortcuts_apply_in_private_config_reloads_labwc` |

## §8 UI
| Item | | Test |
|---|---|---|
| Window CSS classes, titles, mode switch, target lists per mode, HDR → H.265 | ✅ | `test_capture_window_has_standard_menu_and_quit` |
| Popovers are popped down before capture | ✅ | `test_screenshot_shortcut…` |
| Pointer probe (`tessel-capture-selection`, empty input region, enter-monitor); area overlays (OVERLAY, exclusive keyboard, dimming, Escape cancel, ≥ 2 px drag) | ✅ | `test_ui_screenshot_and_area_selection`, `test_direct_monitor_area_and_delay` |
| Delay starts after the overlay or release (monotonic, ≥ 0.35 s) | ✅ | `test_direct_monitor_area_and_delay` |
| Workspace: one session per monitor, composed to the layout size | ✅ | `test_workspace_and_armed…` |
| Window source (type 2) | ✅ | `test_portal_frame_and_window_source`, cancel path |
| Cancel is silent; errors notify only for quick captures, otherwise present the window; never for lock shots | ✅ | `test_screenshot_shortcut…`, `test_shortcut_dispatch…` |
| Lock-screen arming (hidden pointer, frame check, no retained feeds), reopen without consent, `not_before`, no clipboard or notification | ✅ | `test_workspace_and_armed…` |
| ❌ Lock-surface pixels | A real Gtk4SessionLock surface captured through PipeWire is not tested; the feeds are fake | — |
| Recording UI (hide, keep running after close, elapsed status, saved notice, presets) | ✅ | `test_ui_recording_finishes_a_portal_stream` |
| Editor: tools, render/crop, undo, save 0600, one editor per path, text hint | ✅ | `editor::tests`, `test_settings_merge_…`, `test_screenshot_notice_copies…` |
| Editor Save As dialog, keyboard shortcuts, copy button | ☑ | — |
| Folder chooser (`Gtk.FileDialog`) | ☑ | — |
| companion_theme attach, app menu | ✅ menu; ☑ theme | — |

## §9–§11
| Item | | Test |
|---|---|---|
| open_saved: non-local or missing file → `capture-open-error`; default handler with the launch context; `--select` for Files | ✅ | results checks |
| i18n via `suite_core::tr!` with the same English source strings | ☑ (UI is English under test) | — |
| Lock-screen interplay (NO_AUTO_START `lock-shot`) | ✅ | `test_workspace_and_armed…` |

## Not ported
These keep their Python or C implementations:
- `install_session.py` and `install_portal.py`
- `portal/` (patched xdpw)
- `capture_pipewire.c` (dlopen'd)

Python tests that were not ported, because they need a real portal, PipeWire or PulseAudio:
- `test_real_portal_frame` against xdpw
- `test_authorized_screen_captures_lock_surface`
- `test_recording_with_private_system_audio`

Their logic is covered with fakes as listed above.
