# system-monitor — parity with contracts/system-monitor.md

Crate `components/system-monitor`, binary `system-monitor` (replaces the `/usr/local/bin/system-monitor`
wrapper target), GApplication `org.waydock.SystemMonitor`. psutil is replaced by direct `/proc` + `/sys`
reads (`procfs::ProcFs::new(root)`, root `/` live or a fake tree in tests) behind the `source::Source`
trait that mirrors the psutil calls monitor_backend.py makes.

Tests: `env -u TMPDIR cargo test -p system-monitor` (28 unit tests + 1 ignored live-dump helper),
`tests/run_headless.sh` (15 GTK checks in a private nested headless labwc),
`tests/measure_headless.sh` (Rust vs Python startup/RSS/PSS + screenshots, same private compositor).

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points
| Item | | Test |
|---|---|---|
| App id, `HANDLES_COMMAND_LINE`, prgname = Wayland app_id, second launch forwards argv and reuses the window, `--quit` exits 0 | ✅ | `launcher_reuses_existing_window_and_exits_cleanly` (ui) |
| `--check` JSON line (Python `json.dumps` separators), no GTK | ✅ | manual: identical keys/values to Python on this machine (`{"ready": true, "processes": …, "logical_cores": 16, "memory_total": 64953643008, "gpus": 2}`) |
| `--page` choices, argparse help/usage/errors (exit 2), prefix abbreviations, `--opt=value` | ✅ | `args::tests::argparse_texts_match_python` (texts byte-equal to Python's with prog `system_monitor.py`) |
| `show()`: unminimize, present, idle `wlrctl toplevel focus app_id:…` | ☑ | |
| `attach_application` palette theme | ☑ | (suite-gtk tested) |
| .desktop / wrapper / installer | ❌ | installer step (see report) |

## §4 Sampling (monitor_backend.py)
| Item | | Test |
|---|---|---|
| First-sample zero rates, no cmdline/env reads, JSON-serialisable | ✅ | `first_sample_has_zero_rates_and_no_private_arguments` |
| CPU from per-core deltas, guest subtraction, iowait idle, normalised process CPU, I/O rates | ✅ | `cpu_normalization_guest_accounting_and_io_rates` |
| Baselines keyed by (pid, create_time); PID reuse / new / dead | ✅ | `pid_reuse_death_and_new_processes_have_clean_baselines` |
| Vanishing process dropped; denied fields `null` | ✅ | `vanishing_process_and_permission_denied_fields` |
| Hotplug / counter reset / aggregate reset on device-set change | ✅ | `hotplug_counter_resets_and_nonpositive_elapsed` |
| Addresses only when requested | ✅ | `network_addresses_are_only_explicitly_requested` |
| Remote/FUSE capacity not queried; partitions every ≥10 s | ✅ | `remote_mount_usage_is_not_queried` |
| No disk counters → `null` | ✅ | `no_disk_counters_is_unavailable_not_faked_zero` |
| Per-metric failure → warning, others kept | ✅ | `global_metric_failure_does_not_remove_other_metrics` |
| Exit after read not published | ✅ | `process_that_exits_after_read_is_not_published` |
| nvidia-smi argv, 0.75 s timeout, 3 s cache, N/A → null | ✅ | `nvidia_query_is_bounded_cached_and_preserves_unsupported_values`, `nvidia_timeout_becomes_warning_without_aborting_sample`, `real_timeout_only_runs_our_private_helper` |
| DRM sysfs cards (connector links skipped, empty cards skipped, names) | ✅ | `drm_supported_values_and_connector_filtering` |
| psutil semantics of the /proc reader (stat/statm/status/io, create_time = btime + starttime/CLK_TCK, comm extension from argv[0], exe fallback to argv[0] / `(deleted)` / zombie / kernel thread, status names, meminfo `used = total − available`, swap, usage_percent, diskstats ×512 with whole-disk aggregate, mounts + `/proc/filesystems` (+zfs), net/dev, IFF_RUNNING ≈ IFF_UP + operstate up/unknown, speed ≥0, mtu, cpu topology, cpufreq policies / cpuinfo MHz, getifaddrs AF_LINK) | ✅ | `proc_reader_parses_a_fake_proc_tree`, `live_sample_is_well_formed`; live JSON compared with Python psutil on this machine: process name/user/create_time/executable identical except transient kworker names and frequency/timing values |

## §5 Process actions
| Item | | Test |
|---|---|---|
| PID-reuse check before any mutation | ✅ | `pid_reuse_rejects_every_control_before_mutation` |
| Protected (0/1/self), invalid, missing identity — no Process() read | ✅ | `protected_invalid_and_missing_identity_targets` |
| SIGTERM/SIGKILL/SIGSTOP/SIGCONT/setpriority only on explicit request; identity rechecked before signalling | ✅ | `only_explicit_actions_are_sent`, `real_control_only_terminates_our_private_child` (own `sleep 30` child, PID recorded: wrong identity refused, suspend → "stopped", resume, nice 5, details, terminate) |
| Error texts (permission / already exited / could not be changed) | ✅ | `permissions_and_exit_race_are_informative` |
| Priority range −20..19 | ✅ | `priority_validation_is_before_mutation` (⚠ bool/str/float inputs are unrepresentable in Rust) |
| Force must be a bool | ⚠ | type-enforced (`force: bool`); no runtime check needed |
| Details: field order, cmdline only here, never environment, denied → null, identity recheck | ✅ | `details_explicitly_reads_command_line_but_never_environment`, `details_rechecks_identity_after_reads` |

## §6 UI
| Item | | Test |
|---|---|---|
| Sorting (CPU desc default, text casefold, None→0), search, selection kept by (pid, create_time), PID reuse clears it | ✅ | `rows_sort_search_and_preserve_identity` |
| All pages, GPU "not available", graphs keep samples, summary labels | ✅ | `all_pages_render_missing_metrics_and_local_graphs` |
| Storage filters + natural sort, `suite-sidebar` | ✅ | `storage_hides_virtual_devices_and_sorts_naturally` (+ unit `format::tests`) |
| Pause discards samples, status text, timer stopped | ✅ | `paused_sample_does_not_replace_displayed_values` |
| Unavailable values shown as `—`, sorting with nulls | ✅ | `unavailable_metrics_sort_and_display_without_inventing_values` |
| Details dialog + confirmation bound to captured targets | ✅ | `details_and_process_controls_capture_exact_selection` |
| Late confirmation after close never acts; `run_worker`/`perform` refuse after close | ✅ | `late_confirmation_after_close_never_submits_process_action` |
| Modal, destroy-with-parent dialogs; closing them acts on nothing | ✅ | `mapped_modal_dialogs_close_without_process_actions`, `parent_close_destroys_mapped_priority_dialog` |
| Cleanup stops timer, closes, refuses samples | ✅ | `cleanup_stops_sampler_and_capture_preview` |
| Multi-selection: one confirmation listing all; action enablement rules; Ctrl+K | ✅ | `multi_selection_single_confirmation_lists_every_process` |
| Delete / Ctrl+E confirm, not while typing | ✅ | `delete_and_ctrl_e_end_with_confirmation_but_not_while_typing` |
| Context menu labels, header menu + hamburger, `app.quit`, accessible graph descriptions only while mapped | ✅ | `context_menu_header_menu_and_accessible_graphs` |
| Title, sizes, CSS class, defaults | ✅ | `window_contract` |
| CSS (`STYLE` verbatim at APPLICATION+31), look | ✅ | measure_headless screenshots of the Performance page: visually identical to Python |
| Right-click row hit-testing, Open File Location (`GtkFileLauncher`), F5/Ctrl+F, Shift+F10 | ☑ | |

## Deviations ⚠
1. psutil's `nowrap=True` wrap-around compensation for disk/network counters is not reproduced (64-bit kernel counters).
2. `isup` uses sysfs `flags`/`operstate` instead of the `SIOCGIFFLAGS` ioctl (same IFF_RUNNING meaning).
3. Sampling and process actions run on GLib's blocking thread pool (`gio::spawn_blocking`) instead of a 1-thread
   executor; the sampler is behind a mutex and the UI's `worker_pending`/`busy` flags keep calls serialized.
4. argparse `prog` is the executable's basename (`system-monitor`), as argparse itself does.
5. Graph colours use `gtk::Widget::color()` (GTK ≥ 4.10) rather than the deprecated style context.

## Gaps ❌
- Installer/swap step not in the crate (see report). The optional dock-palette screenshot variant of
  `test_cleanup_stops_sampler_and_capture_preview` (`MONITOR_TEST_SCREENSHOT`) only grabs a plain screenshot.
