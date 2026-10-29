# calculator — parity with contracts/calculator.md

Crate `components/calculator`, binary `calculator` (GApplication `org.waydock.Calculator`).
Tests: `env -u TMPDIR cargo test -p calculator` (24 unit tests), `tests/run_headless.sh` (9 GTK checks = all 9
Python `CalculatorUITests`, private nested headless labwc + private D-Bus), `tests/measure_headless.sh`
(Rust vs Python, same compositor; the two idle windows' screenshots are **byte-identical**).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Item | | Test |
|---|---|---|
| App id, HANDLES_COMMAND_LINE, prgname = app_id, one window reused, unminimize/present, idle `wlrctl` raise (≤4 retries) | ✅ | `second_process_reuses_existing_window` (real `calculator` binary as 2nd process on the private bus), measure_headless (`wlrctl … app_id:org.waydock.Calculator`) |
| Installer / launcher / .desktop | ❌ (installer step, not in crate) | — |

## §2 CLI
| Item | | Test |
|---|---|---|
| expression (≤512, error text, exit 2), `--scientific`, `--check` JSON, hidden `--quit`, argparse prefixes/negatives/`--`/explicit-value/unrecognized, help text | ✅ | `cli_matches_argparse`, `overlong_cli_expression_is_rejected_without_truncating_math`, `check_reports_dependencies_as_json`, `second_process_reuses_existing_window` (`7*8`, `--scientific`) |
| Parsed locally first, again in the primary | ✅ | same |

## §3 Files
| Item | | Test |
|---|---|---|
| state.json path, byte format (`json.dump` spacing, `ensure_ascii=False`, float repr, `\n`) | ✅ | `state_file_bytes_match_python_json_dump` |
| Atomic save (0700 dir, `.state-` 0600 temp, fsync, replace, temp always removed), OSError texts | ✅ | `history_memory_preferences_round_trip_and_clear`, `unwritable_location_does_not_destroy_session_history` (`[Errno 17] File exists: '…'`), `failed_atomic_replace_preserves_existing_file` |
| Loader rules (size cap, Python JSON grammar + error texts, type rules, per-entry skips, memory) | ✅ | `invalid_saved_values_are_not_executed_or_used` (`Expecting value: line 1 column 1 (char 0)`), `corrupt_timestamp_and_deep_json_do_not_crash_startup`, `loader_type_rules_follow_python`, `compat::tests::python_values_and_errors` |
| History dedup/limit, memory MC/MR/MS/M+/M- | ✅ | `history_limit_and_duplicate_reuse`, round-trip test |

## §4 Arithmetic
| Item | | Test |
|---|---|---|
| Tokenizer, implicit products, grammar, limits, every error text | ✅ | `precedence_…`, `decimal_arithmetic_and_percent`, `implicit_multiplication_…`, `invalid_domains_…`, `arbitrary_python_is_rejected`, `resource_limits_…` (< 1 s), `error_messages_match_python` |
| Decimal context (28 digits, Emax/Emin ±1000, Underflow trap), `str(Decimal)` incl. trailing zeros, `format_number` | ✅ | `results_match_python_reference` — 57 expressions, both `format_number` **and** `str()` equal to Python 3.14 output, + 11 error cases; `decimal::tests` |
| Trig via f64 libm + `.15g` + 15-digit rounding | ✅ | reference table (sin/cos/tan/asin/acos/atan, degrees and radians) |

## §5 UI
| Item | | Test |
|---|---|---|
| Header bar + app menu (Shortcuts/About/Quit, `<Control>q`), shortcut dialog transient | ✅ | `primary_menu_and_standard_actions` |
| Keypad, answers, error status (`error` class), clear, sqrt | ✅ | `standard_buttons_previous_answer_and_error_recovery` |
| Keyboard (numpad, Enter, Escape, Backspace w/ selection, Ctrl+H, Alt ignored) | ✅ | `keyboard_and_selection_editing` |
| Scientific mode, angle units persisted, height ≤ 700 | ✅ | `scientific_mode_angles_and_layout` |
| Memory row incl. invalid-input MC, memory status text | ✅ | `memory_controls_recall_and_clear_even_with_invalid_input` |
| History cards, `ans` substitution, reuse, clear, empty label | ✅ | `history_reuse_preserves_previous_answer_context_and_clears` |
| Clipboard copy/paste (async, serial), 1.8 s status clear | ✅ (timer ☑) | `clipboard_result_and_paste_expression` |
| Live palette, close cleanup (theme closed, CSS removed) | ✅ | `palette_live_binding_and_close_cleanup` |
| CSS verbatim, widget tree, √ cairo drawing | ✅ | measure_headless screenshot byte-identical to Python |
| Strings via `tr!` from shared/locale | ☑ | |

## Deviations ⚠
1. `ln/log10/exp/x**y` are computed in fixed point with ≥30 guard digits and rounded once — like libmpdec "almost always
   correctly rounded"; all 57 Python reference values match exactly, a 1-ulp difference at an exact rounding tie is possible.
2. JSON: UTF-16/32 state files (Python auto-detects) are reported as a decode error; lone surrogate escapes become U+FFFD;
   the recursion limit is fixed at 990 levels (Python's depends on the interpreter stack).
3. `Decimal()` strings with non-ASCII digits (accepted by Python) are rejected, so such a tampered history entry is skipped.
4. argparse's prog is always `calculator.py` (the Python launcher's script name); Python 3.14's colour output on a TTY is not reproduced.

## Gaps ❌
- Installer/launcher swap (`/usr/local/bin/calculator`, .desktop `Exec`) is not part of the crate.
- Python's `screenshot()` helper (docs image) is not ported; measure_headless takes the comparison screenshots instead.
