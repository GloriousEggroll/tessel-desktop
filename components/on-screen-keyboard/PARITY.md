# on-screen-keyboard (Waykeys) — parity with contracts/on-screen-keyboard.md

Crate `on-screen-keyboard`, library `waykeys`, binaries `osk-manager` (osk_manager.py) and
`osk-status` (osk_status.py). The native helper `waykeys-input` is vendored unchanged
(`native/input.c`, `native/protocols/*.xml`, byte-identical to btmanager) and built by
`build.rs` with the Makefile's flags (`-O2 -Wall -Wextra -Werror`). The GTK process never binds
the virtual-keyboard / input-method protocols; only the helper does.

Tests: `cargo test -p on-screen-keyboard` (unit + `tests/bridge.rs` + `tests/lock_monitor.rs`
on a private `gio::TestDBus`), and `tests/run_headless.sh` (private nested headless labwc,
2 outputs, private D-Bus/XDG dirs) for `tests/ui.rs` and `tests/service_phase.sh`.
`tests/ui.rs` skips itself unless it is in the private compositor (`WAYKEYS_UI`, runtime dir
under `/run/user/1000/cbt/`).

| Contract item | Implemented | Test |
|---|---|---|
| §1 GApplication `org.waydock.OnScreenKeyboard`, HANDLES_COMMAND_LINE, argv forwarded, `hold()` | yes (`manager::Manager`) | service: `forwarded --show exits 0`, `shown via primary` |
| §1 CLI flags, precedence quit>settings>background>hide>show>toggle, unknown → 2 | yes | `manager::tests::arguments_and_precedence`, service: `unknown flag exits 2` |
| §1 `--background` starts with nothing shown, no helper | yes | service: `idle: no helper`, `idle state` |
| §1 `--quit` | yes | service: `quit`, `state removed at shutdown` |
| §1 `--settings` opens/presents one window | yes | ui: `settings_save_and_update_the_existing_keyboard` |
| §2 helper line protocol: `key`/`text`/`layout` validation, queue cap 128, line ≤32768, JSON object | yes (`backend`) | `backend::tests::*`, `bridge::bridge_and_labels` |
| §2 helper exit → "The keyboard input connection closed…"; close = EOF, force_exit after 2 s; write error message | yes | `bridge_and_labels`, ui: `failed_input_helper_can_be_reopened_in_automatic_mode` |
| §2 `waykeys-input --labels` (lock screen, 2 s timeout, LRU 8) | yes (`lock::labels`) | `bridge_and_labels` |
| §2 typing semantics (IM commit, compose, WebKit wait, `<I255>` text keymap) | helper unchanged | ui: `real_wayland_text_entry…`, `layout_switch…`, `legacy_input…` |
| §2 systemd lock monitor: Get ActiveState NO_AUTO_START 1.5 s, PropertiesChanged; `failed` locked only on change | yes (`session`, gio D-Bus) | `session::tests::lock_states`, `lock_monitor` (private bus, fake unit) |
| §2 udev: classification, identity, dedup, sorting, 180 ms debounce, `automatic_available` | yes (`devices`, `udev` crate + glib fd source) | `devices::tests::*` (classification/override ported from test_policy.py) |
| §3 config.json: validation, defaults, indent=2 + `\n`, 0600, `.keyboard-*` temp + fsync + rename | yes (`config`) | `config::tests::config_atomic_roundtrip_and_validation`, `validation_rules` |
| §3 labwc environment `XKB_DEFAULT_LAYOUT` parsing, env fallback, `us` | yes | `config::tests::system_layout_follows_labwc_environment` |
| §3 config dir monitor + environment file monitor, 120 ms debounce, layout sent only if changed, size live | yes | ui: `settings_save_and_update_the_existing_keyboard` |
| §3 state.json (dir 0700, file 0600, `.tmp`+rename, written on change, removed if pid matches) | yes | ui: `state_file_tracks_visibility`, service phase |
| §4 Visibility policy | yes (`state`, verbatim) | `state::tests::*` |
| §4 lock suspends (hide, active=false, epoch=-1, bridge torn down), unlock restores | yes | ui: `desktop_keyboard_suspends_while_lock_is_active` |
| §4 ready w/o IM / ime-unavailable / error / layer-shell-unsupported messages | yes (translated as Python) | — (strings reviewed; IM-less compositor not available headless) |
| §4 context change resets modifiers; purposes 2,3,4,9 → numbers | yes | `keyboard::tests::levels_and_purposes`, ui: `pin_field_and_manual_show_without_touchscreen` |
| §4 bridge failure → keys insensitive; next show recreates the bridge | yes | ui: `failed_input_helper_can_be_reopened_in_automatic_mode` |
| §4 placement on pointer's output (PointerMonitor), width min(1100,max(300,w-24)), `<700` narrow, monitor/geometry changes reposition | yes | ui: `keyboard_uses_pointer_output_without_moving_text_focus`, `manager::tests::widths` |
| §4 launcher visible iff eligible && !shown && !suspended | yes | ui: `automatic_and_live_keyboard_attach_detach`, `desktop_keyboard_suspends…` |
| §5 panel layer surface (namespace `waykeys-keyboard`, OVERLAY, kbd NONE, bottom 8, auto exclusive zone / 0 hidden, close-request → hide) | yes | ui (all panel cases) |
| §5 launcher (`waykeys-launcher`, TOP, bottom+right 12, 52×48, tooltip) | yes | ui: `automatic_and_live…` |
| §5 settings window 640×580, headerbar, app menu (quit closes the window only), KeyboardSettings | yes (`settings`) | ui: `settings_save…` (about action, single window) |
| §5 keyboard widget: CSS classes, three layouts, focusable=False, repeat 420/65 ms, shift double-tap lock, one-shot modifiers, `·` + insensitive empty labels, secure mode | yes (`keyboard`) | `keyboard::tests::*`, ui: `real_pointer_key_and_layout_size`, `pin_field…`, `lock_keyboard…` |
| §5 settings CSS / search_targets keys | yes (`KeyboardSettings::search_targets`) | — |
| §5 lock-screen keyboard (Enter/Esc/arrows/Home/End/Ctrl+A, no clipboard, dead keys + NFC, dead+dead) | yes (`lock::LockKeyboard`, `lock::apply_key`, host-agnostic `LockHost`) | ui: `lock_keyboard_edits_the_selected_lock_field_without_ipc` |
| §5 companion theme on every window | yes (`suite_gtk::theme::attach`) | — |
| §5 waydock status: `osk_status.py` output, pid check, 1 s | yes (`osk-status`, never links GTK) | `status::tests::status_lines`, service: `status line` |
| §6 window_backend / waydock-toplevel | intentionally not spawned (unused at runtime, contract §6) | — |
| §7 i18n strings via `tr!`; osk_status / bridge / lock-button strings untranslated | yes | — |
| §9 idle: no windows, no helper; windows created lazily | yes | service: `idle: no helper` |

## Gaps / differences
- Error texts for malformed JSON/IO come from serde/std instead of Python exception text
  (only shown in the keyboard status line).
- The settings page falls back to defaults if config.json is invalid (Python raised in the
  constructor); Global Settings still embeds the Python `osk_settings.py` until wave 4.
- `LockKeyboard` takes a `LockHost` (entry, submit, visibility, focus, overlay, scroll, button box,
  size group) instead of the lock window object; the lock-screen port must wire it.
- `osk-status` still polls once per second (pid liveness needs polling anyway); it is now a
  ~1 MB native process, but Waydock hard-codes `python3 -u osk_status.py`, so the swap needs a shim
  or a Waydock change.
- D-Bus client uses gio (as suite-core does), not zbus: only one Get + one signal subscription.
