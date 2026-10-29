# calculator — external contract

Source: `~/btmanager/calculator` (`calculator.py` 130 LOC launcher/GApplication, `calculator_backend.py` 366
parser + store, `calculator_ui.py` 502 window). Standalone app, no background service, no D-Bus API of its own.
Nothing else in the suite imports its Python modules.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installer | `desktop-apps/install.py --only calculator [--apply]` (pkexec, root): copies `calculator*.py` (not tests) + shared `gtk4_utils.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libwaydock-background-blur.so app_menu.py` + `waydock/companion_theme.py` to `/usr/local/lib/desktop-suite/calculator/`; backup under `/var/backups/desktop-suite/standalone-apps-<stamp>/` + `manifest.json`; runs `update-desktop-database` | desktop-apps/install.py |
| Launcher | `/usr/local/bin/calculator` = `#!/bin/sh\n# Desktop suite standalone application\nexec /usr/bin/python3 "/usr/local/lib/desktop-suite/calculator/calculator.py" "$@"` (the marker line lets the installer replace it) | install.py:47-50 |
| .desktop | `/usr/share/applications/org.waydock.Calculator.desktop`: `Name=Calculator`, `Comment=Calculate with standard and scientific functions`, `Exec=/usr/local/bin/calculator` (installer rewrites `Exec=calculator`), `Icon=accessories-calculator`, `Categories=Utility;Calculator;`, `Keywords=math;arithmetic;scientific;calculator;`, `StartupNotify=true`, **`StartupWMClass=org.waydock.Calculator`** | org.waydock.Calculator.desktop |
| GApplication | id **`org.waydock.Calculator`**, flags `HANDLES_COMMAND_LINE` (unique; `do_local_command_line` returns "not handled" so a second process forwards its argv to the primary). `GLib.set_prgname(id)` → Wayland app_id `org.waydock.Calculator`. | calculator.py:56-84 |
| activate | create the window if none/closed, `unminimize()`, `present()`, then idle `wlrctl toplevel focus app_id:<id> title:<title>` retried ≤4× every 100 ms while the window is still current | calculator.py:86-118 |
| Launched by | desktop menu / waydock app grid via the .desktop file only | |

## 2. CLI (argparse, prog `calculator.py`)
`calculator.py [-h] [--scientific] [--check] [expression]`, plus hidden `--quit`.
- `expression` (optional positional): > `MAX_EXPRESSION` (512) characters → `calculator.py: error: argument expression: the expression is too long`, exit 2. Shown and evaluated in the (possibly existing) window.
- `--scientific`: switch the window to Scientific mode (persisted).
- `--check`: prints `{"ready": b, "application_id": "org.waydock.Calculator", "gtk4": b, "layer_shell": b, "calculator": b, "error": "…"}` (json.dumps) without a window; exit 0 if ready else 1. `layer_shell` = `CDLL("libgtk4-layer-shell.so", RTLD_GLOBAL)`, `calculator` = `evaluate("(2+3)^2") == 25`.
- `--quit`: close the window and quit the primary.
- argparse rules: unique `--` prefixes (`--sci`), `-5` is a positional, `--` ends options, `--opt=value` → "ignored explicit argument", extra positionals → `unrecognized arguments: …`; usage/error on stderr, exit 2; `-h` text on stdout, exit 0. The arguments are parsed in the launching process first (errors never reach the primary) and again in the primary's `command-line` handler.

## 3. Files
`$XDG_STATE_HOME/waydock-calculator/state.json` (`XDG_STATE_HOME` if non-empty, else `~/.local/state`):
`{"version": 1, "scientific": bool, "degrees": bool, "memory": str(Decimal)|null, "history": [{"expression": s, "result": s, "degrees": bool, "timestamp": float}, …]}` — `json.dump(ensure_ascii=False)` + `\n`, newest first, ≤100 entries.
- Save: `mkdir(parents, mode 0o700)`, `NamedTemporaryFile(prefix=".state-")` (0600) in the same dir, fsync, `os.replace`; the temporary is always removed; errors set `store.error = str(OSError)`.
- Load: read ≤ 256 KiB+1 (larger → "Saved calculator history is too large."), `json.loads` (Python grammar: NaN/Infinity, big ints, recursion error), non-dict → "Invalid saved calculator history.", `scientific is True`, `degrees is not False`, `history[:100]` (TypeError texts for dict/None/…), per entry skip unless both strings, expr ≤512 / result ≤1200 chars, `bounded(Decimal(result))`, finite `float(timestamp)`, `degrees = bool(…)`; `memory` str ≤1200 → `bounded(Decimal(memory))`. Missing file → defaults; other errors → `store.error` shown as "Could not load saved history: <error>".
- `record()` dedups (expression, result, degrees), prepends, truncates, saves. Memory: MC/MR/MS/M+/M- (28 digits, bounded).

