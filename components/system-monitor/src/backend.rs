//! monitor_backend.py: read-only system sampling and explicitly requested
//! process actions.
//!
//! Run [`Sampler::sample`] in a worker thread: it never sleeps, although the
//! optional NVIDIA query can take up to [`GPU_TIMEOUT`]. All rates are zero on
//! the first observation, including newly created processes and newly added
//! devices. Unavailable metrics are `null` rather than invented measurements.
//! Command lines and environments are never read by the sampler.

use crate::source::{PsError, PsResult, Source};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tessel_core::tr;

pub const GPU_TIMEOUT: Duration = Duration::from_millis(750);
pub const GPU_INTERVAL: f64 = 3.0;
pub const REMOTE_FILESYSTEMS: &[&str] = &["nfs", "nfs4", "cifs", "smbfs", "sshfs", "fuse.sshfs", "9p", "ceph",
    "glusterfs", "fuse.glusterfs", "afs", "davfs", "fuse.davfs"];

/// Python `_number`: `float(value)` when finite.
pub fn number_text(text: &str) -> Option<f64> {
    let cleaned: String = text.trim().chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Python `round(value, digits)` (correctly rounded, half-even on the exact value).
pub fn py_round(value: f64, digits: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{value:.digits$}").parse().unwrap_or(value)
}

/// `_percent`: clamp to 0–100, two decimals.
pub fn percent(value: f64) -> f64 {
    py_round(value.clamp(0.0, 100.0), 2)
}

/// `_rate`: counter resets, absent baselines and nonpositive intervals have no rate.
pub fn rate(current: Option<f64>, previous: Option<f64>, interval: f64) -> f64 {
    match (current, previous) {
        (Some(current), Some(previous)) if interval > 0.0 && current >= previous => ((current - previous) / interval).max(0.0),
        _ => 0.0,
    }
}

fn cpu_totals(value: &crate::source::CpuTimes) -> (f64, f64) {
    // Linux guest times are already included in user/nice. I/O wait is idle.
    let total = value.user + value.nice + value.system + value.idle + value.iowait + value.irq + value.softirq
        + value.steal + value.guest + value.guest_nice - (value.guest + value.guest_nice);
    let idle = value.idle + value.iowait;
    (total, total - idle)
}

/// `subprocess.run` result for the NVIDIA query.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `(argv, timeout) -> output | error text` (OSError / TimeoutExpired / SubprocessError).
pub type GpuRunner = Box<dyn Fn(&[String], Duration) -> Result<RunOutput, String> + Send>;
pub type Clock = Box<dyn Fn() -> f64 + Send>;

fn python_list_repr(argv: &[String]) -> String {
    let items: Vec<String> = argv.iter().map(|a| format!("'{}'", a.replace('\\', "\\\\").replace('\'', "\\'"))).collect();
    format!("[{}]", items.join(", "))
}

/// The real runner: no stdin, captured output, killed after `timeout`.
pub fn run_command(argv: &[String], timeout: Duration) -> Result<RunOutput, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped())
        .stderr(Stdio::piped()).spawn()
        .map_err(|e| tessel_core::pyerr::os_error_path(&e, Path::new(&argv[0])))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || { let mut s = Vec::new(); let _ = stdout.read_to_end(&mut s); s });
    let err = std::thread::spawn(move || { let mut s = Vec::new(); let _ = stderr.read_to_end(&mut s); s });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Command '{}' timed out after {} seconds", python_list_repr(argv),
                                   tessel_core::pyfmt::float_repr(timeout.as_secs_f64())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(tessel_core::pyerr::os_error_text(&e)),
        }
    };
    let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    use std::os::unix::process::ExitStatusExt;
    let returncode = status.code().unwrap_or_else(|| -status.signal().unwrap_or(1));
    Ok(RunOutput { returncode, stdout, stderr })
}

/// Sampler options (Python keyword arguments).
pub struct Options {
    pub source: Box<dyn Source>,
    pub clock: Clock,
    pub wall_clock: Clock,
    pub gpu_runner: GpuRunner,
    /// `None` disables the NVIDIA query (Python `gpu_command=None`).
    pub gpu_command: Option<PathBuf>,
    pub drm_root: Option<PathBuf>,
    pub include_addresses: bool,
    /// `platform` facts; injectable for tests.
    pub system_info: Option<Map<String, Value>>,
}

