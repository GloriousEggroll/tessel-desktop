# tessel-prompt parity (wayshell_prompt.py + install_prompts.py)

Binary `tessel-prompt` (src/main.rs → `wayshell_prompt::run`), library `wayshell_prompt`
(`cli`, `window::PromptWindow`, `install`), read-only preview binary `tessel-install-prompts`.
Tests: `cargo test -p tessel-prompt`; widgets: `tests/run_headless.sh`; end-to-end +
measurements: `tests/measure_headless.py` inside `../tessel-theme/tests/headless.sh 1 …`.

| Contract item | Status | Test |
|---|---|---|
| CLI: exactly one of `--message TEXT` / `--message-stdin` / `--error`; `--accept-label` (tr Yes), `--decline-label` (tr No), `--destructive`; argparse 3.14 semantics (abbreviations, `=`, `-hX`, `--`, negative-number values, error order, usage wrapping, help text, exit 2/0) | done | `python_parity` (44 argv cases: namespace, exit code, stdout, stderr byte-equal), `cli_grammar` |
| exit codes Yes 0 / No 1 / dismiss 254; error mode Close/Escape/close → 0 | done | `test_confirmation_default_and_each_response…`, `test_escape_and_window_close_never_approve`, measure (real binary) |
| SIGTERM acts as close (254, or 0 with `--error`); signal source removed only if still present | done | measure_headless.py (`--message`, `--error`, `--message-stdin`) |
| `NON_UNIQUE` GApplication `org.tessel.Prompt`, run with argv[0] only; WayshellTheme(None) created at activate, closed after run | done | measure (app_id seen by foreign-toplevel), `test_prompt_follows_live_shell_palette` |
| window: `.tessel-prompt`, titles "Configuration Error"/"Confirm Action", 540 resizable / 440 fixed, headerbar `:close`, icon 32px `.prompt-icon`, `.prompt-heading`/`.prompt-body` (first line / rest, body hidden when empty, no markup), `.prompt-actions`, CSS at APPLICATION | done | `test_message_is_heading_and_icon_is_symbolic`, `test_escape…` |
| accept `.suggested-action` default; `--destructive` → `.destructive-action`, decline is default; focus on default | done | `test_destructive_accept_is_styled_but_not_default`, `test_confirmation…` |
| error mode monospace TextView (200–420 px, word-char wrap), only "Close" | done | `test_error_stdin_streams_and_retains_only_bounded_text` |
| stdin: non-blocking, 8×8 KiB per wakeup, ≤65536 kept, truncation note, `--message-stdin` strips one trailing `\n`, UTF-8 replace, buttons insensitive until EOF and when truncated, failure text "Could not read message: [Errno N] …" | done | `test_stdin_message_keeps_confirmation_disabled_until_complete`, `test_stdin_truncated_message_stays_disabled`, `test_error_stdin…`, `test_unreadable_stdin_reports_error` |
| install: delimiter `"WAYSHELL_PROMPT_"+sha256(launcher+"\0"+"\0".join(messages))` + `_END` loop (Python `splitlines`) | done | `python_parity` (transform bytes over 45 rc.xml cases + 6 direct seeds); live rc.xml copy → no change |
| install: rc.xml `<core>` rewrite of only promptCommand/errorCommand, comments masked, `<core/>` expanded / created, `"$HOME"/…` + `sh -c` error form, review errors, idempotent | done | `test_only_commands_change_and_reinstall_is_idempotent`, `test_launcher_in_home_is_written_portably`, `test_core_creation_and_self_closing_commands`, `test_review_errors`, `python_parity` |
| punctuation literal through sh | done | `test_prompt_punctuation_is_literal_to_shell` (fake launcher in $TMPDIR) |
| `org.tessel.Prompt.desktop` (Name + 30 Name[xx]) | done | `python_parity` (byte-equal with shared/locale), `test_plan_is_read_only_and_has_no_services` |
| plan is read-only, no services | done | `test_plan_is_read_only_and_has_no_services` |
| `apply()` with backup/rollback | **not ported** (installer-owned; this crate is preview-only by design) | — |

Results: unit 2/2, config 9/9, parity 1/1, headless UI 9/9, end-to-end measure: no failures.

Notes
- Usage/help print `prog` = basename(argv[0]) (`tessel-prompt`; Python showed `wayshell_prompt.py`). argparse 3.14 colour output on a TTY is not reproduced.
- Upstream bug (both versions, same GTK): a mapped `--message-stdin` prompt with ~64 KiB of text grows past cairo's surface limit and SIGSEGVs on render (verified headless for Python and Rust). Not fixed, to keep parity.
- `python_launcher_script()` reproduces the old launcher; `launcher_script(binary)` is the Rust one.
