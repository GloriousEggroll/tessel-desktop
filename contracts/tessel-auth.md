# tessel-auth (polkitman) — external contract

Source: `~/tessel-bluetooth/tessel-auth/` (refs `file:line` there). One resident process:
a Polkit authentication agent (GTK4) that also hosts the BlueZ pairing agent. ~38 MB RSS observed.

## 1. Entry points & processes
- Executable: `/usr/bin/python3 ~/.local/share/tessel-auth/authentication_manager.py` — **no CLI flags** (argv passed to `Gtk.Application.run` untouched) (authentication_manager.py:352-362).
- `main()`: if `Gtk.init_check()` fails or no default `Gdk.Display` → stderr `Authentication agent requires an active graphical session.` and **exit 1 before registering anything** (:353-357). Then `suite_i18n.initialize()`, SIGTERM/SIGINT → `app.quit()` (:358-361).
- `Gtk.Application(application_id='org.tessel.Authentication')` default flags (:303-305). `do_activate` once: `hold()`, create agent, register, start Bluetooth agent, then sd_notify READY (:309-339).
- Registration failure → stderr `Could not register authentication agent: <msg>` and `quit()` (:321-324) (Type=notify ⇒ start fails).
- Shutdown order: bluetooth.close → agent.close (cancel all) → `PolkitAgent.Listener.unregister(handle)` (:341-349).

### systemd user unit `tessel-auth.service` (tessel-auth.service)
```
[Unit] Description=Desktop Authentication Agent
After=graphical-session.target  PartOf=graphical-session.target
[Service] Type=notify
ExecStart=/usr/bin/python3 %h/.local/share/tessel-auth/authentication_manager.py
Restart=on-failure RestartSec=2 TimeoutStartSec=15
```
- **Type=notify**: readiness is sent manually by writing `READY=1` to a `SOCK_DGRAM` AF_UNIX socket at `$NOTIFY_SOCKET` (leading `@` → abstract `\0`) only **after** polkit registration succeeded (:334-339). Bluetooth agent failure does not block READY.
- No [Install]; started from labwc autostart with **restart** (not start), so a stale previous-session registration is replaced: `systemctl --user restart tessel-auth.service >/dev/null 2>&1 &` (tessel-session-startup/configure_session.py:370-371). Autostart first runs `dbus-update-activation-environment --systemd … LABWC_PID …` so the unit sees `LABWC_PID`.
- Drop-in `tessel-auth.service.d/50-tessel-recovery.conf` (configure_session.py): StartLimitBurst=5/60s, `OnFailure=tessel-failed@%n.service`, RestartSec=1, `EnvironmentFile=-%h/.config/labwc/environment.d/90-local.env`. Failure label 'Authentication agent' (tessel-session-startup/desktop_suite_failed.py:26). Listed in configure_session.py:581, configure.py:18, install_gtk4.py:22 UNITS.

### Install layout (shared/install_gtk4.py:176-182; no own installer)
`~/.local/share/tessel-auth/`: authentication_manager.py, bluetooth_agent.py, libtessel-shell-auth-listener.so (0644), companion_theme.py (from tessel-dock/), gtk4_utils.py, suite_i18n.py, language_data.py, suite_catalogs.py (+ background_blur.py, libtessel-dock-background-blur.so present in live dir via themed-dir logic :183-). Unit copied to `~/.config/systemd/user/tessel-auth.service`. Installer backs up the old service and restores it if startup fails (README).
- `build.py`: `cc -O2 -shared -fPIC -Wall -Wextra -Werror auth_listener.c -o libtessel-shell-auth-listener.so $(pkg-config --cflags --libs polkit-agent-1)`; built before install, never at startup (build.py:1-16).
- Path quirk: `sys.path[:0] = [ROOT/'shared', ROOT/'tessel-dock']` with ROOT = `~/.local/share` (:11-12) — i.e. `~/.local/share/tessel-dock/{companion_theme,suite_i18n}.py` **take precedence** over the local copies. (Irrelevant for Rust; note when comparing behaviour.)

## 2. IPC

