# tessel-auth (polkitman): parity with contracts/tessel-auth.md

Crate `components/auth`: binary `tessel-auth` (replaces
`authentication_manager.py` + `bluetooth_agent.py` + `libwayshell-auth-listener.so`).

Tests:
- `cargo test -p tessel-auth`: unit tests, `private_bus` (real binary, fake BlueZ),
  `polkit_register` (real libpolkit registration against a fake Authority).
- `tests/run_headless.sh`: GTK + end-to-end checks in a private nested headless labwc, where the
  private dbus-daemon is also the "system" bus. `MEASURE=1` also times the release binary and the
  Python agent.

No test starts polkit-agent-helper-1, talks to the real polkitd or BlueZ, or uses real credentials.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Contract item | | Test |
|---|---|---|
| No CLI flags; argv passed to the GApplication unchanged | ✅ | ui_headless e2e (run without args, second launch) |
| No display → stderr `Authentication agent requires an active graphical session.`, exit 1, nothing registered, no READY | ✅ | private_bus `headless_start_exits_before_registering_authentication` |
| `org.tessel.Authentication`, default flags; second launch only activates the first | ✅ | ui_headless e2e (`second instance did not register`) |
| activate once: hold, agent, register, Bluetooth, READY | ✅ | ui_headless e2e |
| Subject = `UnixSession.new_for_process_sync($LABWC_PID or getpid())`, object path `/org/tessel/Authentication`, flags NONE | ✅ | polkit_register; ui_headless e2e (Rust and Python send the same subject, locale and path) |
| Registration failure → `Could not register authentication agent: <msg>`, quit, no READY | ✅ | ui_headless `e2e_registration_failure_quits_without_ready` |
| READY=1 on `$NOTIFY_SOCKET` (`@` = abstract) only after registration; Bluetooth failure doesn't block it | ✅ | `app::tests::ready_is_sent_to_path_and_abstract_sockets`, ui_headless e2e (READY timestamp ≥ Register) |
| SIGTERM/SIGINT → quit; shutdown: bluetooth.close → agent.close → unregister | ✅ | ui_headless e2e (exit 0, `UnregisterAuthenticationAgent` seen) |
| Invalid `$LABWC_PID` | ⚠ | Python raises ValueError inside activate and hangs until systemd times out. Rust reports it as a registration failure and quits. `listener::tests::session_pid_prefers_labwc_pid` |
| `i18n.initialize()` order | ⚠ | Rust runs it before `gtk::init` (suite rule); Python runs it after `Gtk.init_check`. No observable effect. |

## §2 IPC
| Item | | Test |
|---|---|---|
| `WayshellAuthListener` subclass of `PolkitAgentListener`; GTask keeps Polkit's callback/user_data in C; finish = `g_task_propagate_boolean` | ✅ | `agent::tests::native_polkit_callback_preserves_opaque_user_data`, ui_headless (same) |
| Identities stringified (`polkit_identity_to_string`), icon/details ignored | ✅ | `agent::tests::request_fields_reach_the_window…` |
| Success → `return_boolean(TRUE)`; cancel → `G_IO_ERROR_CANCELLED "Authentication cancelled"`; closed/no identities → `"Authentication is unavailable"` | ✅ | agent tests, polkit_register (BeginAuthentication reply ok / error) |
| `CancelAuthentication` via the cancellable, handled in an idle callback (never disconnecting inside it) | ✅ | polkit_register, `caller_cancellation_closes_prompt_and_queue` (unit + ui) |
| BlueZ `/org/tessel/BluetoothAgent` `org.bluez.Agent1` (same XML), `RegisterAgent(o,"KeyboardDisplay")` → `RequestDefaultAgent(o)`, 5 s, re-register when `org.bluez` reappears, `UnregisterAgent` 2 s on close | ✅ | private_bus `bluetooth_agent_registers_answers_and_reregisters_on_private_bus` |
| Agent1 methods: Release/Cancel/RequestPinCode/RequestPasskey/RequestConfirmation/RequestAuthorization/AuthorizeService/DisplayPinCode/DisplayPasskey, `Rejected "Rejected by user"` / `"Unsupported request"` | ✅ | `bluetooth::tests::*` (port of test_bluetooth_agent.py), private_bus |
| Device name: `Properties.Get(Device1, Alias)` sync 2 s; empty → path tail; error → `AA:BB:…` | ✅ | `device_name_fallbacks`, private_bus (alias "Headphones") |
| Identity labels `"<GECOS> (<login>)"` / login / uid; non-users → string | ✅ | `identity::tests::identities_parse_and_label` |

