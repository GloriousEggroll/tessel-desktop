# Brief for component porting agents (read fully first)

1. Read /home/tcrider/nobara-de-rust/PLAN.md, tools/headless.md, crates/README.md and your
   contracts/<component>.md. The Python source of truth is /home/tcrider/btmanager/<component>
   (READ-ONLY — never edit btmanager).
2. Create components/<component>/ as a binary crate (package name = component dir name; binary
   names should mirror the Python entry points, e.g. keep-awake). Use `workspace = true` deps
   that exist in the root Cargo.toml; for anything else put an explicit version in YOUR crate's
   Cargo.toml. Do NOT edit the root Cargo.toml or anything in crates/ (other agents run in
   parallel). If suite-core/suite-gtk lack something, implement it locally in your crate under
   a `compat` module and list it in your report as a candidate to upstream.
3. Port the full behaviour, not a subset: every CLI flag, D-Bus method/signal/property, file
   format, UI surface, CSS class, string (via suite_core tr!), and every subtle behaviour listed
   in the contract. Port the Python tests as Rust tests (unit + integration). For D-Bus use zbus
   (put its version in your crate) and test on a private bus (gio::TestDBus or dbus-daemon
   --session with a private address) — never the real session bus. For UI, use the private
   headless labwc recipe; never the live session.
4. Never install, never touch ~/.config, ~/.local, systemd units, or running processes. No
   sudo/pkexec. Scratch: TMPDIR=/run/user/1000/cbt/rust-<component>. Cargo builds in the shared
   workspace target/ (a lock wait from parallel builds is normal).
5. Write components/<component>/PARITY.md: contract item → implemented? → test name. List any
   gaps honestly.
6. Measure: release binary size, idle RSS and time-to-ready of the Rust binary in the headless
   session vs the Python version run the same way (if the Python one can run headless safely on
   a private bus; otherwise say so).
7. Do not git commit (the parent commits). Final report <= 400 words: done/gaps, tests + results,
   measurements, compat items to upstream, what the installer/swap step must do (paths, units,
   shims at the old .py paths incl. flag strings that desktop-suite-action greps for).
8. Widget visibility: Python `widget.get_visible()` / `widget.props.visible` is the widget's
   OWN flag; port it as gtk-rs `.get_visible()`. gtk-rs `WidgetExt::is_visible()` is
   gtk_widget_is_visible(), which is false whenever any ancestor (or the not-yet-shown window)
   is hidden — use it only where the Python code really called `widget.is_visible()`. Mixing
   them up latches state: `a.set_visible(b.is_visible())` run before the window is shown, or
   with `a` an ancestor of `b`, hides `a` forever (waydock 1b3fc6d). Non-widget getters keep
   their names (`ColumnViewColumn::is_visible()` is its own `visible` property;
   `EntryExt::is_visible()` is the password-visibility flag).
