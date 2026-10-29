# lock-screen — parity (Rust vs ~/btmanager/lock-screen)

Contract: `contracts/lock-screen.md`. Binaries: `lock-screen` (lock_screen.py), `waylock-pam` (pam_helper.c).
Tests: `cargo test -p lock-screen` (unit + `pam_helper`, TMPDIR may be unset) and
`tests/run_headless.sh` (private 2-output headless labwc, private bus = session + system bus,
fake HOME/XDG, fails on any D-Bus activation). `LOCK_MEASURE_PYTHON=1` / `LOCK_SCREEN_BIN=` for measurements.

## Contract item → implemented → test
| Item | Status | Test |
|---|---|---|
| waylock-pam argv (none / --password-only / --fingerprint, else exit 1 silently) | yes | pam_helper::password_only_and_fingerprint_use_fixed_policies_and_reject_other_arguments |
| Wire protocol, 4096 limits, NUL rejection, short reads | yes | wrong_missing_and_malformed_responses_fail_closed, a_4096_byte_secret_is_the_maximum, long_messages_are_truncated_to_4096_bytes |
| PAM_MAX_NUM_MSG 32, count 0, styles 1..4 only | yes | message_count_and_style_limits |
| authenticate(DISALLOW_NULL_AUTHTOK) → acct_mgmt → setcred(REFRESH_CRED), "ok" only on success | yes | success_requires_password_and_second_factor, correct_password_does_not_unlock_a_disabled_account (fake checks flags) |
| uid from getuid(), never env | yes | authenticates_only_the_invoking_uid |
| RLIMIT_CORE 0, PR_SET_DUMPABLE 0, not setuid | yes | core_limit_is_zero_and_helper_is_not_setuid, dumpable_flag_is_cleared |
| Byte-identical to the C helper | yes | byte_identical_to_the_c_helper (12 scenarios, same fake libpam) |
| lock_auth: ok packet AND exit 0; fail-closed packets; cancel; client↔C/Rust helper | yes | rust_client_completes_real_native_conversation_with_both_helpers, ok_packet_without_exit_status_zero_is_not_success, cancel_terminates_the_helper_and_suppresses_completion, rust_client_selects_the_password_only_and_fingerprint_policies |
| lock_fingerprint detection only (no Claim/Verify) | yes | fingerprint::tests (3) |
| lock_status battery_summary / notification_summary | yes | status::tests (2) |
| CLI argparse prefixes, exit 2, --check JSON/exit code, no lock | yes | cli::tests, ui: check_reports_without_locking |
| GApplication ids, NON_UNIQUE | yes | app.rs (headless uses unique test ids like Python) |
| Session lock: monitor/locked/failed/unlocked, all monitors, hotplug, removal keeps lock | yes | all_monitors_covered_and_escape_cannot_unlock, monitor_hotplug_gets_a_lock_surface_and_removal_keeps_the_lock |
| READY=1 only after `locked`; duplicate locker exit 75 | yes | real_locker_survives_wrong_password_signals_and_crashes |
| Only PAM success unlocks; stale success ignored; Escape/close/SIGTERM/SIGKILL keep lock | yes | failure_cancellation_stale_success_and_valid_authentication, real_locker_… (grim: 0 canary pixels while locked) |
| Fingerprint parallel transaction, restarts, 5-attempt stop, password-only policy | yes | fingerprint_is_a_parallel_pam_transaction, password_success_cancels_fingerprint_and_failures_restart, fingerprint_stops_after_repeated_non_matches, without_fingerprint_policy_password_uses_waylock |
| Wake/idle/login layer, input forwarding between monitors, native input | yes | starts_with_wallpaper_and_native_input_works_on_either_monitor, idle_hides_only_login_…, action_sizes_and_native_mouse_wake, pointer_wake_and_hints_… |
| In-surface hints (no new toplevel) | yes | pointer_wake_and_hints_use_the_existing_secure_surface |
| Extra PAM prompts, account display name | yes | account_fallback_and_extra_authentication_prompts |
| Print → Capture lock-shot, never reveals login | yes | print_screen_does_not_reveal_login_or_type_password |
| Touch keyboard (waykeys::lock), dead keys | yes | touch_keyboard_edits_…, touch_keyboard_dead_keys_… |
| Battery / notification count privacy | yes | battery_and_notification_count_are_privacy_safe |
| Appearance live reload, assistant/voice conditional, palette per wallpaper | yes | appearance_changes_live_…, palette_follows_each_lockscreen_wallpaper_and_slideshow |
| Preview: unlocked, no auth, Preview · Close | yes | preview_is_explicitly_unlocked_and_has_no_authentication |
| Sleep (logind) / Switch User (DisplayManager seat) | yes (code) | visibility covered; calls not exercised (no logind/DM on the private bus) |
| Dashboard media group | yes (suite-lock) | dashboard_media_group_follows_media_row |

| Security review fixes: helper gets no inherited fds (close_fds), SIGPIPE default in waylock-pam, revealed password never copied/cut to the clipboard, entry text read without a GString copy | yes | auth::tests::helper_does_not_inherit_descriptors, pam_helper::closed_stdout_kills_the_helper_with_sigpipe_like_c, ui: revealed_password_never_reaches_the_clipboard |

## Gaps / differences
- **Deviation (security):** copy/cut of the revealed password is blocked (Python allows it). PRIMARY
  selection of revealed, selected text is still exported in both versions (no GTK switch).
- Unchanged in both: the GtkEntry's own popovers (right-click context menu, emoji chooser) are
  xdg popups of the lock surface; not modified here (no secret content, Python identical).
- `SettingsTests` (lock_settings.py) are covered by components/global-settings, not here.
- `test_optional_weather_and_media_on_secure_surface` (fake MPRIS player + weather fixtures + wlr-randr sizes)
  not ported here; widget behaviour lives in suite-lock (its own widgets_headless test).
- PaletteClient: the 60 ms directory-monitor fast path is folded into the 2 s poll (same as Python's poll path).
- PSS cannot be read: both lockers set PR_SET_DUMPABLE 0, so /proc/<pid>/smaps_rollup is root-owned. RSS from /proc/<pid>/status.
- No pam_start_confdir/pam_permit test (optional); real-libpam free() compatibility is covered by the fake (which frees responses with free()) and the C differential test.
- Test harness note: the persistent `wtype -s` virtual keyboard must outlive the run; when it vanished mid-run,
  later wtype text arrived with a wrong keymap (wlroots virtual-keyboard keymap switching), not a locker bug.

## Measurements (private headless labwc, pixman, 2 outputs, 3 runs each)
| | Rust (release) | Python lock_screen.py |
|---|---|---|
| Binary size | lock-screen 3.39 MB, waylock-pam 334 KB | waylock-pam (C) 13.6 KB + .py |
| spawn → READY=1 (compositor confirmed lock) | 69 / 75 / 71 ms | 187 / 174 / 175 ms |
| VmRSS while locked | 62.3–62.9 MB | 96.7–96.9 MB |
| RssAnon while locked | 12.5 MB | 37.1 MB |
