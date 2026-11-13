# tessel-login (Tessel Login) — external contract

Source: `~/tessel-bluetooth/tessel-login/` (refs `file:line` there). A greetd greeter: GTK4 +
gtk4-layer-shell UI running as the unprivileged `greetd` user inside a private labwc,
talking to greetd over `$GREETD_SOCK`; plus a root appearance helper used by Global
Settings, and a one-shot post-login desktop fade. **SECURITY-CRITICAL.**

Python files (installed to `/usr/local/lib/tessel-login/`, all root-owned):
`login_ui.py` (greeter), `login_protocol.py` (greetd IPC), `login_model.py` (appearance,
sessions, users), `login_settings_helper.py` (root helper), `login_transition.py` (Fade),
`session_transition.py` (desktop fade), `start-greeter`, `start-session`, `README.md`, plus
copies of `lock_visuals.py lock_palette.py lock_config.py gtk4_utils.py suite_i18n.py
suite_catalogs.py language_data.py osk_lock.py osk_keyboard.py osk_config.py osk_devices.py
tessel-keyboard-input` (install_login.py:107-118).

## 1. Threat model

| Actor / asset | Rule |
|---|---|
| Greeter process | Runs as `greetd` (tessel-config.toml `[default_session] user = "greetd"`), in its own `dbus-run-session` bus, under a private labwc (`--config-dir /etc/tessel-login/labwc`, `XDG_CONFIG_HOME=XDG_CONFIG_DIRS=/etc/tessel-login`, `umask 077`). It has **no privilege**: it never authenticates, never reads shadow, never runs `su`/PAM. |
| greetd | Owns PAM (`service = "tessel-login"`), account checks, credentials, logind registration and user session creation. The greeter only relays prompts/answers. |
| Session start | `start_session` is sent **only** from state `ready`, which is reached **only** by a `success` reply to a `create_session`/`post_auth_message_response` of the **current generation**; the UI may call `start_session(generation)` only with the generation captured at the `authenticated` event (after the 250 ms fade). Any stale, cancelled or mismatched generation is a no-op. |
| Fail closed | Malformed frames (size 0 or > 1 MiB, non-JSON, non-object, `type` not in `success/error/auth_message`), socket loss, connect/IO timeouts, unknown `auth_message_type`, or a non-`success` reply in reset/start phases → state `error` (or `idle` for unsupported prompts) with a generic message; never a session. A GUI/compositor crash cannot start a session (greetd needs `start_session`). |
| Secrets | The password/response is never logged, never in argv/env/files, never in error text (all IPC errors map to one fixed string). Entries: `enable_undo=False`, cleared on submit, cancel, user switch, idle, hide and cleanup. **Rust:** the entry uses `GtkPasswordEntryBuffer` (non-swappable secure memory, wiped on delete); the text is copied once out of GTK (`gtk_editable_get_chars`, whose C buffer is wiped then freed) into a `Zeroizing<String>`; the serialized request frame is a `Zeroizing<Vec<u8>>` with exact pre-computed capacity (no reallocation copies); both are wiped on drop. *Limitation:* GTK's GtkText/input-method layers may keep transient copies while typing; the kernel socket buffer is outside our control. Python could not wipe anything (immutable `str`). |
| Process hardening | `RLIMIT_CORE = (0, 0)` and `prctl(PR_SET_DUMPABLE, 0)`; if prctl fails: exit with `Unable to protect the authentication process` (login_ui.py:738-741). tessel-login.service also sets `LimitCORE=0`, `UMask=0077`. |
| Root login | PAM `tessel-login`: `auth required pam_succeed_if.so user != root quiet` (pam-tessel-login:3). |
| Session command injection | greetd joins `cmd` with spaces and runs `/bin/sh -c`. `Session.request()` passes exactly one `cmd` element: `shlex.join(['/usr/local/lib/tessel-login/start-session', *argv])` — every argument shell-quoted; Exec field codes (`%f` etc.) make an entry ineligible; `%%` → `%`. Sessions come only from root-owned `/usr/local/share/wayland-sessions` then `/usr/share/wayland-sessions`. |
| Root helper | `login_settings_helper.py` is run by `pkexec` (as root) with JSON from the logged-in user on stdin: euid must be 0 and argv must be exactly the program; stdin ≤ 32768 bytes; exact key sets; full value validation; optimistic concurrency (`expected`); the session must be installed; the wallpaper is fully resolved (symlinks), must be a regular, world-readable image whose every ancestor is world-searchable; the destination is fixed (`/etc/tessel-login/appearance.json`), refused if it is a symlink, written via `mkstemp` (O_EXCL|O_NOFOLLOW, 0600) in the same root-owned directory + `fsync` + `chmod 0644` + `os.replace` (atomic). Never starts services, never touches PAM. *TOCTOU:* the wallpaper checks are advisory (the greeter only *reads* the image as `greetd`); the config's directory is root-owned 0755 so the temp/rename cannot be raced by the user. Rust uses `fchmod` on the open fd instead of `chmod(path)` (same result, no path re-resolution). Test builds inject the config path and session roots via `cfg(test)` only — the production binary has no env/flag overrides. |
| Polkit | `50-tessel-login-power.rules`: only `subject.user == "greetd" && subject.local && subject.active` and only `org.freedesktop.login1.{power-off,reboot,suspend}` → YES. No inhibitor bypass, no multi-user actions. |
| Compositor | Private labwc config: one keybind `F35 → None` and a no-op root right-click so no default/desktop shortcuts exist; `<numlock>on</numlock>` so number-pad passwords work; autostart is a comment only; greeter is `--session` child so labwc exits when it exits. |
| Faillock | `/etc/security/tessel-login-faillock.conf`: deny=5, fail_interval=900, unlock_time=300, even_deny_root, root_unlock_time=300, silent, dir=/run/faillock. |

