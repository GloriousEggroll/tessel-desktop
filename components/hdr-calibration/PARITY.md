# tessel-hdr-calibration — parity with contracts/tessel-hdr-calibration.md

Crate `components/hdr-calibration`, binary `tessel-hdr-calibration` (GApplication `org.desktop.HDRCalibration`,
NON_UNIQUE). Ports hdr_calibration.py, calibration.py and correction.py. Depends on the way-displays lib
crate by path for `color_config` (status keyfile, `parse_colors`, `atomic_write`, `rollback`, `Transaction`)
and `style::style_window`. **`hdr-pattern` stays the C helper** (hdr-pattern.c, built by `build.py`): it is a
raw Wayland client using the staging color-management-v1 protocol with XRGB2101010 shm buffers, not a trivial
port; the Rust binary runs `<exe dir>/hdr-pattern`, else `<exe dir>/build/hdr-pattern` (same lookup as Python),
with a Rust-only `HDR_CALIBRATION_PATTERN` override for development runs.

Tests: `env -u TMPDIR cargo test -p tessel-hdr-calibration` (19 unit tests incl. compat::etree), `tests/run_headless.sh`
(6 GTK checks in a private nested headless labwc, fake backend, logging fakes on PATH),
`tests/measure_headless.sh` (Rust vs Python startup/RSS/PSS in the same private compositor; both only read the
private compositor's own status file, never write or signal; verified rc.xml unchanged, no XDG files, no tool calls).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Item | | Test |
|---|---|---|
| argparse `--output`, `--sdr-editing`, `--smoke-test`, help/usage/errors (exit 2), abbreviations | ✅ | `argparse_texts_match_python` (help text byte-equal with prog `hdr_calibration.py`) |
| NON_UNIQUE app, argv not passed to GApplication, `--smoke-test` closes + quits after 300 ms | ✅ | `smoke_test` (ui, real binary) |
| `--output` preselects the connector; `--sdr-editing` → preset 1 + focus | ✅ / ☑ | `worker` (requested connector) |
| Launcher, .desktop, installer | ❌ | installer step (see report) |

## §2–§4 Storage, rc.xml and apply transaction
| Item | | Test |
|---|---|---|
| `config_path` from labwc argv (`-c`, `--config=`, `-cFILE`, `-C`, `--config-dir=`, `-CDIR`, cwd for relative, refuses `-m`/non-labwc) | ✅ | `active_config_file_takes_precedence` |
| `validate` (version, identity, connector, mode, method/instrument, ranges, presets, correction ref, single line; floats, key order, preset appended) | ✅ | `limits_and_provenance`, `separate_presets_same_picture_mode` |
| `check_monitor` | ✅ | `monitor_identity_sdr_and_8bit_rejected` |
| `write_config`: merge per connector, keep other XML/comments/colour block/trailing comments, reset `enabled="false"`, self-closing root, lock, backup, mode | ✅ | `independent_monitors_preserve_sdr_vrr_and_comments`, `managed_block_bytes_match_python` (bytes equal to CPython ElementTree output, incl. escaping and `.9g`), `malformed_config_does_not_write` |
| rollback refuses a later edit | ✅ | `rollback_and_concurrent_changes` |
| profiles.json (one per displayId/connector/mode/preset, `saved`, indent 2, ensure_ascii, 0600, lock) | ✅ | `saved_modes_are_separate` |
| `apply`: SIGHUP, 8 s commit poll, pid/identity/correction-error aborts, save on success, rollback + second SIGHUP on failure (ESRCH ignored) | ✅ | `failed_apply_restores_config`, `success_requires_committed_values`, `apply_times_out_and_restores` |
| `matches` (targets rel 1e-6/abs 1e-5, correction committed, pending/error, reset = inactive + 203) | ✅ | `correction_commit_required_and_reset` |
| `.cube` import: O_NONBLOCK regular file, size, NUL, UTF-8, marker/identity/mode, LUT size, domain, samples; 0400 copy named by sha256 | ✅ | `import_and_strict_rejection` (9 mutations + FIFO), `mutated_import_cannot_apply` |
| `workspace_report` ("not performed" / unverified, mismatch recorded) | ✅ | `report_records_pending_measurements_and_mismatch` |
| `session()` requires `hdr_calibration_supported` | ✅ | `session_requires_updated_compositor` |

## §5 UI
| Item | | Test |
|---|---|---|
| Monitor selection, capability text (`{white:g}`), saved modes list + load, presets ↔ white, custom on manual white, correction import sensitivity, method → instrument + pattern buttons, apply worker message + report path, SDR gating | ✅ | `worker` (ui) |
| Reset waits for confirmation (AlertDialog) | ✅ | `worker` |
| Active saved correction kept; unknown active correction blocks `profile()` until cleared | ✅ | `worker` |
| Old compositor → empty page, controls insensitive | ✅ | `worker` |
| Title, size, `way-displays` + `tessel-hdr-calibration` classes, `.primary`, app menu, defaults from status, close refused while applying | ✅ | `window_contract` |
| Visual parity | ✅ | Main page screenshot from `run_headless.sh HDR_TEST_SCREENSHOT=…` is **byte-identical** to Python's `preview.png` (same scenario); unavailable page identical in `measure_headless.sh` |
| Pattern child (spawn, 100 ms poll, JSON encoding check, nits recorded, cancel text, SIGTERM on close/unavailable) | ☑ | not exercised (needs the colour-managed wlroots test server; the C helper keeps `test_patterns.py`) |
| FileChooserNative `.cube` import | ☑ | import logic tested in unit tests |

## Deviations ⚠
1. The status keyfile is a `BTreeMap` (way-displays crate), so `compositor`/`active` objects in reports are key-sorted
   rather than in file order.
2. Error texts from Rust parsers (invalid JSON in profiles.json, XML errors inside the managed block) differ from
   Python's `json`/ElementTree messages; ValueError texts written in calibration.py/correction.py are identical.
3. `XDG_*_HOME` set to an empty string is honoured as a relative path, exactly like Python's `os.environ.get(...)`.
4. The apply worker runs on GLib's blocking pool; closing is refused while it runs (as in Python), so it is never cut off.

## Gaps ❌
- hdr-pattern is not ported (C helper kept; installer must keep building/installing it next to the binary).
- Installer/swap step not part of the crate.
