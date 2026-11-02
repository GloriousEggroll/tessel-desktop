# tessel-tiling (Tessel Tiling) — parity with contracts/tessel-tiling.md

Rust crate `components/tiling`: binaries `tessel-tiling` (= `tiling_manager.py`,
also runs `--watch` when started as `tiling_status.py`) and `tessel-tiling-status`
(= `tiling_status.py`), library `tiling_manager` (client, config, gaps,
outline, bindings, GTK `ui`), example `fake_compositor` (measurement fake).
The compositor-side C (`labwc-color/labwc/src/tiling.c`) is not ported; the
datagram protocol is the boundary.

Tests: `cargo test -p tessel-tiling` (unit + `cli` end-to-end against a fake
control socket + `python_parity` differential test), `tests/run_headless.sh`
(GTK checks in a private nested headless labwc), `tests/measure_headless.sh`.
Every rc.xml edit in tests is on a scratch copy under `$TMPDIR`; reload is a
no-op closure or a recording `labwc` script first on `PATH`.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Contract item | | Test |
|---|---|---|
| Mutually exclusive `--toggle/--enable/--disable/--restore/--status/--watch/--settings/--popup`, `--popup-edge` choices, `--popup-offset` int (clamped ≥0, never an error), argparse usage/help/errors/abbreviations, exit 2 | ✅ | `cli::tests::*`, `cli::argument_errors_match_argparse` |
| `--status`: one `json.dumps(status())` line (ensure_ascii, key order = reply then text/class/tooltip/sign_out_required) | ✅ | `cli::status_reports_the_compositor_and_every_failure_kind` (byte-exact line); measure: identical to Python output |
| `--watch`: 1 s poll, OutlineSync (errors ignored), print only on change, flush; SIGINT / broken pipe → exit 0 | ✅ | `cli::watch_prints_only_changes_and_ends_on_sigint` (both binaries) |
| `--enable/--disable/--toggle`: not ready → stderr error, exit 1; else `set_enabled` | ✅ | `cli::enable_disable_toggle_and_restore_edit_a_scratch_rc_xml` |
| `--restore`: only when `config.json` `enabled is True`, `remember=False`, never checks ready | ✅ | same |
| `--settings/--popup/none` → GTK popup app `org.tessel.Tiling`, argv[0] only, 2nd launch activates → `controller.activate()` | ✅ | ui_headless `second_launch_activates_the_first_instance` |
| `tiling_status.py` = `--watch` | ✅ | `tessel-tiling-status` bin + argv[0] alias; `cli::python_shim_argv_keeps_script_names` |
| Status path imports no GTK | ✅ | GTK initialised only in `ui::popup::run` |

## §2 Control socket client (`client`)
| Item | | Test |
|---|---|---|
| Path `$XDG_RUNTIME_DIR/tessel-tiling-$WAYLAND_DISPLAY.sock` (fallback `/run/user/<uid>`), reject empty/`/` display or >255-byte command | ✅ | cli status test (`../x`) |
| stat: owner uid + S_ISSOCK, else "not owned" | ✅ | cli status test (regular file) |
| reply socket in `mkdtemp(wt-…)/reply`, 1.5 s timeout, recv 65536, cleaned up | ✅ | cli status test (no `wt-*` leftovers) |
| connect error is an OSError (`[Errno 111] Connection refused`, not sign-out) | ✅ | cli status test |
| `version==1 && ready is True`, non-empty `error` → RuntimeError, translated messages, `sign_out_required` | ✅ | cli status test (locked session error) |
| Library for tessel-shell (`request('capture-output …')`) | ☑ | `client::request` is public |