## 2. Entry points

### 2.1 Greeter `login_ui.py`
- argparse: `--preview` (store_true, "Windowed preview; no login or power actions"), `--config PATH` (default `/etc/tessel-login/appearance.json`). Description `Tessel Login — Python Wayland login greeter`. Unknown args → argparse error exit 2. **Rust:** exact flags only (no prefix abbreviations — argparse would accept `--prev`; Rust accepts `--preview`, `--config X`, `--config=X`, `-h/--help`).
- Order: parse → `setrlimit(RLIMIT_CORE,(0,0))` → `prctl(4,0…)` (fail → message, exit 1) → `Appearance.load(config)` (exception → traceback, exit 1; Rust: message on stderr, exit 1) → `Gtk.Application('org.tessel.Login', NON_UNIQUE).run([argv0])`.
- Non-preview activation requires layer-shell support **and** non-empty `$GREETD_SOCK`, else `RuntimeError('The greeter requires Wayland layer-shell and a greetd login socket')` (Rust: printed, exit 1).
- Installed launch: `start-greeter` → `exec dbus-run-session -- labwc --config-dir /etc/tessel-login/labwc --session '/usr/bin/python3 -I /usr/local/lib/tessel-login/login_ui.py'`.

### 2.2 Root helper `login_settings_helper.py` (§6)
`pkexec /usr/bin/python3 -I /usr/local/lib/tessel-login/login_settings_helper.py` (Global Settings `HELPER_COMMAND`).

### 2.3 Desktop fade `session_transition.py`
Appended by `Session.request()` to labwc sessions: `--startup '/usr/bin/python3 -I /usr/local/lib/tessel-login/session_transition.py'` (§5).

## 3. greetd IPC (login_protocol.py)

### 3.1 Framing
- Unix stream socket `$GREETD_SOCK`. Frame = `struct.pack('=I', len)` (**native-endian** u32, 4 bytes) + UTF-8 JSON payload. `MAX_FRAME = 1024*1024`.
- Request bytes = `json.dumps(request, ensure_ascii=False).encode('utf-8')`: separators `", "` and `": "`, keys in insertion order, non-ASCII literal UTF-8, escapes `\" \\ \n \r \t \b \f`, other C0 → `\u00xx` (lowercase hex); DEL not escaped. Payload > MAX_FRAME → `ValueError('Login request is too large')` (nothing sent).
- `channel.sendall(header + data)` (one buffer).
- Reply: `receive(4)` → length; `not 0 < length <= MAX_FRAME` → `ValueError('Invalid login backend message size')`. `receive(n)` loops `recv`; EOF → `ConnectionError('Login backend disconnected')`. `json.loads` of payload; not a dict, or `type` not in `('success','error','auth_message')` → `ValueError('Invalid login backend response')`.
- Requests (exact bytes):
  - `{"type": "cancel_session"}`
  - `{"type": "create_session", "username": "<u>"}`
  - `{"type": "post_auth_message_response", "response": "<r>"}` (prompt answers)
  - `{"type": "post_auth_message_response"}` (acknowledging `info`/`error` messages — **no** `response` key)
  - `{"type": "start_session", "cmd": ["<shell command>"], "env": ["XDG_SESSION_TYPE=wayland", "XDG_SESSION_DESKTOP=<ident>", "XDG_CURRENT_DESKTOP=<desktops>", "DESKTOP_SESSION=<ident>"]}`

