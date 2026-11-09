# tessel-system-monitor — external contract

Source: `~/tessel-bluetooth/tessel-system-monitor` (`system_monitor.py` 85 LOC launcher, `monitor_backend.py` 515,
`monitor_ui.py` 919; tests `test_monitor_backend.py`, `test_monitor_ui.py`). Standalone GTK4 task
manager. No D-Bus service of its own, no config/state files, no systemd unit.

## 1. Entry points

| Item | Value | Ref |
|---|---|---|
| Installed by | `desktop-apps/install.py` (`APPS['tessel-system-monitor'] = ('system_monitor.py', 'org.tessel.SystemMonitor.desktop')`): all non-test `*.py` → `/usr/local/lib/tessel/tessel-system-monitor/` (0644) + shared `gtk4_utils.py suite_i18n.py language_data.py suite_catalogs.py background_blur.py libtessel-dock-background-blur.so app_menu.py companion_theme.py` | desktop-apps/install.py:15-55 |
| Launcher | `/usr/local/bin/tessel-system-monitor` (0755) = `#!/bin/sh\n# Tessel standalone application\nexec /usr/bin/python3 "/usr/local/lib/tessel/tessel-system-monitor/system_monitor.py" "$@"\n` (marker line `# Tessel standalone application` identifies suite-owned wrappers) | install.py:47-50 |
| .desktop | `/usr/share/applications/org.tessel.SystemMonitor.desktop`: `Name=System Monitor`, `Comment=View running processes and system performance`, `Exec=/usr/local/bin/tessel-system-monitor` (installer rewrites `Exec=tessel-system-monitor`), `Icon=utilities-system-monitor`, `StartupNotify=true`, **`StartupWMClass=org.tessel.SystemMonitor`**, `Categories=System;Monitor;`, `Keywords=Task Manager;Processes;CPU;Memory;RAM;Performance;GPU;` | org.tessel.SystemMonitor.desktop |
| Launched by | tessel-dock `panel_actions.open_system_monitor` → runs `/usr/local/bin/tessel-system-monitor` (fails with "System Monitor is not installed." if not a file); tessel-shell menu action `open_system_monitor` → `org.tessel.SystemMonitor.desktop` | tessel-dock/panel_actions.py:96-100, tessel-shell/tessel-shell:943,1005 |
| GApplication id | `org.tessel.SystemMonitor`, flags `HANDLES_COMMAND_LINE` (unique; secondary instances forward argv to the primary and exit 0); `GLib.set_prgname(APP_ID)` → Wayland app_id `org.tessel.SystemMonitor` | system_monitor.py:13,36-41 |
| CLI | `--check` (local, no GTK, no window: prints `json.dumps({"ready": true, "processes": N, "logical_cores": N, "memory_total": N, "gpus": N})` = Python separators `", "`/`": "`, exit 0); `--page {processes,performance,storage,network,gpu,system}`; `--quit` (hidden: closes the window and quits the primary). argparse errors: usage on stderr, exit 2 (also forwarded to the primary, which returns the code). `-h/--help`. | system_monitor.py:17-28,49-60 |
| Activation | `show(page)`: create window if none or closed; `show_page(page)` if given; `unminimize()`; `present()`; idle → `wlrctl toplevel focus app_id:org.tessel.SystemMonitor` (only if wlrctl on PATH, output silenced). A second launch reuses the existing window (never a second toplevel). | system_monitor.py:62-79 |
| Theme | `companion_theme.attach_application(app)` (all windows follow the dock palette) | |

## 2. IPC
None exported. Wayland only via GTK (+ `wlrctl` for raising). `Gtk.FileLauncher.open_containing_folder`
(→ `org.freedesktop.FileManager1` / portal through GTK) for "Open File Location".

## 3. Files
Read-only: `/proc/stat`, `/proc/meminfo`, `/proc/<pid>/{stat,statm,status,io,exe,cmdline,cwd}`,
`/proc/diskstats`, `/proc/self/mounts` (`/etc/mtab`), `/proc/filesystems`, `/proc/net/dev`,
`/sys/class/net/<if>/{flags,operstate,speed,mtu}`, `/sys/devices/system/cpu/…` (frequency, topology),
`/sys/class/drm/card[0-9]*/device/{vendor,gpu_busy_percent,mem_info_vram_total,mem_info_vram_used,product_name,hwmon/hwmon*/temp1_input}`,
`/etc/os-release` (`PRETTY_NAME`, else `platform.system()` = `Linux`). Writes nothing.
`~/.cache/tessel-dock/palette.json` via companion_theme. Never reads process environments, open files
or connections; command line only for an explicit Properties request.

