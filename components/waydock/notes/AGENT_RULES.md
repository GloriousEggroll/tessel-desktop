# Rules for every waydock sub-agent (read fully before starting)

Project: Rust port of ~/btmanager/waydock (Python, READ-ONLY: never edit anything under ~/btmanager).
Workspace: /home/tcrider/nobara-de-rust. Crate: components/waydock (lib `waydock` + bin `waydock`).
Read first: tools/PORTING.md, PLAN.md, tools/headless.md, crates/README.md (suite-core/suite-gtk/
suite-wayland APIs — reuse them, do not re-port), contracts/waydock.md (the contract; the section(s)
for your module are binding).

## Ownership
- You own ONLY the files/directories listed in your task. Other agents edit the same crate in
  parallel. Never edit lib.rs, main.rs, another directory's files, the root Cargo.toml, crates/*,
  or other components.
- components/waydock/Cargo.toml is shared: you may ADD lines (a dependency under [dependencies] /
  [dev-dependencies], or a [[test]]/[[bin]] section at the end) with a single small Edit; never
  reformat or remove others' lines. Prefer workspace deps (suite-core, suite-gtk, suite-wayland,
  glib, gio, gtk, gtk4-layer-shell, serde_json, libc, regex) and libs already used by the suite.
- D-Bus: use gio (gio::DBusConnection / DBusProxy / register_object / own_name), the suite
  convention — NOT zbus. Tests on a private bus only (gio::TestDBus or a private dbus-daemon).
- If the crate fails to build because of a file you do not own, wait (sleep 60 via a background
  command or just retry later) — never "fix" another agent's file. Keep YOUR modules compiling at
  all times (write in increments; run `cargo build -p waydock` / `cargo test -p waydock --lib <yourmod>` often).
- Use `cargo test -p waydock --lib <module path filter>` to run only your tests. Workspace target/
  is shared; a cargo lock wait is normal.

## Hard safety
- export TMPDIR=/run/user/1000/cbt/rust-waydock/<your-agent-letter> (mkdir -p it). Put scratch there.
- Never touch the live session: WAYLAND_DISPLAY=wayland-0 in /run/user/1000, the real session bus,
  running processes. Never restart/kill waydock/wayshell/labwc or anything (no pkill, no
  systemctl --user start/stop/restart, no sudo/pkexec). Never run installers with --apply.
- Never write real ~/.config, ~/.cache (palette.json!), ~/.local, gtk.css, rc.xml, settings.ini,
  gsettings, portal settings. All file-writing code must take injectable paths and tests must use
  TMPDIR copies; GSettings/portal/gsettings/labwc --reconfigure via injectable fakes.
- Never take over a running StatusNotifierWatcher outside a private bus.
- Never launch real apps: process spawning goes through injectable spawners in tests.
- GUI tests only in a private headless labwc (tools/headless.md recipe; run.sh must begin with the
  case guard line). Keep socket paths short.
- Do not git commit.

## Porting standard
Port behaviour fully (not a subset): every branch, string (via suite_core::tr!), error text, JSON
byte format, CSS class. Port the listed Python tests as Rust tests (#[cfg(test)] unit tests inside
your modules, or files under components/waydock/tests/<yourprefix>_*.rs). Where Python output bytes
matter (JSON, CSS, rc.xml), add parity tests comparing against the Python module run with
`python3 -I` on fixtures in TMPDIR (import from ~/btmanager/waydock via sys.path, read-only).
At module top, write a doc comment describing the public API other agents will call.
Write your PARITY notes to components/waydock/notes/PARITY-<letter>.md (contract item → done? →
test name; honest gaps). Your final message: <= 300 words: files, public API summary, tests +
results, gaps, anything the integrator must wire.