### 3.2 Client(path, dispatch, changed)
- One worker thread (`ThreadPoolExecutor(max_workers=1, 'greetd-ipc')`) runs requests **in submission order**; results go back with `dispatch(_result, serial, phase, response)` (GUI: `GLib.idle_add`; Rust: async-channel drained by a `glib::spawn_future_local` loop). One persistent socket per generation; the UI thread never blocks.
- Per request: `serial = generation` at submit. Worker: under mutex, if `closed or serial != generation` → result `None` (ignored); if no socket, create one and store it (still under mutex); connect **outside** the mutex with timeout 5 s, then per-operation timeout 120 s; `exchange`. Any `OSError`/`ValueError` → response `{'type':'error','description':'The login service is unavailable. Please try again.'}` (no request data, no exception text). **Rust** additionally re-checks after connect that the socket is still the current one (Python closes the fd on disconnect; Rust shuts it down) and treats a mismatch as that same error.
- `_disconnect()`: under mutex take socket, `shutdown(SHUT_RDWR)` (errors ignored), close.
- States: `idle → resetting → authenticating → prompt ⇄ authenticating → ready → launching → started`; `error` from anywhere.
- `begin(username, session)`: no-op if closed or `launching`. Invalid username (not str, empty, > 256 UTF-8 bytes, contains NUL) → `cancel()`, state `error`, `changed('error','Enter a valid username.')`, nothing sent but the cancel. Else generation += 1, disconnect, store username/session, state `resetting`, `changed('busy','Preparing sign-in…')`, request `cancel_session` (phase `reset`) — always resets greetd first, even after a crash.
- `respond(text)`: only in `prompt`. Invalid (NUL or > 4096 UTF-8 bytes) → `cancel()`, state `error`, `changed('error','The authentication response is invalid or too long.')`. Else state `authenticating`, `changed('busy','Signing in…')`, request `post_auth_message_response` + response (phase `auth`).
- `cancel()`: no-op if closed or `launching`; generation += 1, disconnect, state `idle`, request `cancel_session` (phase `cancel`) on a fresh connection.
- `start_session(generation)`: only if not closed, state `ready` and `generation == self.generation` → state `launching`, request `session.request()` (phase `start`). Second call is a no-op.
- `_result(serial, phase, response)` (returns False for idle_add): ignore if closed, stale serial or `None`; phase `cancel` → ignore. Then by `type`:
  - `error`: state `error`; message `Sign-in failed. Check your credentials and try again.` if `error_type == 'auth_error'`, else `str(response.get('description','Sign-in could not be completed.'))`.
  - not `success` and phase ≠ `auth` (i.e. `auth_message` in reset/start): state `error`, `Unexpected response from the login service.`
  - phase `reset` (success): state `authenticating`, request `create_session` (phase `auth`).
  - `auth_message`: `auth_message_type` `visible`/`secret` → state `prompt`, `changed(type, str(auth_message or ''))`; `info`/`error` → `changed('message', text)` and request bare `post_auth_message_response` (phase `auth`); anything else → `cancel()` then `changed('error','Unsupported authentication prompt.')`.
  - phase `auth` success → state `ready`, `changed('authenticated','Starting your session…')`.
  - phase `start` success → state `started`, `changed('started','')`.
- `close()`: closed, generation += 1, disconnect, executor shutdown (queued work cancelled).

## 4. Model (login_model.py)