## 4. Arithmetic (calculator_backend.py)
Tokens: numbers `(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?`, names `[a-zA-Z]+` (lower-cased), `+-*/^%()!`; `× ÷ − π **` normalised first; implicit `*` between value-ending and value-starting tokens, but two numbers → error. Limits: 512 chars, 160 tokens, depth 32, |exponent| ≤ 1000, literal exponent ≤ 1000, |adjusted| ≤ 1000, factorial 0..170, exp arg ≤ 2300, trig arg ≤ 1e12. Grammar: `+ -` < `* /` < unary < `^` (right assoc, exponent is unary) < postfix `% !` < atom. Constants `pi`, `e` (28 digits), `ans`. Functions sqrt abs ln log exp sin cos tan asin acos atan (degrees by default; trig via float `math`, result `Decimal(format(x, ".15g"))`, whole result rounded to 15 digits). Decimal context prec 28, Emax 1000, Emin −1000, Underflow trapped. All error texts are English constants translated by the UI (`tr(str(error))`), e.g. "Cannot divide by zero.", "This calculation is outside the supported range.". `format_number`: `0`, plain for adjusted −6..16 with trailing zeros stripped, else `<mantissa>e<exp>`.

## 5. UI
- Window: `Gtk.ApplicationWindow`, title "Calculator", class `calculator`, default 440×(585 scientific | 480), size request 365×−1; history toggle resizes to (735|440)×(620|530). Header bar = `gtk4_utils.headerbar`, app menu (`install_app_menu`, name "Calculator", icon `accessories-calculator`, comments "Standard and scientific calculations"; Keyboard Shortcuts sections "Calculation"/"View").
- CSS (`STYLE`, APPLICATION+31): classes `calculator calculator-display expression result muted calculator-key scientific-key memory-key icon-button history-title history-card history-expression history-result error primary card profile-row`. Palette: `companion_theme.attach(window)`.
- Widgets: mode drop-down (Standard/Scientific), angle drop-down (Degrees/Radians, scientific only), history toggle (`document-open-recent-symbolic`), expression entry (max 512, placeholder 0), result label (selectable, ellipsized, max 28 chars) + copy button, status label (wrap, `error` class), memory row MC MR M+ M− MS Ans, scientific grid (18 keys), keypad (24 keys, `=` has `primary`), √ drawn with cairo, memory status "Memory: <n>", history panel (cards: reuse button, Degrees/Radians, copy, insert; "Clear history"; empty text).
- Keys (capture phase): Ctrl+H history, Ctrl+C copy result (unless text selected), Ctrl+V paste (async, latest request only, stripped), Ctrl+Q close; Alt/Super ignored; ignored while a DropDown has focus; Return/KP_Enter/`=` calculate; Escape/Delete/KP_Delete clear; BackSpace; typed chars `0-9 . + - * / ^ % ( ) ! a-z A-Z π × ÷ −`. App accels `<Control>q`, `<Control>question`.
- Behaviours: after `=`, an operator continues from the answer, anything else starts fresh; `ans` in history is replaced by `(<previous answer>)`; negate/reciprocal wrap the expression or answer; MS/M+/M− use the answer right after `=` or an empty expression, else evaluate the expression; "Copied to clipboard" clears after 1.8 s; close-request → destroy (timers, paste serial, theme, CSS provider).

## 6. i18n strings
Calculator, Standard, Scientific, Degrees, Radians, Calculator mode, Angle units, History (Ctrl+H), Show calculation history, Expression, Result, Copy result (Ctrl+C), Clear memory, Recall memory, Add to memory, Subtract from memory, Store in memory, Insert previous answer, Percent (divide by 100), Backspace, Clear (Esc), Change sign, Calculate (Enter), Square, Power, Square root, Factorial, Reciprocal, History, Clear history, Your calculations will appear here., Reuse calculation: , Copy result, Insert result, Could not load saved history: , Could not save history: , Could not save memory: , This expression is too long., Memory: , Copied to clipboard, the shortcut labels, and every backend error text.
