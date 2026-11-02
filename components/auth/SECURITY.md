# Security notes: Rust polkitman vs Python

## What does not change
- **The trust boundary is the same.** The agent never decides authorization. libpolkit-agent-1's
  `PolkitAgentSession` runs polkit's setuid `polkit-agent-helper-1`. Only that helper talks to PAM
  and reports to polkitd, and only for the cookie polkitd issued. Returning TRUE from the GTask
  ("conversation finished") grants nothing. A compromised or fake agent can deny or annoy, but it
  cannot authorize.
- **Registration is the same.** Flags NONE, subject `PolkitUnixSession` for `$LABWC_PID`
  (otherwise our own pid), object path `/org/tessel/Authentication`. READY is sent only after
  registration succeeded. With no display the agent refuses to start, so it never registers a
  headless agent. Autostart still uses `restart`, which replaces a stale registration.
- **Cancellation works the same way.** It is handled in an idle callback, never inside the
  GCancellable handler, and every request completes exactly once.
- **No logging of secrets.** Nothing reads or writes files. No secret appears in argv, stderr or
  the journal. The error text for a window failure still leaves out request data.

## What differs
| Area | Python | Rust |
|---|---|---|
| Native bridge | `libwayshell-auth-listener.so` loaded with ctypes from the install dir (0644, user-writable dir) | Listener GType registered in-process (`src/listener.rs`). There is no loadable .so for an attacker to swap in the user's dir, but the binary itself lives in that same user-writable dir, so this is not a privilege boundary. |
| Password in the entry | `Gtk.Entry` with a normal `GtkEntryBuffer` (heap; old text is not zeroed) | `GtkPasswordEntryBuffer`: GTK's non-swappable secure memory, zeroed when text is deleted |
| Copy handed to polkit | `entry.get_text()` → immutable Python `str`, never wiped; it lives until GC (and interned copies may persist) | One copy into `Secret` (exact capacity, no reallocation), passed to `polkit_agent_session_response`, then overwritten with volatile writes. The entry is cleared before the response is sent. |
| polkit cookie | Python `str`; `request['cookie'] = ''` only drops the reference | `Secret`, zeroed when the request finishes and on drop |
| Core dumps / ptrace | dumpable (systemd-coredump could capture the heap) | `prctl(PR_SET_DUMPABLE, 0)` at startup: no core files, and other same-uid processes cannot ptrace or read `/proc/<pid>/mem`. This is new hardening; take it out of `main.rs` if debugging ever needs it. |
| Abandoned request | if a Python exception escapes a signal handler, the GTask is never completed and polkitd waits | `PolkitTask::drop` always completes with CANCELLED |
| Bad `$LABWC_PID` | uncaught ValueError, then hangs until TimeoutStartSec | reported as a registration failure; quits without READY |
| Interpreter | python3, PyGObject and a writable `sys.path` (with `~/.local/share/{shared,tessel-dock}` taking precedence) | one binary, linked only to system libraries |

## What stays out of reach
- libpolkit's own copies. `polkit_agent_session_response` formats the response into its own
  buffers to write to the helper, and does not zero them. GTK's input method / GtkText may keep
  transient copies while you type. Both are the same as in Python.
- The BlueZ PIN and passkey entries are normal entries, as in Python. Pairing codes are short-lived
  and are shown on screen anyway.
- Same-uid code that could already replace the binary or the unit is out of scope. Python had the
  same limit.

## Tests never touch real authority
Tests use a private dbus-daemon as the "system bus", a fake `org.freedesktop.PolicyKit1` Authority
and a fake BlueZ, and `FakeSession` stands in for the PAM helper. `ui_headless` refuses to run
unless `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY` and both bus addresses are the private ones. No test
registers on the real system bus or authenticates the real user.