### Owned / registered
| What | Detail | Ref |
|---|---|---|
| Session bus name `org.tessel.Authentication` | GApplication unique name, `/org/tessel/Authentication` with `org.gtk.Application`/`org.gtk.Actions` (default GApplication export). A second launch just activates the first (no-op since agent exists). | :305,310 |
| Polkit agent (system bus) | `PolkitAgent.Listener.register(flags=NONE, subject, object_path='/org/tessel/Authentication', cancellable=None)` → libpolkit exports `org.freedesktop.PolicyKit1.AuthenticationAgent` (`BeginAuthentication(s action_id, s message, s icon_name, a{ss} details, s cookie, a(sa{sv}) identities)`, `CancelAuthentication(s cookie)`) at that path on the **system bus** and calls `org.freedesktop.PolicyKit1.Authority.RegisterAuthenticationAgent((sa{sv}) subject, s locale, s object_path)`. Subject = `Polkit.UnixSession.new_for_process_sync(int($LABWC_PID or getpid()))` — the compositor's logind session, because user services aren't in a session. | :315-320 |
| BlueZ agent (system bus) object `/org/tessel/BluetoothAgent`, interface `org.bluez.Agent1` | registered with `org.bluez.AgentManager1.RegisterAgent(o "/org/tessel/BluetoothAgent", s "KeyboardDisplay")` at `/org/bluez`, then `RequestDefaultAgent(o)`; re-registered whenever `org.bluez` (re)appears (`bus_watch_name`); `UnregisterAgent(o)` on close (2 s timeout) | bluetooth_agent.py:14-29,141-170,267-281 |

`org.bluez.Agent1` methods implemented (exact signatures, bluetooth_agent.py:19-29,185-235):
| Method | In → Out | Behaviour |
|---|---|---|
| Release | () → () | mark unregistered, dismiss prompt |
| Cancel | () → () | dismiss open prompt (its pending invocation gets `org.bluez.Error.Rejected`) |
| RequestPinCode | (o) → (s) | entry "PIN code", valid 1–16 chars |
| RequestPasskey | (o) → (u) | digits entry "Passkey", valid ≤6 digits, reply `int(text)` |
| RequestConfirmation | (o,u) → () | shows code `%06d`, buttons "Doesn’t match"/"Pair" |
| RequestAuthorization | (o) → () | "Deny"/"Allow" |
| AuthorizeService | (o,s) → () | title "Bluetooth connection", "Deny"/"Allow" |
| DisplayPinCode | (o,s) → () | returns immediately; non-blocking window showing code, "Close" |
| DisplayPasskey | (o,u,q entered) → () | returns immediately; same device's repeats update the existing window text "… {typed} of {total} digits entered." |
| other | | error `org.bluez.Error.Rejected` "Unsupported request" |
- Reject/close/Escape → `org.bluez.Error.Rejected` "Rejected by user" (`CANCELED` constant defined but unused). New request dismisses (rejects) the previous prompt. Device name: `Properties.Get(org.bluez.Device1, Alias)` sync 2 s; fallback path tail `dev_AA_BB…` → `AA:BB:…` (:176-183).

### Native ABI: libtessel-shell-auth-listener.so (auth_listener.c)
- GType `TesselShellAuthListener` subclass of `PolkitAgentListener`; exported C fn `void tessel-shell_auth_register_type(void)` (g_type_ensure) (auth_listener.c:43).
- Overrides `initiate_authentication` → creates `GTask(listener, cancellable, callback, user_data)` and emits signal **`authenticate`** `(GTask task, gchar* action_id, gchar* message, gchar* cookie, GStrv identity_strings, GCancellable cancellable)`; identities stringified with `polkit_identity_to_string` (e.g. `unix-user:tcrider`); icon_name & details ignored (:11-25,36-38).
- `initiate_authentication_finish` → `g_task_propagate_boolean` (:27-30).
- Python completes with `task.return_boolean(True)` (conversation done) or `task.return_error(G_IO_ERROR_CANCELLED, "Authentication cancelled" | "Authentication is unavailable")` (authentication_manager.py:239-241,287-293). Polkit's opaque callback/user_data must stay in C (test_native_callback.c verifies original user_data survives success and cancel). A Rust port can implement the PolkitAgentListener subclass directly (glib subclassing) — the .so is then unnecessary.

### Consumed
| Service | Use |
|---|---|
| polkitd (system bus) via libpolkit-agent-1 `PolkitAgent.Session.new(identity, cookie)`: signals `request(prompt, echo_on)`, `show-info(text)`, `show-error(text)`, `completed(authorized)`; methods `initiate()`, `response(text)`, `cancel()` | authentication_manager.py:130-135,204 |
| logind (indirectly through `Polkit.UnixSession.new_for_process_sync`) | :318 |
| BlueZ `org.bluez` system bus | above |
| `pwd.getpwuid` for identity labels: `"<gecos first field> (<login>)"` or login; unknown uid → number; non-user identities → `to_string()` | :22-30 |

## 3. Files
None read or written (no config/state). Credentials never logged/saved/passed in argv; error print `Could not display authentication request` deliberately omits request data (:271-273).

