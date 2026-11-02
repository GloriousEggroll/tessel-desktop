# tessel-keep-awake — parity with contracts/tessel-keep-awake.md

Rust crate `tessel-keep-awake` (lib `keep_awake` + binary `tessel-keep-awake`). Tests:
`cargo test -p tessel-keep-awake` (unit + `tests/{timer,inhibitor,cli}.rs`, private
`gio::TestDBus` bus that also serves as the "system" bus, with a fake logind) and
`components/keep-awake/tests/run_headless.sh` (`tests/ui.rs` in a private headless
labwc; set `CARGO_WS=<workspace>` to build somewhere other than the repository root).

| Contract item | Status | Test |
|---|---|---|
| §1 app id `org.tessel.KeepAwake`, `HANDLES_COMMAND_LINE`, `IS_LAUNCHER` unless `--background` | done (`gio::Application` subclass) | `cli` (launcher loop, second `--background`) |
| §1 `local_command_line` passes everything to the primary; remote `print_literal` | done | `cli` (`--status` stdout from primary) |
| §1 launcher with no primary: no backend, exit ≠ 0 | done (GLib "Failed to register", rc 1, same as Python) | `cli`, `ui::launcher_without_primary_never_creates_a_backend` |
| §1 SIGTERM/SIGINT → quit, FD released | done (`compat::unix_signal_add_local`) | `cli` (SIGTERM → rc 0, pipe EOF) |
| §1 CLI flags, mutual exclusion, prefixes, `int()` parsing, `--popup-offset` ≥ 0, exit 2, help/usage text | done (argparse twin; usage wraps like argparse at 80 cols; byte-identical with prog `keep_awake.py`) | `args::tests::*`, `cli` |
| §1 local parse first (errors on caller's stderr), re-parse in primary | done | `cli` |
| §1 `--popup` / no mode / `activate` → popup; new (edge, offset) recreates, same → `activate()` | done | `ui` |
| §1 `--enable/--disable/--toggle` never open the popup | done | `ui`, `cli` (empty stderr, no display) |
| §2.1 `Controls`: `GetState`, `SetEnabled(b)→(bs)` ok = no error, `Changed` | done (tessel-core `SettingsService`) | `cli` |
| §2.1 `Duration`: `GetState`, `SetDuration(u)`, error `org.tessel.KeepAwake.Duration.Error` "Unsupported Keep Awake duration.", `Changed` after SetDuration | done; backend changes emit `Changed` on `Controls` only, exactly like Python | `cli` |
| §2.2 logind watch, `Inhibit("idle:sleep","Keep Awake","Keep this desktop awake until turned off","block")`, NO_AUTO_START, 10 s, FD CLOEXEC | done (gio, `call_with_unix_fd_list`) | `inhibitor::real_unix_fd_transfer_on_private_bus`, `cli` |
| §2.2 vanish → FD closed, "System power management is unavailable."; owner change → re-acquire if requested | done | `inhibitor` (fake + real bus), `cli` |
| §2.2 bus `closed` → detach, retry every 2 s; bus-get failure text | done | not tested (needs killing a bus daemon mid-run) |
| §2.3 status JSON keys/order/classes/untranslated literals, `json.dumps` escaping | done | `status::tests::*`, `cli` |
| §3 timer.json path, `{"duration": N, "until": F\|null}\n`, dir 0700, `.timer-*` temp + rename, write errors ignored | done (`suite_core::clock::atomic_write`, adds fsync) | `timer` |
| §3 load rules (invalid duration → 0, until only numeric and future) | done | `timer::load_rules` |
| §3 clock prefs 12/24 h end time, leading 0 stripped | done (`ClockPreferencesStore`) | `ui::tests::durations_and_end_time_format` |
| §4 restore unexpired timer on start; "until turned off" not restored | done | `timer`, `cli` |
| §4 set_enabled / set_duration / timer `max(1,min(60,int(left)+1))` / expiry / forget when off elsewhere | done | `timer` |
| §4 generation counter, stale replies closed, denied → requested=false + error, immediate failure | done | `inhibitor` |
| §5 popup widgets, strings, CSS (APPLICATION priority, removed on unrealize), classes `tessel-keep-awake popup-surface card row-title row-detail status error` | done | `ui`, `ui::tests::subtitle_and_status_texts` |
| §5 PopupController (layer shell, `desktop-applet`, click-away, Escape), theme attach | done (tessel-gtk) | `ui` |
| §5 sensitivity rules | done | `ui` |
| §5 embedded mode + `KeepAwakeClient` | done in Rust (`KeepAwakePopup::new(.., embedded=true)`, `client::KeepAwakeClient`); Global Settings still imports the **Python** modules | `cli` (client follows state, `request_duration`, unavailable text) |
| §7 i18n via `tr!` | done | English checked; catalogs read from suite locale dir |
| §11 lazy GTK | done: primary is `gio::Application`; `gtk::init()` on the first popup | measurement below |

## Deliberate differences
- Popup window is a plain `gtk::Window` (no `GtkApplication` exists until GTK is
  needed); the app hold keeps the daemon alive. Same title/classes/layer surface.
- The popup object is kept across dismiss (Python drops its reference on
  unrealize and leaks the hidden window); a later `--popup` at the same
  position reuses it, which is what Python shows as well. CSS provider is
  re-added if the window is realized again.
- `timer.json` `duration` values `30.0`/`false` are normalised to ints (Python
  would echo them back as-is). Non-finite `until` is ignored (Python would crash).
- Daemon without a display keeps running; Python's Gtk.Application would abort.
- argparse colour output (Python 3.14 on a tty) is not reproduced.
- Rust uses gio for D-Bus (not zbus): GApplication command-line forwarding and
  tessel-core's SettingsService are gio already, and FD passing needs no extra runtime.

## Gaps
- Bus `closed` → 2 s reconnect path is implemented but untested.
- `print_literal` needs gio `v2_80` (enabled in this crate; system GLib 2.88).

## Measurements (headless labwc, private bus, fake logind, release build)
| | Rust | Python |
|---|---|---|
| binary | 2.0 MB stripped | — |
| name registered | 42–76 ms | 195–273 ms |
| `--status` reports `ready: true` | 88–130 ms | 431–654 ms |
| idle RSS / PSS / private dirty | 27.4 / 6.4 / 3.8 MB | 70.9 / 34.2 / 26.2 MB |
| after first popup RSS / PSS / private dirty | 60.6 / 23.7 / 13.4 MB | 84.9 / 41.6 / 29.6 MB |

Idle RSS is mostly clean shared library pages (GTK is linked but not initialised).