impl Default for Options {
    fn default() -> Self {
        let start = Instant::now();
        Self {
            source: Box::new(crate::procfs::ProcFs::new("/")),
            clock: Box::new(move || start.elapsed().as_secs_f64()),
            wall_clock: Box::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64()).unwrap_or(0.0)),
            gpu_runner: Box::new(run_command),
            gpu_command: tessel_core::shell::which("nvidia-smi"),
            drm_root: Some(PathBuf::from("/sys/class/drm")),
            include_addresses: false,
            system_info: None,
        }
    }
}

#[derive(Default, Clone)]
struct ProcessBaseline {
    time: f64,
    cpu_time: Option<f64>,
    read_bytes: Option<f64>,
    write_bytes: Option<f64>,
}

/// Stateful sampler; one instance supplies an independent rate baseline.
pub struct Sampler {
    ps: Box<dyn Source>,
    clock: Clock,
    wall_clock: Clock,
    gpu_runner: GpuRunner,
    gpu_command: Option<PathBuf>,
    drm_root: Option<PathBuf>,
    include_addresses: bool,
    previous_time: Option<f64>,
    cpu_previous: Vec<(f64, f64)>,
    /// Keyed by `(pid, create_time bits)`.
    process_previous: HashMap<(i64, u64), ProcessBaseline>,
    disk_previous: BTreeMap<String, (Option<f64>, Option<f64>)>,
    network_previous: BTreeMap<String, (Option<f64>, Option<f64>)>,
    gpu_time: Option<f64>,
    gpus: Vec<Value>,
    gpu_warnings: Vec<String>,
    partition_time: Option<f64>,
    partitions: Vec<Value>,
    partition_warnings: Vec<String>,
    system_info: Map<String, Value>,
}

fn os_name(root: &Path) -> String {
    for path in [root.join("etc/os-release"), root.join("usr/lib/os-release")] {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                    let shell = tessel_core::shell::shell_split(value);
                    return shell.into_iter().next().unwrap_or_default();
                }
            }
            return "Linux".into();
        }
    }
    "Linux".into()
}

fn uname() -> (String, String, String) {
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return (String::new(), String::new(), String::new());
    }
    let text = |field: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(field.as_ptr()) }.to_string_lossy().into_owned();
    (text(&name.release), text(&name.nodename), text(&name.machine))
}

/// `{"os", "kernel", "hostname", "architecture"}` (platform module equivalents).
pub fn system_info() -> Map<String, Value> {
    let (release, node, machine) = uname();
    let mut map = Map::new();
    map.insert("os".into(), json!(os_name(Path::new("/"))));
    map.insert("kernel".into(), json!(release));
    map.insert("hostname".into(), json!(node));
    map.insert("architecture".into(), json!(machine));
    map
}

