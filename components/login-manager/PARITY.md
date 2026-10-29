# login-manager (Waylogin) — parity

Contract: `contracts/login-manager.md`. Python: `~/btmanager/login-manager`.
Binaries: `login-ui` (login_ui.py), `login-settings-helper` (login_settings_helper.py),
`session-transition` (session_transition.py). Library `waylogin`: `protocol` (login_protocol.py),
`transition` (login_transition.py), `desktop_fade`, `ui`, `hardening`. Model additions
(`User`, `users()`, `Session::request()`) are in `crates/suite-lock/src/login_model.rs`.

Tests: `cargo test -p login-manager` (unit: 14 protocol + 14 helper incl. login_model copy; 4 helper_cli)
and `cargo test -p suite-lock --lib login_model` (7); UI: `tests/run_headless.sh` (15 checks, private
2-output headless labwc, private bus as session+system bus, fails on any service activation);
measurements: `tests/measure.sh`.

| Contract item | Status | Test |
|---|---|---|
| Frame bytes (`=I` native length, `json.dumps(ensure_ascii=False)`, separators, key order, escapes) | byte-identical to Python reference frames | protocol::frames_are_byte_identical_to_python_exchange; ui real_greetd_sign_in_* (password frame on the wire) |
| MAX_FRAME, size 0, truncated/EOF, non-JSON, non-object, bad `type` → fail closed | yes | protocol::malformed_backend_frames_fail_closed, malformed_reply_mid_conversation_fails_closed |
| Oversized request never sent | yes | protocol::oversized_request_is_never_sent |
| State machine, reset-before-create, info/error ack without `response`, 2FA, start only after success | yes | protocol::password_second_factor_and_info_before_session_start |
| auth_error / description / missing / null description texts | yes | protocol::wrong_password_cannot_start_session, backend_error_description_is_shown_without_session |
| Socket loss / missing socket → generic message | yes | protocol::backend_disconnect_cannot_start_session, missing_socket_reports_unavailable |
| Unexpected auth_message in reset/start; unsupported prompt → cancel | yes | protocol::unexpected_and_unsupported_messages_fail_closed |
| Generation counters, stale results, cancel race | yes | protocol::stale_authentication_success_after_cancel_is_ignored, cancel_race_never_creates_or_starts_a_session |
| start_session only from `ready` + current generation, once; launching not cancellable | yes | protocol::fade_completion_cannot_launch_without_current_authentication |
| Username ≤256 / response ≤4096 UTF-8 bytes, NUL | yes (inclusive limits) | protocol::invalid_credentials_are_rejected_before_transmission |
| Timeouts 5 s connect / 120 s IO | implemented (poll connect, SO_RCV/SNDTIMEO) | not timed in tests |
| Session.request quoting, labwc `--startup` rule | yes | login_model::session_request_quotes_for_greetd_shell |
| users() (UID_MIN, shells, gecos, casefold sort) | yes (NSS via getpwent) | login_model::users_follow_python_rules |
| Root helper: euid/argv refusal, 32768 limit, symlink refusal, exact keys, Python `==` on `expected`, from_dict errors, session check, realpath(strict) error texts, suffix, S_IROTH/S_IXOTH | yes; error strings diffed against Python | helper tests (9) + helper_cli (production binary, unprivileged) |
| Helper output `json.dump(indent=2)`+`\n`, 0644, atomic, temp removed on failure | byte-identical | config_bytes_match_python_json_dump, writes_atomically_*, failed_replace_removes_the_temporary |
| Helper links no GTK | yes (glib/gio/libc only) | helper_cli::helper_does_not_link_gtk |
| Fade (smoothstep, deadline+150 ms, cancel) | yes | ui success_fades_before_handoff_*, desktop_cover_* |
| session_transition (2 windows, keyboard NONE, zone -1, ≤2 s, quit once) | yes | ui desktop_cover_expires_and_does_not_take_keyboard_focus |
| Greeter UI: idle/wake, keys, pointer per monitor, idle timeout, a11y mirroring, orca launch, power confirmation, equal 44 px actions, chevron, OSK local typing, visual family | yes | ui (11 ported checks) |
| --preview (1 window, close button, inert submit, no power, no greetd, no reader) | yes | ui preview_is_windowed_and_inert; measure.sh screenshot = Python's |
| logind CanX/Suspend(false), never during launching | yes | ui fake_logind_controls_power_buttons |
| CLI: `--bogus` exit 2, no GREETD_SOCK exit 1, bad config exit 1 | yes | measure.sh |
| Core dumps off + PR_SET_DUMPABLE | yes | measure.sh (needs an LD_PRELOAD shim to read smaps — proof it is active) |

## Intentional differences / gaps
- Secrets: GtkPasswordEntryBuffer, one zeroized copy out of GTK, zeroized request frame (Python could not wipe).
- argparse prefix abbreviations (`--prev`) are not accepted.
- Startup failure (no layer shell / GREETD_SOCK) exits 1 with the message (Python printed a traceback from the vfunc).
- Helper JSON syntax-error wording is serde_json's; UTF-16/32 JSON input is rejected (Python auto-detects); non-printable chars in path reprs are not `\u`-escaped.
- `chmod` is `fchmod` on the open fd.
- Tests use a test-only `Options::application_id` per fixture (registered GtkApplications keep a startup ref).
- Real PAM / greetd / logind / polkit are never exercised (fakes only), by design.
- Security review fixes (parity restored, crates/suite-lock login_model): `shlex_split` no longer treats `\$`, ``\` `` and `\<newline>` as escapes inside double quotes (Python posix shlex only escapes `\"`/`\\`; the old port silently changed session `Exec=` argv) — test `login_model::shlex_double_quote_escapes_match_python`; an `UID_MIN` too large for i64 now saturates like Python's unbounded `int()` (hides every account) instead of falling back to 1000 — test `login_model::oversized_uid_min_saturates_like_python`.
- Root helper links libglib/libgio/libgobject (via suite-core) though it calls no GLib API; Python's helper loaded no GLib. Acceptable under pkexec's sanitized environment; noted for a future suite-core split.

## Measurements (release, private headless labwc, GSK cairo, 3 rounds, `--preview`)
| | Rust | Python |
|---|---|---|
| time to toplevel mapped | 186–298 ms | 263–330 ms |
| RSS (3 s after map) | ~93.7 MB | ~147.0 MB |
| PSS | ~57.1 MB | ~103 MB |
| binary size | login-ui 2.5 MB, helper 0.58 MB, session-transition 0.41 MB | — |