## 4. Sample format (`Sampler.sample()`, also the basis of `--check`)
`{timestamp, interval, cpu{percent, per_core[], logical_cores, physical_cores, frequency{current,min,max}},
memory{total,used,available,free,percent,cached,buffers}, swap{total,used,free,percent},
system{os,kernel,hostname,architecture,uptime}, processes[{pid,name,user,status,cpu_percent,memory_percent,rss,threads,
read_bytes_per_sec,write_bytes_per_sec,create_time,executable}], disks{read_bytes_per_sec,write_bytes_per_sec,read_bytes,write_bytes,
devices[{name,read_bytes,read_bytes_per_sec,write_bytes,write_bytes_per_sec}], partitions[{device,mountpoint,fstype,total,used,free,percent,error}]},
network{recv_bytes_per_sec,sent_bytes_per_sec,recv_bytes,sent_bytes, interfaces[{name,up,speed_mbps,mtu,recv_bytes,recv_bytes_per_sec,sent_bytes,sent_bytes_per_sec[,addresses]}]},
gpus[{id,name,source,utilization_percent,memory_total,memory_used,temp_c}], warnings[]}`.
Rules (monitor_backend.py): all rates 0 on first observation, for new processes/devices, on counter
decrease or nonpositive interval; unavailable = `null`; CPU from per-core `/proc/stat` deltas
(guest/guest_nice subtracted, iowait = idle), percent rounded to 2 decimals and clamped 0–100; process
CPU normalised by logical cores; process baseline keyed by `(pid, create_time)` (PID reuse resets);
process rows dropped when not running after the read; disk aggregate baseline reset when the device
set changes; network aggregate = sum of per-interface values; partitions re-read every ≥10 s, remote
fs (`nfs nfs4 cifs smbfs sshfs fuse.sshfs 9p ceph glusterfs fuse.glusterfs afs davfs fuse.davfs`) →
`"Network filesystem capacity is not sampled."`, `fuse*` → `"Userspace filesystem capacity is not sampled."`;
GPUs re-read every ≥3 s: `nvidia-smi --query-gpu=uuid,name,utilization.gpu,memory.total,memory.used,temperature.gpu --format=csv,noheader,nounits`
(timeout 0.75 s; failures → warning `NVIDIA GPU information is unavailable: …`), then DRM cards (skip
NVIDIA `0x10de` when nvidia-smi gave rows; skip cards with no metric; name `product_name` or
`"{AMD|Intel|NVIDIA} GPU (cardN)"`). Addresses only with `include_addresses` (UI never).
Warnings (translated): `CPU usage is unavailable: {error}`, `Memory usage is unavailable: {error}`,
`Swap usage is unavailable: {error}`, `Some processes could not be inspected: {error}`,
`Disk activity is unavailable: {error}`, `Disk capacity is unavailable: {error}`,
`Network activity is unavailable: {error}`, `System uptime is unavailable: {error}`.