fn opt_u(value: Option<u64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

fn opt_f(value: Option<f64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

impl Sampler {
    pub fn new(options: Options) -> Self {
        Self {
            ps: options.source, clock: options.clock, wall_clock: options.wall_clock, gpu_runner: options.gpu_runner,
            gpu_command: options.gpu_command, drm_root: options.drm_root, include_addresses: options.include_addresses,
            previous_time: None, cpu_previous: Vec::new(), process_previous: HashMap::new(),
            disk_previous: BTreeMap::new(), network_previous: BTreeMap::new(), gpu_time: None, gpus: Vec::new(),
            gpu_warnings: Vec::new(), partition_time: None, partitions: Vec::new(), partition_warnings: Vec::new(),
            system_info: options.system_info.unwrap_or_else(system_info),
        }
    }

    /// Number of process rate baselines kept (Python `_process_previous`).
    pub fn tracked_processes(&self) -> usize {
        self.process_previous.len()
    }

    pub fn sample(&mut self) -> Value {
        let now = (self.clock)();
        let timestamp = (self.wall_clock)();
        let interval = self.previous_time.map(|p| (now - p).max(0.0)).unwrap_or(0.0);
        let mut warnings = Vec::new();
        let logical = self.ps.cpu_count(true).filter(|n| *n > 0).unwrap_or(1);
        let cpu = self.sample_cpu(logical, &mut warnings);
        let memory = self.sample_memory(&mut warnings);
        let total = memory.get("total").and_then(Value::as_f64);
        let processes = self.sample_processes(now, logical as f64, total, &mut warnings);
        let disks = self.sample_disks(now, interval, &mut warnings);
        let network = self.sample_network(interval, &mut warnings);
        let gpus = self.sample_gpus(now, &mut warnings);
        let mut system = self.system_info.clone();
        match self.ps.boot_time() {
            Ok(boot) => { system.insert("uptime".into(), json!((timestamp - boot).max(0.0))); }
            Err(error) => {
                system.insert("uptime".into(), Value::Null);
                warnings.push(tr!("System uptime is unavailable: {error}", error = error.to_string()));
            }
        }
        self.previous_time = Some(now);
        let swap = self.sample_swap(&mut warnings);
        json!({
            "timestamp": timestamp, "interval": interval, "cpu": cpu, "memory": memory, "swap": swap,
            "system": system, "processes": processes, "disks": disks, "network": network, "gpus": gpus,
            "warnings": warnings,
        })
    }

    fn sample_cpu(&mut self, logical: usize, warnings: &mut Vec<String>) -> Value {
        let mut result = json!({
            "percent": null, "per_core": [], "logical_cores": logical,
            "physical_cores": self.ps.cpu_count(false).filter(|n| *n > 0),
            "frequency": {"current": null, "min": null, "max": null},
        });
        match self.ps.cpu_times_percpu() {
            Ok(times) => {
                let values: Vec<(f64, f64)> = times.iter().map(cpu_totals).collect();
                let (mut total_delta, mut busy_delta) = (0.0, 0.0);
                let mut per_core = Vec::new();
                for (index, (total, busy)) in values.iter().enumerate() {
                    let mut value = 0.0;
                    if let Some((old_total, old_busy)) = self.cpu_previous.get(index) {
                        let (delta, active) = (total - old_total, busy - old_busy);
                        if delta > 0.0 && active >= 0.0 {
                            value = percent(active / delta * 100.0);
                            total_delta += delta;
                            busy_delta += delta.min(active);
                        }
                    }
                    per_core.push(json!(value));
                }
                result["per_core"] = Value::Array(per_core);
                result["percent"] = json!(if total_delta != 0.0 { percent(busy_delta / total_delta * 100.0) } else { 0.0 });
                self.cpu_previous = values;
            }
            Err(error) => {
                self.cpu_previous.clear();
                warnings.push(tr!("CPU usage is unavailable: {error}", error = error.to_string()));
            }
        }
        if let Ok(Some(frequency)) = self.ps.cpu_freq() {
            let finite = |v: Option<f64>| opt_f(v.filter(|v| v.is_finite()));
            result["frequency"] = json!({"current": finite(frequency.current), "min": finite(frequency.min), "max": finite(frequency.max)});
        }
        result
    }

    fn sample_memory(&mut self, warnings: &mut Vec<String>) -> Map<String, Value> {
        match self.ps.virtual_memory() {
            Ok(v) => json!({"total": v.total, "used": v.used, "available": v.available, "free": v.free,
                            "percent": v.percent, "cached": v.cached, "buffers": v.buffers}),
            Err(error) => {
                warnings.push(tr!("Memory usage is unavailable: {error}", error = error.to_string()));
                json!({"total": null, "used": null, "available": null, "free": null, "percent": null})
            }
        }.as_object().cloned().unwrap()
    }

    fn sample_swap(&mut self, warnings: &mut Vec<String>) -> Value {
        match self.ps.swap_memory() {
            Ok(v) => json!({"total": v.total, "used": v.used, "free": v.free, "percent": v.percent}),
            Err(error) => {
                warnings.push(tr!("Swap usage is unavailable: {error}", error = error.to_string()));
                json!({"total": null, "used": null, "free": null, "percent": null})
            }
        }
    }

    fn sample_processes(&mut self, now: f64, logical: f64, memory_total: Option<f64>, warnings: &mut Vec<String>) -> Vec<Value> {
        let mut rows = Vec::new();
        let mut previous = HashMap::new();
        match self.ps.pids() {
            Ok(pids) => {
                for pid in pids {
                    if let Some((row, key, baseline)) = self.sample_process(pid, now, logical, memory_total) {
                        rows.push(row);
                        previous.insert(key, baseline);
                    }
                }
            }
            Err(error) => warnings.push(tr!("Some processes could not be inspected: {error}", error = error.to_string())),
        }
        self.process_previous = previous;
        rows
    }

    fn sample_process(&self, pid: i64, now: f64, logical: f64, memory_total: Option<f64>)
                      -> Option<(Value, (i64, u64), ProcessBaseline)> {
        // Fresh objects avoid stale caches after PID reuse.
        let process = self.ps.process(pid).ok()?;
        let created = process.create_time().ok()?;
        let key = (process.pid(), created.to_bits());
        let old = self.process_previous.get(&key).cloned();
        // `read()`: AccessDenied / Zombie / OSError → fallback; NoSuchProcess drops the row.
        fn read<T>(value: PsResult<T>) -> Result<Option<T>, ()> {
            match value {
                Ok(v) => Ok(Some(v)),
                Err(PsError::NoSuchProcess(_)) => Err(()),
                Err(_) => Ok(None),
            }
        }
        let name = read(process.name()).ok()?.unwrap_or_else(|| process.pid().to_string());
        let user = read(process.username()).ok()?;
        let status = read(process.status()).ok()?;
        let times = read(process.cpu_times()).ok()?;
        let cpu_time = times.map(|t| t.user + t.system);
        let elapsed = now - old.as_ref().map(|o| o.time).unwrap_or(now);
        let cpu_percent = cpu_time.map(|c| percent(rate(Some(c), old.as_ref().and_then(|o| o.cpu_time), elapsed) / logical * 100.0));
        let rss = read(process.rss()).ok()?;
        let disk = read(process.io_counters()).ok()?;
        let read_bytes = disk.and_then(|d| d.read_bytes).map(|v| v as f64);
        let write_bytes = disk.and_then(|d| d.write_bytes).map(|v| v as f64);
        let threads = read(process.num_threads()).ok()?;
        let executable = read(process.exe()).ok()?;
        let memory_percent = match (rss, memory_total) {
            (Some(rss), Some(total)) if total != 0.0 => Some(percent(rss as f64 / total * 100.0)),
            _ => None,
        };
        let row = json!({
            "pid": process.pid(), "name": name, "user": user, "status": status, "cpu_percent": cpu_percent,
            "memory_percent": memory_percent, "rss": rss, "threads": threads,
            "read_bytes_per_sec": read_bytes.map(|r| rate(Some(r), old.as_ref().and_then(|o| o.read_bytes), elapsed)),
            "write_bytes_per_sec": write_bytes.map(|w| rate(Some(w), old.as_ref().and_then(|o| o.write_bytes), elapsed)),
            "create_time": created, "executable": executable,
        });
        if !process.is_running() {
            return None;
        }
        Some((row, key, ProcessBaseline { time: now, cpu_time, read_bytes, write_bytes }))
    }

    fn sample_disks(&mut self, now: f64, interval: f64, warnings: &mut Vec<String>) -> Value {
        let mut result = json!({"read_bytes_per_sec": null, "write_bytes_per_sec": null,
                                "read_bytes": null, "write_bytes": null, "devices": [], "partitions": []});
        let mut previous = BTreeMap::new();
        match self.ps.disk_io_counters() {
            Ok((devices, aggregate)) => {
                let names: BTreeSet<&String> = devices.iter().map(|(n, _)| n).collect();
                let old_names: BTreeSet<&String> = self.disk_previous.keys().filter(|k| *k != "__aggregate__").collect();
                let mut sorted = devices.clone();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                let mut counters: Vec<(String, Option<crate::source::DiskCounters>)> = vec![("__aggregate__".into(), aggregate)];
                counters.extend(sorted.into_iter().map(|(n, c)| (n, Some(c))));
                let devices_changed = names != old_names;
                for (name, value) in counters {
                    let Some(value) = value else { continue };
                    let mut old = self.disk_previous.get(&name).cloned().unwrap_or((None, None));
                    if name == "__aggregate__" && devices_changed {
                        // Device addition/removal changes accumulated aggregate totals.
                        old = (None, None);
                    }
                    let (read, write) = (value.read_bytes.map(|v| v as f64), value.write_bytes.map(|v| v as f64));
                    let row = json!({
                        "name": name, "read_bytes": opt_u(value.read_bytes),
                        "read_bytes_per_sec": read.map(|r| rate(Some(r), old.0, interval)),
                        "write_bytes": opt_u(value.write_bytes),
                        "write_bytes_per_sec": write.map(|w| rate(Some(w), old.1, interval)),
                    });
                    previous.insert(name.clone(), (read, write));
                    if name == "__aggregate__" {
                        for key in ["read_bytes", "read_bytes_per_sec", "write_bytes", "write_bytes_per_sec"] {
                            result[key] = row[key].clone();
                        }
                    } else {
                        result["devices"].as_array_mut().unwrap().push(row);
                    }
                }
            }
            Err(error) => warnings.push(tr!("Disk activity is unavailable: {error}", error = error.to_string())),
        }
        self.disk_previous = previous;
        // Capacity changes slowly and statvfs can be expensive on many mounts.
        if self.partition_time.is_none_or(|t| now - t >= 10.0) {
            let (rows, warnings) = self.sample_partitions();
            self.partitions = rows;
            self.partition_warnings = warnings;
            self.partition_time = Some(now);
        }
        result["partitions"] = Value::Array(self.partitions.clone());
        warnings.extend(self.partition_warnings.iter().cloned());
        result
    }

    fn sample_partitions(&self) -> (Vec<Value>, Vec<String>) {
        let (mut rows, mut warnings, mut seen) = (Vec::new(), Vec::new(), BTreeSet::new());
        match self.ps.disk_partitions() {
            Ok(mounts) => {
                for mount in mounts {
                    if !seen.insert(mount.mountpoint.clone()) {
                        continue;
                    }
                    let mut row = json!({"device": mount.device, "mountpoint": mount.mountpoint, "fstype": mount.fstype,
                                         "total": null, "used": null, "free": null, "percent": null, "error": null});
                    // A stale network mount must not stall the monitoring worker.
                    if REMOTE_FILESYSTEMS.contains(&mount.fstype.as_str()) {
                        row["error"] = json!("Network filesystem capacity is not sampled.");
                    } else if mount.fstype.starts_with("fuse") {
                        row["error"] = json!("Userspace filesystem capacity is not sampled.");
                    } else {
                        match self.ps.disk_usage(&mount.mountpoint) {
                            Ok(usage) => {
                                row["total"] = json!(usage.total);
                                row["used"] = json!(usage.used);
                                row["free"] = json!(usage.free);
                                row["percent"] = json!(usage.percent);
                            }
                            Err(error) => row["error"] = json!(error.to_string()),
                        }
                    }
                    rows.push(row);
                }
            }
            Err(error) => warnings.push(tr!("Disk capacity is unavailable: {error}", error = error.to_string())),
        }
        (rows, warnings)
    }

    fn sample_network(&mut self, interval: f64, warnings: &mut Vec<String>) -> Value {
        let mut result = json!({"recv_bytes_per_sec": null, "sent_bytes_per_sec": null,
                                "recv_bytes": null, "sent_bytes": null, "interfaces": []});
        let mut previous = BTreeMap::new();
        match self.ps.net_io_counters() {
            Ok(counters) => {
                let stats: BTreeMap<String, crate::source::NetStats> = self.ps.net_if_stats().unwrap_or_default().into_iter().collect();
                let addresses: BTreeMap<String, Vec<crate::source::NetAddress>> = if self.include_addresses {
                    self.ps.net_if_addrs().unwrap_or_default().into_iter().collect()
                } else {
                    BTreeMap::new()
                };
                let counter_map: BTreeMap<String, crate::source::NetCounters> = counters.iter().cloned().collect();
                let names: BTreeSet<String> = counter_map.keys().chain(stats.keys()).cloned().collect();
                let mut interfaces = Vec::new();
                for name in names {
                    let (value, state) = (counter_map.get(&name), stats.get(&name));
                    let old = self.network_previous.get(&name).cloned().unwrap_or((None, None));
                    let recv = value.and_then(|v| v.bytes_recv);
                    let sent = value.and_then(|v| v.bytes_sent);
                    let mut row = json!({
                        "name": name, "up": state.and_then(|s| s.isup), "speed_mbps": state.and_then(|s| s.speed),
                        "mtu": state.and_then(|s| s.mtu),
                        "recv_bytes": opt_u(recv), "recv_bytes_per_sec": recv.map(|r| rate(Some(r as f64), old.0, interval)),
                        "sent_bytes": opt_u(sent), "sent_bytes_per_sec": sent.map(|s| rate(Some(s as f64), old.1, interval)),
                    });
                    if self.include_addresses {
                        row["addresses"] = Value::Array(addresses.get(&name).map(|list| list.iter().map(|item| json!({
                            "family": item.family, "address": item.address, "netmask": item.netmask, "broadcast": item.broadcast,
                        })).collect()).unwrap_or_default());
                    }
                    previous.insert(name.clone(), (recv.map(|v| v as f64), sent.map(|v| v as f64)));
                    interfaces.push(row);
                }
                // Sum per-interface deltas, so hotplug cannot create aggregate spikes.
                if !counter_map.is_empty() {
                    for field in ["recv_bytes", "sent_bytes"] {
                        let sum: u64 = interfaces.iter().map(|r| r[field].as_u64().unwrap_or(0)).sum();
                        result[field] = json!(sum);
                    }
                    for field in ["recv_bytes_per_sec", "sent_bytes_per_sec"] {
                        let sum: f64 = interfaces.iter().map(|r| r[field].as_f64().unwrap_or(0.0)).sum();
                        result[field] = json!(sum);
                    }
                }
                result["interfaces"] = Value::Array(interfaces);
            }
            Err(error) => warnings.push(tr!("Network activity is unavailable: {error}", error = error.to_string())),
        }
        self.network_previous = previous;
        result
    }

    fn sample_gpus(&mut self, now: f64, warnings: &mut Vec<String>) -> Value {
        if self.gpu_time.is_none_or(|t| now - t >= GPU_INTERVAL) {
            let (mut rows, mut errors) = (Vec::new(), Vec::new());
            if let Some(command) = &self.gpu_command {
                match self.nvidia_gpus(command) {
                    Ok(found) => rows = found,
                    Err(error) => errors.push(format!("NVIDIA GPU information is unavailable: {error}")),
                }
            }
            if let Some(root) = &self.drm_root {
                rows.extend(drm_gpus(root, !rows.is_empty()));
            }
            self.gpus = rows;
            self.gpu_warnings = errors;
            self.gpu_time = Some(now);
        }
        warnings.extend(self.gpu_warnings.iter().cloned());
        Value::Array(self.gpus.clone())
    }

    fn nvidia_gpus(&self, command: &Path) -> Result<Vec<Value>, String> {
        let fields = "uuid,name,utilization.gpu,memory.total,memory.used,temperature.gpu";
        let argv = vec![command.to_string_lossy().into_owned(), format!("--query-gpu={fields}"),
                        "--format=csv,noheader,nounits".to_owned()];
        let result = (self.gpu_runner)(&argv, GPU_TIMEOUT)?;
        if result.returncode != 0 {
            let text = if result.stderr.is_empty() { "nvidia-smi failed" } else { &result.stderr };
            return Err(text.trim().chars().take(240).collect());
        }
        if result.stdout.len() > 1024 * 1024 {
            return Err("nvidia-smi returned an unexpectedly large response".into());
        }
        let mut rows = Vec::new();
        for values in csv_rows(&result.stdout)? {
            if values.len() != 6 {
                continue;
            }
            let values: Vec<&str> = values.iter().map(|v| v.trim()).collect();
            let (total, used, utilization) = (number_text(values[3]), number_text(values[4]), number_text(values[2]));
            rows.push(json!({
                "id": values[0], "name": values[1], "source": "nvidia-smi",
                "utilization_percent": utilization.map(percent),
                "memory_total": total.map(|t| (t * 1024f64.powi(2)) as i64),
                "memory_used": used.map(|u| (u * 1024f64.powi(2)) as i64),
                "temp_c": number_text(values[5]),
            }));
        }
        Ok(rows)
    }
}

/// `csv.reader(..., skipinitialspace=True)` for nvidia-smi output.
pub fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut chars = line.chars().peekable();
        let mut start = true;
        let mut quoted = false;
        while let Some(c) = chars.next() {
            if start && c == ' ' {
                continue;
            }
            if start && c == '"' {
                quoted = true;
                start = false;
                continue;
            }
            start = false;
            if quoted {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        field.push('"');
                        chars.next();
                    } else {
                        quoted = false;
                    }
                } else {
                    field.push(c);
                }
            } else if c == ',' {
                fields.push(std::mem::take(&mut field));
                start = true;
            } else {
                field.push(c);
            }
        }
        if quoted {
            return Err("unexpected end of data".into());
        }
        if line.is_empty() {
            continue;
        }
        fields.push(field);
        rows.push(fields);
    }
    Ok(rows)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()).map(|s| s.trim().to_owned())
}