### 4.1 Appearance — `/etc/tessel-login/appearance.json`
Fields/defaults: `picture="/usr/share/wallpapers/tessel/slate-3840x2160.png"` (`tessel_core::wallpaper::LOGIN_PICTURE`, the dark Slate render from `tessel-wallpapers`; for mode `picture` each monitor shows the Slate sibling nearest its shape, see `render_for_screen`; a missing file draws `background_color`), `placement="fill"`, `background_color="#161820"`, `blur_strength=50`, `transparency=5`, `idle_seconds=30`, `clock_24h=false`, `default_session="labwc"`. Missing file → defaults. `from_dict` errors (in order): `Appearance settings must be an object`, `Invalid blur_strength|transparency` (int 0–100, bool rejected), `Invalid idle_seconds` (10–3600), `Invalid wallpaper placement` (fill/fit/stretch/center), `Wallpaper must be an absolute, system-readable path` ("" or starts with "/"), `Invalid clock or session setting`, `Invalid background color` (`#` + 6 hex). Unknown keys ignored. (Ported in `crates/locking::login_model`.)

### 4.2 Sessions
`sessions(roots=(/usr/local/share/wayland-sessions, /usr/share/wayland-sessions))`: sorted `*.desktop` per root, first stem wins, configparser `[Desktop Entry]`, `Exec` via `shlex.split` (POSIX, no comments; inside double quotes only `\"` and `\\` are escapes), skip Hidden/NoDisplay true, Type ≠ Application, any `%` field code, `which(argv0)`/`which(TryExec)` missing; `desktops = ':'.join(DesktopNames split ';')` (default stem); sorted by `name.casefold()`.
`Session.request()`: if `Path(argv[0]).name == 'labwc'` and no `-s`, `--startup` or `--startup=…` argument → append `--startup`, `/usr/bin/python3 -I /usr/local/lib/tessel-login/session_transition.py`. `cmd = [shlex.join(('/usr/local/lib/tessel-login/start-session', *argv))]`; env as §3.1.