## 4. UI surfaces
Normal xdg toplevel windows (no layer-shell), `modal=True`, non-resizable, application = the agent app.
- **AuthenticationWindow** (:39-117): title/headerbar "Authentication Required", default width 460; CSS class `authentication-window`; 52×52 tile `.settings-tile.authentication-lock` (border-radius 14px) with icon `changes-prevent-symbolic` 32px; message label (wrap, 44 chars); identity `Gtk.DropDown` (tooltip/a11y "Authenticate as") shown only when >1 identity else label "Authenticate as <name>"; default selection = identity with uid == getuid() else 0; prompt label (hidden for plain password prompts); `Gtk.Entry` (hidden text, input-purpose PASSWORD, placeholder "Password"); status label `.subtitle` (+`.error`), auto-hidden when empty; `Gtk.Expander` "Details" with selectable action_id; buttons "Cancel" and "Authenticate" (`suggested-action`, label becomes "Try Again" after failure).
- **PairingWindow** (bluetooth_agent.py:45-128): same `authentication-window` styling, width 420, icon `bluetooth-active-symbolic` 30px, `.pairing-code` label (30px, bold, letter-spacing 6px, tnum), optional entry, reject + accept (`suggested-action`) buttons; accept disabled until validator passes.
- Theme: `companion_theme.attach(window)` (live dock palette/transparency/blur) — test asserts `window._tessel-dock_companion_theme` set. CSS added once per display via `gtk4_utils.add_css`.
- Shared modules: gtk4_utils (`headerbar`, `image`, `key_controller`, `add_css`), companion_theme, suite_i18n. No tray, no tessel-dock integration.

## 5. Conversation behaviour (parity-critical)
- Requests queued FIFO; one window at a time; next shown on idle after completion (:243-248,258-274,295).
- Caller cancel (`cancellable` from CancelAuthentication) handled via `GLib.idle_add` — never disconnect inside the cancellable callback (deadlock) (:244-247).
- `is_password_prompt`: strip, strip trailing `:`/`：`, casefold ∈ {"password", tr("Password").casefold()} (:33-36). Password prompt → prompt label hidden, placeholder "Password"; other prompts → label shown with PAM text, placeholder empty, a11y label = prompt. `echo_on` → visible text, FREE_FORM.
- Up to **3 attempts**: on `completed(False)` with attempts <3 → drop session, error "Authentication failed. Please try again.", button "Try Again"; pressing it starts a new session. `completed(True)` or 3rd failure → finish (task returns TRUE either way — polkitd's helper decides authorization) (:177-190,276-293).
- Changing identity resets attempts, clears status, cancels old session (if not completed) and starts a new one (:146-150,137-144).
- Escape / close / Cancel → cancelled (G_IO_ERROR_CANCELLED). Entry cleared after each submit, finish, and completion; cookie wiped from request dict.
- Headless start → exit 1 without constructing the Application.

## 6. External commands
None at runtime. Build: `pkg-config`, `cc` (build.py).

## 7. i18n
`suite_i18n` catalogs (shared/locale). Strings: Authentication Required, Authenticate as, Password, Details, Cancel, Authenticate, Try Again, Authentication failed. Please try again.; Bluetooth: Pair with {device}, Enter the PIN code for {device}. It is often 0000 or 1234, or shown on the device., PIN code, Pair, Enter the six-digit passkey shown on {device}., Passkey, Confirm that {device} shows this code., Doesn’t match, {device} wants to pair with this computer., Allow, Deny, Bluetooth connection, {device} wants to connect to this computer., Type this code on {device}, then press Enter., Type this code on {device}, then press Enter. {typed} of {total} digits entered., Close. stderr messages untranslated.

## 8. Parity test cases
test_authentication.py: headless start returns 1 and never builds Application (:40); hidden response → response delivered, entry cleared; echo-on prompt shows text; success completes TRUE, window cleared (:81); password prompt is placeholder, status hidden when empty, `suggested-action` present, non-password prompt shown (:98); failure → retry; identity switch cancels old session and uses new uid (:115); caller cancellation closes the prompt and the queued request, both CANCELLED (:131); shutdown cancels every request exactly once (:145); native callback keeps opaque user_data on success and cancel (:154).
test_bluetooth_agent.py: confirmation code `004021` zero-padded, title contains `AA:BB:CC:DD:EE:FF` fallback name, accept → empty reply, reject → Rejected (:72); PIN reply `("0000",)`, passkey `(123456,)`, validators (:85-96); DisplayPasskey progress updates one window "3 of 6" (:98); Cancel and new request reject the open prompt, window cleared (:107).

## 9. Memory / startup
GTK4 + Polkit/PolkitAgent GI + full suite_catalogs (1.3 MB .py) + companion_theme → ~38 MB resident for a mostly idle agent. Rust: register the agent before building any window; create windows only on demand. Must keep TimeoutStartSec=15 budget and READY only after registration.