/// `_drm_gpus`: AMD/Intel sysfs telemetry.
pub fn drm_gpus(root: &Path, skip_nvidia: bool) -> Vec<Value> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir(root).map(|d| d.flatten().map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card")
            && n[4..].chars().next().is_some_and(|c| c.is_ascii_digit())))
        .collect()).unwrap_or_default();
    cards.sort();
    let mut rows = Vec::new();
    for card in cards {
        let name = card.file_name().unwrap().to_string_lossy().into_owned();
        if !name[4..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let device = card.join("device");
        let vendor = read_trimmed(&device.join("vendor"));
        if vendor.as_deref() == Some("0x10de") && skip_nvidia {
            continue;
        }
        let number = |file: &str| read_trimmed(&device.join(file)).and_then(|t| number_text(&t));
        let utilization = number("gpu_busy_percent");
        let total = number("mem_info_vram_total");
        let used = number("mem_info_vram_used");
        let mut hwmons: Vec<PathBuf> = std::fs::read_dir(device.join("hwmon")).map(|d| d.flatten().map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("hwmon")))
            .map(|p| p.join("temp1_input")).filter(|p| p.exists()).collect()).unwrap_or_default();
        hwmons.sort();
        let temperature = hwmons.iter().filter_map(|p| read_trimmed(p).and_then(|t| number_text(&t))).next().map(|v| v / 1000.0);
        if utilization.is_none() && total.is_none() && used.is_none() && temperature.is_none() {
            continue;
        }
        let maker = match vendor.as_deref() { Some("0x1002") => "AMD", Some("0x8086") => "Intel", Some("0x10de") => "NVIDIA", _ => "" };
        let product = read_trimmed(&device.join("product_name")).filter(|p| !p.is_empty());
        let display = product.unwrap_or_else(|| format!("{maker} GPU ({name})").trim().to_owned());
        rows.push(json!({"id": name, "name": display, "source": "drm",
                         "utilization_percent": utilization.map(percent),
                         "memory_total": total.map(|t| t as i64), "memory_used": used.map(|u| u as i64),
                         "temp_c": temperature}));
    }
    rows
}