## §3 Files
None read or written. Secrets are never logged, saved or passed in argv. `Could not display authentication request` leaves out the request data. | ✅ | `window_failure_cancels_without_logging_request_data` |

## §4 UI
| Item | | Test |
|---|---|---|
| AuthenticationWindow: title/headerbar "Authentication Required", modal, non-resizable, width 460, `.authentication-window`, 52×52 `.settings-tile.authentication-lock` tile with `changes-prevent-symbolic` at 32 px, message (wrap, 44), DropDown for more than one identity (default = own uid), otherwise the label "Authenticate as X", prompt label, entry (hidden, PASSWORD, placeholder), `.subtitle` status hidden when empty, `.error`, "Details" expander, Cancel + `suggested-action` Authenticate / "Try Again" | ✅ | ui_headless `hidden_response…`, `password_prompt…`, `failure_retry…`, `three_failures…` |
| `companion_theme.attach` on every window | ✅ | ui_headless (`tessel-dock-companion-theme` data) |
| CSS once per display (lock radius; pairing code) | ☑ | |
| PairingWindow: width 420, `bluetooth-active-symbolic` at 30 px, `.pairing-code`, entry with DIGITS/FREE_FORM, reject defaults to "Cancel", accept is `suggested-action` and stays disabled until the validator passes, close/Escape → reject | ✅ | ui_headless `pairing_window_validation_and_answers`, `pairing_agent_display_passkey_updates_real_window` |
| Escape key | ☑ | Same handler as close-request (tested). Synthetic key input is not sent. |
| Entry storage | ⚠ | `GtkPasswordEntryBuffer` (secure memory, zeroed by GTK) instead of a normal buffer. Looks and behaves the same, including echo-on prompts. See SECURITY.md. |

## §5 Conversation
FIFO queue, one window, next on idle; is_password_prompt; echo_on; up to 3 attempts; identity switch resets attempts and cancels the old session; Escape/close/Cancel → CANCELLED; entry cleared after submit/finish/completion; cookie wiped. All ✅: `requests_are_queued_one_window_at_a_time`, `identity::tests::password_prompts`, ui_headless tests 1-7.
- `casefold` ⚠: approximated as lowercase plus ß→ss and ς→σ. That is enough for the 31 catalog translations of "Password".
- Session creation failure outside the first show (retry or identity switch) ⚠: Python raises out of a signal handler and the request is left hanging. Rust prints `Could not display authentication request` and cancels.

## §7 i18n
All strings go through `tr!` with the Python text and placeholders. Catalogs are loaded at runtime (`~/.local/share/tessel/locale`).

## §8 Python tests ported
test_authentication.py: all 7 → unit `agent::tests::*` + ui_headless. test_bluetooth_agent.py: all 4 → `bluetooth::tests::*`. test_native_callback.c → `native_polkit_callback_preserves_opaque_user_data` (Rust `extern "C"` callback with a sentinel user_data).

## Gaps
- The real `PolkitAgentSession` (PAM helper) path is never run, on purpose. `PolkitSession` is a thin FFI wrapper (signals `request`/`show-info`/`show-error`/`completed`, `initiate`/`response`/`cancel`). It is checked only by compiling and by registering for real; the first real-world check is the swap.
- The real binary's window is not opened end to end, because that would start the setuid helper. Windows are tested in-process with FakeSession through the same listener vfunc.