## 5. Process actions (explicit only)
`end_process(pid, create_time, force)` (SIGTERM, or SIGKILL when force), `suspend_process` (SIGSTOP),
`resume_process` (SIGCONT), `set_process_priority(nice -20..19)` (setpriority), `process_details`
(name, user, status, executable, command_line, working_directory, parent_pid, nice, threads; denied
fields `null`; never environment). Before any mutation: valid pid ≥1, not 1 / own pid
("This process is protected and cannot be changed here."), finite positive create_time ("The process
identity is unavailable. Refresh the process list."), create_time must equal ("The selected process
has exited and its process ID was reused. Refresh the list."). Errors: "The selected process has already
exited.", "You do not have permission to change this process.", "The process could not be changed:
{error}", "You do not have permission to inspect this process.", "The process could not be inspected:
{error}", "Choose a valid process.", "Process priority must be an integer between -20 and 19.",
"Force quit must be explicitly enabled or disabled.". No elevation.

## 6. UI (monitor_ui.py)
- Window title `System Monitor`, default 1120×760, min 820×540, class `tessel-system-monitor`, CSS `STYLE` at
  APPLICATION+31 (removed on cleanup), suite headerbar, `attach(window)`.
- Navigation ListBox (`.monitor-nav`, `suite-sidebar`, 170 px) rows Processes/Performance/Storage/Network/
  Graphics/System with themed icon fallbacks (`PAGE_ICONS`); Stack pages `processes performance storage network gpu system` (`.monitor-page`, `.monitor-heading`).
- Processes: summary cards CPU/Memory/Disk I/O/Network (`.monitor-summary .card`, `.monitor-caption`, `.monitor-value`);
  toolbar (`.monitor-toolbar`): SearchEntry "Search name, PID, user or status" (a11y "Search processes"),
  "All users" check (default on; off = only `getpass.getuser()`), "Details", "End Task" (`destructive-action`);
  ColumnView (MultiSelection, row separators) columns Name, PID, CPU, Memory, Disk read, Disk write, User,
  Status (`.monitor-metric` cells, right-aligned numbers, ellipsized, tooltip raw value or "Unavailable"),
  custom sorters (text casefold; numbers `None`→0), initial sort CPU descending; footer caption
  "CPU is shown as a share of total processor capacity.".
  Search matches casefolded `name pid user status`. Selection kept across refreshes by `(pid, create_time)`.
- Actions (`win.`): end, kill, suspend, resume, priority, open-location, details (labels End Task, Force Quit,
  Suspend, Resume, Set Priority…, Open File Location, Properties) in section "Selected processes" of the
  header primary menu + `win.refresh` "Refresh Now"; enabled rules: any selected and none of pid 0/1/self and
  not busy (priority: also single; details: single; open-location: single with executable).
  `install_app_menu(name='System Monitor', icon='utilities-system-monitor', comments='Processes, performance and system information', shortcuts=SHORTCUTS)`.
- Context menu: right click (selects the row if needed), Menu, Shift+F10 → PopoverMenu from the actions section.
- Keys: Ctrl+F search (switches to Processes), F5 refresh; on the Processes page and not in an editable:
  Delete/KP_Delete/Ctrl+E end (confirm), Ctrl+K force quit (confirm), Menu/Shift+F10 menu.
- Confirmation `Gtk.AlertDialog(message, detail, buttons=[Cancel, <action>], cancel=0, default=0)`: single
  `"<Action> — name (pid)"`; multiple `"End {count} processes?"` / `Force quit…` / `Suspend…` / `Resume…`
  with detail + up to 12 `name (pid)` lines + `…and {count} more`. Targets are captured before the prompt;
  a late answer after close never acts. Failures `name (pid): error` joined → error alert "Unable to complete the request".
  Success → status "Process action completed" + refresh.
- Priority dialog "Process Priority" (modal, destroy-with-parent, headerbar), "name (pid)", hint text, SpinButton −20..19, Cancel/Apply.
- Details dialog "Process Details" 580×500: one info card, keys `key.replace('_',' ').capitalize()`, command line `shlex.join`.
- Performance: 4 cards (`.monitor-card .card`, `.monitor-card-title`) with Graph (DrawingArea, 130 px, 60 samples,
  class `profile-check`, role IMG, a11y name `CPU usage history` …, description
  "Current {current}, average {average}, peak {peak} over the last {count} samples" updated only while mapped),
  "Last 60 samples"; cpu info `"{physical} cores · {logical} logical processors"` + ` · x.xx GHz` + `\n{used} / {total} memory in use`;
  per-core FlowBox `CPU n  x.x%`.
- Storage (`.monitor-storage`, `.monitor-storage-heading`, `.monitor-storage-row`): Disks (hide `loop ram zram`, natural sort) with Read/Write rates;
  Volumes (hide squashfs, loop/ram/zram devices, `/snap/…`; natural sort by mountpoint) with `{used} of {total}` + ProgressBar or error.
- Network cards: Status (Unavailable/Connected/Disconnected), Receive, Send, Link speed `N Mbps`. Graphics cards
  (Utilization, Video memory used/total, Temperature `N °C`, progress) or "Graphics telemetry is not available from the installed driver.".
  System card: Operating system, Kernel, Uptime `Dd Hh Mm`, Memory, Available memory, Swap used, Swap total.
- Footer `.monitor-status`: status label ("Reading system information…", `{count} processes · Updated HH:MM:SS`, tooltip = warnings),
  "Refresh" + DropDown `0.5 s 1 s 2 s 5 s` (default 1 s, a11y "Refresh interval"), Pause toggle (label Pause/Resume; status
  "Paused — displayed values are frozen"; paused samples are discarded). One sampling worker at a time; "Could not refresh: " + error.
- Close: cleanup (closed, destroy transient children, stop timer, drop worker results, close theme, remove CSS).
- Formatting: `size` B/KiB/MiB/GiB/TiB (`%.0f B`, else `%.1f`), `rate` = size + `/s`, `percent` `%.1f%`, `—` for unavailable.

## 7. Shortcuts window
Processes: Delete / Ctrl+E "End the selected processes", Ctrl+K "Force quit the selected processes", Menu / Shift+F10
"Open the process menu", Return "Show process details", Ctrl+A "Select all processes", Ctrl+F "Search processes";
View: F5 "Refresh now"; plus app_menu's General (Ctrl+?, Ctrl+Q).

## 8. Parity tests
`test_monitor_backend.py` (fake psutil: first sample zero rates/no cmdline; CPU normalisation + guest; PID reuse;
vanishing/denied fields; hotplug resets; addresses opt-in; remote mount; no disk counters; partial failure warnings;
exit-after-read; nvidia caching/timeout/private helper; DRM; PID-reuse/protected/explicit/permission/priority/force/details
checks; real terminate of own child). `test_monitor_ui.py` (private compositor: sorting/search/identity, pages, storage
filters, pause, unavailable metrics, launcher reuse + `--quit`, details/confirm capture exact selection, late confirmation,
modal dialogs, parent close destroys dialog, cleanup, multi-selection confirmation, Delete/Ctrl+E not while typing,
context/header menu + accessible graphs).