/// An explicitly requested process operation could not safely proceed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessActionError(pub String);

impl std::fmt::Display for ProcessActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProcessActionError {}

fn verified_process(pid: i64, create_time: Option<f64>, ps: &dyn Source, protect: bool)
                    -> Result<Result<Box<dyn crate::source::Process>, PsError>, ProcessActionError> {
    if pid < 1 {
        return Err(ProcessActionError(tr!("Choose a valid process.")));
    }
    if protect && (pid <= 1 || pid == std::process::id() as i64) {
        return Err(ProcessActionError(tr!("This process is protected and cannot be changed here.")));
    }
    let expected = create_time.filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| ProcessActionError(tr!("The process identity is unavailable. Refresh the process list.")))?;
    let process = match ps.process(pid) {
        Ok(process) => process,
        Err(error) => return Ok(Err(error)),
    };
    match process.create_time() {
        Ok(created) if created != expected => Err(ProcessActionError(
            tr!("The selected process has exited and its process ID was reused. Refresh the list."))),
        Ok(_) => Ok(Ok(process)),
        Err(error) => Ok(Err(error)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Terminate,
    Kill,
    Suspend,
    Resume,
    Nice(i32),
}

fn action(pid: i64, create_time: Option<f64>, action: Action, ps: &dyn Source) -> Result<bool, ProcessActionError> {
    let process = verified_process(pid, create_time, ps, true)?;
    let result = process.and_then(|process| match action {
        // The control methods recheck identity before sending the signal.
        Action::Terminate => process.terminate(),
        Action::Kill => process.kill(),
        Action::Suspend => process.suspend(),
        Action::Resume => process.resume(),
        Action::Nice(value) => process.set_nice(value),
    });
    match result {
        Ok(()) => Ok(true),
        Err(e) if e.is_no_such_process() => Err(ProcessActionError(tr!("The selected process has already exited."))),
        Err(PsError::AccessDenied(_)) => Err(ProcessActionError(tr!("You do not have permission to change this process."))),
        Err(error) => Err(ProcessActionError(tr!("The process could not be changed: {error}", error = error.to_string()))),
    }
}

/// Request SIGTERM, or SIGKILL only when `force` was explicitly chosen.
pub fn end_process(pid: i64, create_time: Option<f64>, force: bool, ps: &dyn Source) -> Result<bool, ProcessActionError> {
    action(pid, create_time, if force { Action::Kill } else { Action::Terminate }, ps)
}

pub fn suspend_process(pid: i64, create_time: Option<f64>, ps: &dyn Source) -> Result<bool, ProcessActionError> {
    action(pid, create_time, Action::Suspend, ps)
}

pub fn resume_process(pid: i64, create_time: Option<f64>, ps: &dyn Source) -> Result<bool, ProcessActionError> {
    action(pid, create_time, Action::Resume, ps)
}

pub fn set_process_priority(pid: i64, create_time: Option<f64>, nice: i64, ps: &dyn Source) -> Result<bool, ProcessActionError> {
    if !(-20..=19).contains(&nice) {
        return Err(ProcessActionError(tr!("Process priority must be an integer between -20 and 19.")));
    }
    action(pid, create_time, Action::Nice(nice as i32), ps)
}

/// Explicit process inspection; the command line may contain private arguments.
/// Environment variables, files and network connections are never collected.
/// Permission-denied fields stay `null`. A stale selection fails like the controls.
pub fn process_details(pid: i64, create_time: Option<f64>, ps: &dyn Source) -> Result<Map<String, Value>, ProcessActionError> {
    let fail = |error: PsError| -> ProcessActionError {
        match error {
            e if e.is_no_such_process() => ProcessActionError(tr!("The selected process has already exited.")),
            PsError::AccessDenied(_) => ProcessActionError(tr!("You do not have permission to inspect this process.")),
            e => ProcessActionError(tr!("The process could not be inspected: {error}", error = e.to_string())),
        }
    };
    let process = verified_process(pid, create_time, ps, false)?.map_err(fail)?;
    let mut result = Map::new();
    result.insert("pid".into(), json!(pid));
    result.insert("create_time".into(), json!(create_time));
    fn field<T: Into<Value>>(value: PsResult<T>) -> Result<Value, PsError> {
        match value {
            Ok(v) => Ok(v.into()),
            Err(PsError::AccessDenied(_)) | Err(PsError::Zombie(_)) => Ok(Value::Null),
            Err(e) => Err(e),
        }
    }
    let entries: [(&str, Box<dyn Fn() -> Result<Value, PsError>>); 9] = [
        ("name", Box::new(|| field(process.name()))),
        ("user", Box::new(|| field(process.username()))),
        ("status", Box::new(|| field(process.status()))),
        ("executable", Box::new(|| field(process.exe()))),
        ("command_line", Box::new(|| field(process.cmdline()))),
        ("working_directory", Box::new(|| field(process.cwd()))),
        ("parent_pid", Box::new(|| field(process.ppid()))),
        ("nice", Box::new(|| field(process.nice()))),
        ("threads", Box::new(|| field(process.num_threads()))),
    ];
    for (key, read) in entries.iter() {
        result.insert((*key).into(), read().map_err(fail)?);
    }
    // Do not display detail from a reused PID if it changed during reads.
    if !process.is_running() {
        return Err(ProcessActionError(tr!("The selected process has already exited.")));
    }
    Ok(result)
}