### 4.3 Users
`users()`: `UID_MIN` from `/etc/login.defs` (default 1000; last parsable line wins, parse error stops scanning; an integer too large for i64 saturates — Python's `int` is unbounded), all `pwd.getpwall()` with `UID_MIN <= uid < 65534` and shell not in `('', '/sbin/nologin', '/usr/sbin/nologin', '/bin/false')`; `User(username=pw_name, name=gecos.split(',')[0].strip() or pw_name)`, sorted by `name.casefold()` (stable).

## 5. Transitions
- `login_transition.Fade(widgets, start, end, duration_ms, finished)`: sets every widget's opacity to `start`; tick callback on the first widget; first frame time is t0; `fraction = clamp((now-t0)/(max(1,duration)*1000))`, smoothstep `f²(3-2f)`; at 1 → complete. Deadline `GLib.timeout_add(duration_ms + 150)` completes even if no frames are drawn. `cancel()` removes both and drops `finished`. `complete()` = cancel, set `end`, call finished once.
- Greeter: on `authenticated` → transitioning; every window clears its entry, hides the OSK, form insensitive, `.session-fade` cover visible; `Fade(covers, 0, 1, 250, → client.start_session(generation))`. `error` while transitioning restores (cover hidden, form sensitive, re-picks an active window if unplugged). `started` → `app.quit()`.
- `session_transition.py`: `Gtk.Application('org.tessel.LoginTransition', NON_UNIQUE)`; quits at once if layer-shell unsupported; CSS `window.login-transition { background: black; border-radius: 0; box-shadow: none; }`; one black overlay layer window per monitor (namespace `tessel-login-transition`, OVERLAY, exclusive zone -1, keyboard NONE, all anchors, empty input region on realize), follows monitor hotplug until the fade starts; watches session-bus names `org.tessel.Dock` and `com.nobaraDesktopView`; polls every 100 ms: waits ≥ 0.8 s and until both names are owned, at most 2 s; then `Fade(windows, 1, 0, 400, quit)`. Starts no services.

## 6. Root helper protocol (login_settings_helper.py)
- `euid != 0 or len(argv) != 1` → `Administrator authorization is required`.
- Read `stdin.buffer.read(32769)`; > 32768 → `Appearance request is too large`.
- `CONFIG.is_symlink()` → `Refusing to replace a symbolic link`.
- `json.loads(raw)` (Python JSONDecodeError/UnicodeDecodeError text; **Rust** prints serde_json's wording for syntax errors and accepts UTF-8 only — Python also auto-detects UTF-16/32), then `Appearance.load()` of the current file, then `sessions()`.
- `validate_request`: not a dict or keys ≠ `{expected, appearance}` → `Invalid appearance request`; `expected != asdict(current)` (Python equality: `0 == False`, `50 == 50.0`) → `Login settings changed elsewhere. Reload the page and try again.`; `appearance` not a dict or keys ≠ the 8 fields → `Unknown appearance setting`; `Appearance.from_dict` errors; `default_session` not an installed ident → `Choose an installed Wayland session`; non-empty picture: `Path.resolve(strict=True)` (Python 3.14 realpath: `OSError` text `[Errno N] <strerror>: '<component path>'`, ENOTDIR for a non-directory component, ELOOP for loops), suffix (`os.path.splitext` rules) lower not in `.png .jpg .jpeg .webp .bmp .gif` → `Choose a wallpaper image; login backgrounds do not support video`; not a regular file or not `S_IROTH` → `Choose a system-readable wallpaper image`; any ancestor without `S_IXOTH` → `Wallpaper folders must be accessible to the login screen`.
- Write: `CONFIG.parent.mkdir(0o755, parents=True, exist_ok=True)`; `mkstemp(prefix='.appearance-', dir=/etc/tessel-login)`; content `json.dump(asdict(appearance), indent=2)` + `"\n"` — i.e. `ensure_ascii=True` (`\uXXXX`, surrogate pairs), item separator `","` + newline, key separator `": "`, field order as §4.1; flush; fsync; chmod 0644; `os.replace`; on any failure the temporary is unlinked.
- Errors (`ValueError`, `OSError`): `str(error)` + `\n` on stderr, exit 1; success: no output, exit 0.

## 7. Greeter UI (login_ui.py)
- One `LoginWindow` per monitor (preview: first monitor only, 1200×820 window, close button from `preview_close_button(app.quit)`, close-request allowed; otherwise close-request refused). Layer shell: namespace `tessel-login`, OVERLAY, keyboard EXCLUSIVE, exclusive zone -1, all four anchors. Hotplug: windows added/removed; removing the active window hides the login.
- Global CSS `STYLE` (+ `family_css('tessel-login')`) at application priority; `gtk-application-prefer-dark-theme`; `Papirus-Dark` icon theme if installed. Per-window palette provider (APPLICATION + 10) derived from the wallpaper colour (`colorsys` HSV accent `s∈[.3,.65]`, `v=.85`; tint `c*.18+15`; opacity `(100-transparency)/100`), class scope `login-<id>`. Accessibility CSS (LARGE_TEXT / HIGH_CONTRAST) at APPLICATION + 20; large text also sets `gtk-xft-dpi` 96·1024·1.25.
- Widgets/classes: window `.tessel-login`; idle clock `.clock.family-clock`, `.date.family-date` (non-targetable); form 400 px wide inside a ScrolledWindow inside `login_content` overlay inside `LoginLayer`; `Avatar` (margin-bottom 8); user DropDown (`.family-user-choice`, users + `Other user…`, a11y label `User`, tooltip `Choose user`); username Entry (`Username`, max 256, hidden unless "Other"); prompt label (hidden for `password:`/`password` casefold); password Entry (`.family-password`, max 4096, PASSWORD purpose, a11y label `Password`/prompt) with overlay eye toggle (`.eye.family-eye`, tooltip `Show password`/`Hide password`); session DropDown (`.family-session`, icon `computer-symbolic` in the closed pill, `No Wayland sessions installed` when empty, label/tooltip `Desktop session`, default from appearance); `Caps Lock is on` caption; status caption; action buttons 44×44 `.action.family-action` in one SizeGroup: Sign in (`.suggested-action`, `submit_icon()` 20×20), Sleep (Adwaita `weather-clear-night-symbolic`), Restart, Power off, Accessibility MenuButton, OSK button (from `LockKeyboard`); power confirmation `.confirmation` (`Restart this computer?`/`Power off this computer?`, `Cancel`, `.destructive-action` `Restart`/`Power off`); `.session-fade` cover.
- Accessibility popover switches: `Screen reader` (only if `orca` on PATH; starts `orca --replace` in a new session with null stdio, terminates on off/shutdown, never in preview), `Large text`, `High contrast`, `On-screen keyboard`; mirrored on every monitor.
- Idle/wake: starts on the clock; pointer motion (enter counts only > 0.35 s after map), click, scroll or key → reveal on all monitors, the first wake event is consumed; Escape or `idle_seconds` without activity → hide (cancels greetd conversation, clears entries). Keys pressed on another monitor are forwarded to the active window's focus. Clicking the form on an inactive monitor makes it active.
- `begin`: select window (cancels other conversation, clears other entries); no username → cancel + focus username; no sessions → `Install a Wayland desktop session to sign in.`; preview → fake `secret` `Password:` prompt; else `client.begin`. Changing user/session restarts.
- Submit: preview → clear + `Preview only — authentication and power actions are disabled.`; prompt on active window → take text, clear entry, `respond`; otherwise restart (`begin`).
- Events: prompt → entry sensitive only on active window, eye visible for `secret`, visibility for `visible`; status text otherwise; `error` clears entry, hides prompt, focuses the sign-in button (retry restarts PAM, nothing cached).
- Power (logind on the **system** bus): `CanSuspend/CanPowerOff/CanReboot` (3 s) → enabled only on `yes`; buttons insensitive in preview; restart/power-off need confirmation; `Suspend|PowerOff|Reboot(false)` (10 s); failure → reveal + `The power action was denied or is currently unavailable.`; never during launching/started.
- Tick every 1 s: idle timeout, `family-compact` below 900 px, clock `%H:%M` or `%I:%M %p` without leading zero, `long_date`, Caps Lock, power sensitivity.
- OSK: `LockKeyboard` types only into the local entry (never through desktop keyboard IPC).

## 8. Installed files (install_login.py `payload()`; owner root:root)
| Path | Mode | Source |
|---|---|---|
| `/usr/local/lib/tessel-login/{login_ui,login_model,login_settings_helper,login_protocol,login_transition,session_transition}.py`, `README.md`, shared/lock/OSK modules | 0644 | repo |
| `/usr/local/lib/tessel-login/start-greeter`, `start-session`, `tessel-keyboard-input` | 0755 | repo |
| `/etc/systemd/system/tessel-login.service` | 0644 | `Type=simple`, `ExecStart=/usr/bin/greetd --config /etc/greetd/tessel-config.toml`, Restart=always, LimitCORE=0, UMask=0077, `Alias=display-manager.service`, Conflicts getty@tty1/greetd/sddm/gdm/lightdm/plasmalogin |
| `/etc/greetd/tessel-config.toml` | 0644 | vt=1, switch, `service="tessel-login"`, `source_profile=true`, `runfile=/run/tessel-login.run`, greeter `command=/usr/local/lib/tessel-login/start-greeter`, `user="greetd"`, `service="greetd-greeter"` |
| `/etc/tessel-login/labwc/rc.xml` 0644, `/etc/tessel-login/labwc/autostart` 0755 | | private compositor config |
| `/etc/polkit-1/rules.d/50-tessel-login-power.rules` | 0644 | §1 |
| `/etc/pam.d/tessel-login`, `/etc/pam.d/tessel-lock` | 0644 | repo |
| `/etc/pam.d/tessel-login-auth` | 0644 | generated from `/etc/pam.d/system-auth` (validated) |
| `/etc/pam.d/tessel-lock-password`, `tessel-lock-fingerprint` | 0644 | generated when applicable |
| `/etc/tessel-login/appearance.json`, `/etc/security/tessel-login-faillock.conf` | 0644 | only if absent |
`tessel-start-greeter`: the greeter's compositor and the greeter itself write to the journal under the identifier `tessel-login-greeter` (`journalctl -b -t tessel-login-greeter`), through `systemd-cat` when it is available; `TESSEL_LOGIN_CONSOLE=1` leaves them on the console.

`start-session`: unsets the greeter's bus vars; if `$XDG_RUNTIME_DIR/bus` is a socket, exports `DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus` and `exec "$@"`, else `exec dbus-run-session -- "$@"`.
The installer never enables/starts a DM; the Rust port does not change any of these files (the swap only changes the greeter command, §9).

## 9. Rust deltas (intentional)
- Binaries: `tessel-login` (greeter), `tessel-login-helper` (no GTK linked), `tessel-session-transition`.
- Swap (not performed): install the binaries root-owned 0755 in `/usr/local/lib/tessel-login/` plus `locale/*.json`; `start-greeter --session '/usr/local/lib/tessel-login/tessel-login'`; `Session.request()` startup command becomes `/usr/local/lib/tessel-login/tessel-session-transition`; Global Settings `HELPER_COMMAND = ["pkexec", "/usr/local/lib/tessel-login/tessel-login-helper"]` (a polkit `exec.path` annotation is not needed: pkexec's default `org.freedesktop.policykit.exec` auth_admin applies to both).