## §3 rc.xml overlay (`config`, `bindings`, `xml`)
| Item | | Test |
|---|---|---|
| BINDINGS table, keyboard/mouse XML, EXTRA_SHORTCUTS byte-identical to bindings.py | ✅ | `bindings::tests::table_matches_python_bindings` (runs the Python, read-only `-B`) |
| No `W-l`, `C-A-Delete`, `close-all`; unique keys; `W-A-l` layout; lock path | ✅ | `bindings::tests::block_never_binds_close_all_or_super_l` |
| Managed blocks, base64 wrapper for missing/self-closing sections, unwrap unless user added children, marker checks, comment masking, multiple-section and DOCTYPE/ENTITY/root rejections | ✅ | `config::tests::{enable_disable_*, missing_and_self_closing_*, user_added_*, rejected_documents}` |
| Byte-exact vs Python on a corpus + a scratch COPY of the live rc.xml (enable, disable, enable→disable, with_gap 0/7) | ✅ | `python_parity::rc_xml_edits_match_python_byte_for_byte` |
| `set_enabled` transaction: flock, status, session path, empty-root expansion, backup `rc-<stamp>.xml` 0600, concurrent-change refusal, mode-preserving atomic write, reload (LABWC_PID), enable/disable confirm, prefs merge, OutlineSync, full rollback | ✅ | `config::tests::{transaction_failure_*, saved_preference_*, missing_config_*, concurrent_edit_is_refused, unconfirmed_change_rolls_back}`, cli enable test |
| `session_config()` (cmdline `-c/-C/--config[=]/--config-dir[=]`, `-m` refusal, `(deleted)` exe, cwd-relative, XDG fallback) | ✅ | `config::tests::session_config_*`, cli enable test (fake `labwc` PID) |
| `labwc --reconfigure` with Python's CalledProcessError/TimeoutExpired/OSError texts | ✅ | `config::tests::reconfigure_reports_python_messages` |

## §3b Gapless (`gaps`)
`with_gap` span edits, minidom `toxml()` of `<core>`, previous gap, self-closing root/core, comments, invalid inputs; `set_gapless` no-op, `gapped_spacing`, backup `gap-<stamp>.xml`, rollback on reload/prefs failure: ✅ `gaps::tests::*`, `config::tests::{persistence_*, reload_or_preference_*, invalid_saved_spacing_is_rejected}`, `python_parity`.

## §4 Files / OutlineSync
config.json (unknown keys kept, `json.dumps`+`\n`, 0600), change.lock, backups, palette.json limits (1 MiB, 64 monitors, name regex, int channels): ✅ `outline::tests::*`, `config::tests::{toggle_preserves_*, failed_preference_write_*, invalid_preferences_raise}`, cli watch (palette → one `outline-color`).

## §5 UI
| Item | | Test |
|---|---|---|
| Popup: title, `tiling-shortcuts`, `.popup-surface`, width ≤420 × 620, PopupController (edge/offset), header `view-grid-symbolic`, subtitle On/Off/hidden, hint, SearchEntry filter (all casefolded words), rows `.shortcut-row/.key`, scroller 120–480, 2 s refresh while visible, timer + CSS removed on unrealize | ✅ | ui_headless `shortcut_popup_controls_and_search` |
| TilingControls: rows, AsyncSwitch (worker thread, insensitive while busy, revert on error), Gapless, outline, notices, `.error`, Sign out visibility, shortcuts button | ✅ | same, with `Runner::Inline` and `Runner::Thread` (`worker_thread_controls`) |
| Sign-out AlertDialog + `labwc --exit` guarded by `/proc/$LABWC_PID/comm` | ☑ | not clicked in tests (would end the private session) |
| Measured: popup screenshot of Rust vs Python | see report | `measure_headless.sh` |

## Deviations ⚠
1. **XML parser**: a small span-preserving parser (`xml.rs`) replaces expat/minidom. Accept/reject decisions and output bytes match on the parity corpus; expat's exact error *texts* are matched for common cases only (mismatched tag, no element, junk, duplicate attribute, undefined entity, syntax error).
2. JSON decode errors of a corrupt `config.json` use serde_json's wording.
3. A reply without `enabled` is treated as disabled (Python raised KeyError from `status()`).
4. The popup app destroys a stale unrealized popup window before creating its replacement (Python left it hidden).
5. Python shims exec the binary as `[python3, <script>.py, …]` (see report); the binary drops the interpreter argument.
6. D-Bus: none needed (GApplication only). No zbus dependency.

## Gaps ❌
- Global Settings embeds the **Python** `TilingControls`/`TilingConfig` and reads `bindings.BINDINGS`; tessel-shell imports `tiling_client`. These Python modules must stay installed until those consumers are ported (the Rust equivalents are public in the library; `bindings::bindings_json()` is the machine-readable export).
- `test_native.py` (compositor behaviour) stays a C-side suite; installer (`install.py`) not ported.
