//! Real Linux [`Source`]: psutil's `_pslinux` reads done directly on `/proc`
//! and `/sys` below a root directory (`/` normally; a fake tree in tests).
//! Signals, `setpriority`, `statvfs` and `getifaddrs` act on the live system
//! only when the root is `/`.

use crate::source::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct ProcFs {
    root: PathBuf,
    users: Arc<Mutex<HashMap<u32, String>>>,
}

fn clock_ticks() -> f64 {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks as f64 } else { 100.0 }
}

fn page_size() -> u64 {
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 { size as u64 } else { 4096 }
}

fn os_error(error: &io::Error) -> PsError {
    PsError::Os(tessel_core::pyerr::os_error_text(error))
}

/// psutil's `wrap_exceptions` for per-process reads.
fn process_error(error: &io::Error, pid: i64, proc_dir: &Path) -> PsError {
    match error.raw_os_error() {
        Some(libc::EPERM) | Some(libc::EACCES) => PsError::AccessDenied(Some(pid)),
        Some(libc::ESRCH) => PsError::NoSuchProcess(pid),
        Some(libc::ENOENT) => if proc_dir.exists() { PsError::Zombie(pid) } else { PsError::NoSuchProcess(pid) },
        _ => os_error(error),
    }
}

/// `/proc/<pid>/stat` fields after the command name; index 0 = field 3 (state).
struct Stat {
    comm: String,
    fields: Vec<String>,
}

impl Stat {
    fn parse(text: &str) -> Option<Stat> {
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let comm = text[open + 1..close].to_owned();
        let fields = text[close + 1..].split_whitespace().map(str::to_owned).collect();
        Some(Stat { comm, fields })
    }

    /// 1-based `proc(5)` field number.
    fn field(&self, number: usize) -> Option<&str> {
        self.fields.get(number.checked_sub(3)?).map(String::as_str)
    }

    fn num(&self, number: usize) -> PsResult<f64> {
        self.field(number).and_then(|v| v.parse::<f64>().ok()).ok_or_else(|| PsError::Os("malformed stat".into()))
    }
}

pub fn status_name(code: &str) -> String {
    match code {
        "R" => "running", "S" => "sleeping", "D" => "disk-sleep", "T" => "stopped", "t" => "tracing-stop",
        "Z" => "zombie", "X" | "x" => "dead", "K" => "wake-kill", "W" => "waking", "P" => "parked", "I" => "idle",
        "L" => "locked", other => return other.to_owned(),
    }.to_owned()
}

fn read_text(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn unescape_mount(text: &str) -> String {
    // /proc/mounts octal escapes (\040 space, \011 tab, \012 newline, \134 backslash).
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
            let value = (bytes[i + 1] - b'0') as u32 * 64 + (bytes[i + 2] - b'0') as u32 * 8 + (bytes[i + 3] - b'0') as u32;
            out.push(value as u8);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// psutil `usage_percent(used, total, round_=1)`.
pub fn usage_percent(used: f64, total: f64) -> f64 {
    if total == 0.0 { 0.0 } else { crate::backend::py_round(used / total * 100.0, 1) }
}

impl ProcFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), users: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn live(&self) -> bool {
        self.root == Path::new("/")
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn meminfo(&self) -> PsResult<HashMap<String, u64>> {
        let text = read_text(&self.path("proc/meminfo")).map_err(|e| os_error(&e))?;
        Ok(text.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let key = fields.next()?.trim_end_matches(':').to_owned();
            let value = fields.next()?.parse::<u64>().ok()?;
            Some((key, value * 1024))
        }).collect())
    }

    fn cpuinfo_mhz(&self) -> Vec<f64> {
        read_text(&self.path("proc/cpuinfo")).map(|text| text.lines().filter_map(|line| {
            let lower = line.to_lowercase();
            if !lower.starts_with("cpu mhz") {
                return None;
            }
            line.split_once(':').and_then(|(_, v)| v.trim().parse::<f64>().ok())
        }).collect()).unwrap_or_default()
    }

    fn sorted_numbered(&self, dir: &str, prefix: &str, suffix: &str) -> Vec<PathBuf> {
        let mut found: Vec<(u64, PathBuf)> = std::fs::read_dir(self.path(dir)).map(|d| d.flatten().filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let number: String = name.strip_prefix(prefix)?.to_owned();
            if number.is_empty() || !number.chars().next()?.is_ascii_digit() {
                return None;
            }
            let digits: String = number.chars().take_while(|c| c.is_ascii_digit()).collect();
            let path = e.path().join(suffix);
            path.exists().then(|| (digits.parse().unwrap_or(0), path))
        }).collect()).unwrap_or_default();
        found.sort();
        found.into_iter().map(|(_, p)| p).collect()
    }

    fn username_for(&self, uid: u32) -> String {
        if let Some(name) = self.users.lock().unwrap().get(&uid) {
            return name.clone();
        }
        let name = if self.live() {
            lookup_user(uid)
        } else {
            read_text(&self.path("etc/passwd")).ok().and_then(|t| t.lines().find_map(|l| {
                let parts: Vec<&str> = l.split(':').collect();
                (parts.len() > 2 && parts[2].parse::<u32>().ok() == Some(uid)).then(|| parts[0].to_owned())
            }))
        }.unwrap_or_else(|| uid.to_string());
        self.users.lock().unwrap().insert(uid, name.clone());
        name
    }

    fn btime(&self) -> PsResult<f64> {
        let text = read_text(&self.path("proc/stat")).map_err(|e| os_error(&e))?;
        text.lines().find_map(|l| l.strip_prefix("btime ").and_then(|v| v.trim().parse::<f64>().ok()))
            .ok_or_else(|| PsError::Os("line 'btime' not found in /proc/stat".into()))
    }
}

pub fn lookup_user(uid: u32) -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; 16384];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let code = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
    if code != 0 || result.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }.to_string_lossy().into_owned())
}

/// The effective user name (`getpass.getuser()`: LOGNAME/USER/LNAME/USERNAME, then pwd).
pub fn current_user() -> String {
    for key in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return value;
            }
        }
    }
    let uid = unsafe { libc::getuid() };
    lookup_user(uid).unwrap_or_else(|| uid.to_string())
}

pub struct ProcProcess {
    pid: i64,
    dir: PathBuf,
    boot: f64,
    live: bool,
    created: f64,
    users: Arc<Mutex<HashMap<u32, String>>>,
    source_root: PathBuf,
}

impl ProcProcess {
    fn read(&self, name: &str) -> PsResult<String> {
        read_text(&self.dir.join(name)).map_err(|e| process_error(&e, self.pid, &self.dir))
    }

    fn stat(&self) -> PsResult<Stat> {
        let text = self.read("stat")?;
        Stat::parse(&text).ok_or_else(|| PsError::Os("malformed stat".into()))
    }

    fn start_time(&self, stat: &Stat) -> PsResult<f64> {
        Ok(self.boot + stat.num(22)? / clock_ticks())
    }

    fn link(&self, name: &str) -> PsResult<String> {
        match std::fs::read_link(self.dir.join(name)) {
            Ok(path) => Ok(path.to_string_lossy().into_owned()),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) || e.raw_os_error() == Some(libc::ESRCH) => {
                if self.stat().ok().and_then(|s| s.field(3).map(|f| f == "Z")).unwrap_or(false) {
                    return Err(PsError::Zombie(self.pid));
                }
                // May be a kernel thread.
                if self.dir.exists() { Ok(String::new()) } else { Err(PsError::NoSuchProcess(self.pid)) }
            }
            Err(e) => Err(process_error(&e, self.pid, &self.dir)),
        }
    }

    fn check_identity(&self) -> PsResult<()> {
        if self.is_running() { Ok(()) } else { Err(PsError::NoSuchProcess(self.pid)) }
    }

    fn signal(&self, signal: libc::c_int) -> PsResult<()> {
        self.check_identity()?;
        if !self.live {
            return Err(PsError::Os("signals are only sent on the live system".into()));
        }
        if unsafe { libc::kill(self.pid as libc::pid_t, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        Err(match error.raw_os_error() {
            Some(libc::ESRCH) => PsError::NoSuchProcess(self.pid),
            Some(libc::EPERM) | Some(libc::EACCES) => PsError::AccessDenied(Some(self.pid)),
            _ => os_error(&error),
        })
    }
}

impl Process for ProcProcess {
    fn pid(&self) -> i64 { self.pid }

    fn create_time(&self) -> PsResult<f64> { Ok(self.created) }

    fn name(&self) -> PsResult<String> {
        let stat = self.stat()?;
        let name = stat.comm.clone();
        // The kernel truncates comm to 15 bytes; psutil extends it from argv[0].
        if name.len() >= 15 {
            if let Ok(args) = self.cmdline() {
                if let Some(first) = args.first() {
                    let base = Path::new(first).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if base.starts_with(&name) {
                        return Ok(base);
                    }
                }
            }
        }
        Ok(name)
    }

    fn username(&self) -> PsResult<String> {
        let text = self.read("status")?;
        let uid = text.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u32>().ok()).ok_or_else(|| PsError::Os("malformed status".into()))?;
        let source = ProcFs { root: self.source_root.clone(), users: self.users.clone() };
        Ok(source.username_for(uid))
    }

    fn status(&self) -> PsResult<String> {
        let stat = self.stat()?;
        Ok(status_name(stat.field(3).unwrap_or("?")))
    }

    fn cpu_times(&self) -> PsResult<ProcCpuTimes> {
        let stat = self.stat()?;
        Ok(ProcCpuTimes { user: stat.num(14)? / clock_ticks(), system: stat.num(15)? / clock_ticks() })
    }

    fn rss(&self) -> PsResult<u64> {
        let text = self.read("statm")?;
        let pages = text.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| PsError::Os("malformed statm".into()))?;
        Ok(pages * page_size())
    }

    fn io_counters(&self) -> PsResult<ProcIo> {
        let text = self.read("io")?;
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().parse::<u64>().ok());
        Ok(ProcIo { read_bytes: field("read_bytes:"), write_bytes: field("write_bytes:") })
    }

    fn num_threads(&self) -> PsResult<i64> {
        Ok(self.stat()?.num(20)? as i64)
    }

    fn exe(&self) -> PsResult<String> {
        // psutil's Process.exe(): a denied or empty link falls back to an
        // absolute, executable argv[0].
        let guess = || -> Option<String> {
            let first = self.cmdline().ok()?.into_iter().next()?;
            let path = Path::new(&first);
            let c = std::ffi::CString::new(first.as_bytes()).ok()?;
            (path.is_absolute() && path.is_file() && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0).then_some(first)
        };
        let path = match self.link("exe") {
            Ok(path) if path.is_empty() => return Ok(guess().unwrap_or_default()),
            Ok(path) => path,
            Err(PsError::AccessDenied(pid)) => return guess().ok_or(PsError::AccessDenied(pid)),
            Err(error) => return Err(error),
        };
        let path = path.split('\0').next().unwrap_or_default().to_owned();
        // psutil's readlink(): drop " (deleted)" unless that literal path exists.
        if let Some(stripped) = path.strip_suffix(" (deleted)") {
            if std::fs::symlink_metadata(&path).is_err() {
                return Ok(stripped.to_owned());
            }
        }
        Ok(path)
    }

    fn cmdline(&self) -> PsResult<Vec<String>> {
        let data = self.read("cmdline")?;
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let sep = if data.ends_with('\0') { '\0' } else { ' ' };
        let data = data.strip_suffix(sep).unwrap_or(&data);
        let mut parts: Vec<String> = data.split(sep).map(str::to_owned).collect();
        if sep == '\0' && parts.len() == 1 && data.contains(' ') {
            parts = data.split(' ').map(str::to_owned).collect();
        }
        Ok(parts)
    }

    fn cwd(&self) -> PsResult<String> { self.link("cwd") }

    fn ppid(&self) -> PsResult<i64> { Ok(self.stat()?.num(4)? as i64) }

    fn nice(&self) -> PsResult<i64> { Ok(self.stat()?.num(19)? as i64) }

    fn is_running(&self) -> bool {
        match self.stat() {
            Ok(stat) => self.start_time(&stat).map(|t| t == self.created).unwrap_or(false),
            Err(PsError::Zombie(_)) => true,
            Err(_) => false,
        }
    }

    fn terminate(&self) -> PsResult<()> { self.signal(libc::SIGTERM) }
    fn kill(&self) -> PsResult<()> { self.signal(libc::SIGKILL) }
    fn suspend(&self) -> PsResult<()> { self.signal(libc::SIGSTOP) }
    fn resume(&self) -> PsResult<()> { self.signal(libc::SIGCONT) }

    fn set_nice(&self, value: i32) -> PsResult<()> {
        self.check_identity()?;
        if !self.live {
            return Err(PsError::Os("priorities are only changed on the live system".into()));
        }
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, self.pid as libc::id_t, value) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        Err(match error.raw_os_error() {
            Some(libc::ESRCH) => PsError::NoSuchProcess(self.pid),
            Some(libc::EPERM) | Some(libc::EACCES) => PsError::AccessDenied(Some(self.pid)),
            _ => os_error(&error),
        })
    }
}

impl Source for ProcFs {
    fn cpu_count(&self, logical: bool) -> Option<usize> {
        if logical {
            let text = read_text(&self.path("proc/stat")).ok()?;
            let count = text.lines().filter(|l| l.starts_with("cpu") && l[3..].starts_with(|c: char| c.is_ascii_digit())).count();
            return (count > 0).then_some(count);
        }
        let mut siblings = BTreeSet::new();
        let cpus = self.sorted_numbered("sys/devices/system/cpu", "cpu", "topology");
        for file in ["core_cpus_list", "thread_siblings_list"] {
            for topology in &cpus {
                if let Ok(text) = read_text(&topology.join(file)) {
                    siblings.insert(text.trim().to_owned());
                }
            }
            if !siblings.is_empty() {
                return Some(siblings.len());
            }
        }
        // /proc/cpuinfo "physical id" → "cpu cores".
        let text = read_text(&self.path("proc/cpuinfo")).ok()?;
        let mut mapping: BTreeMap<i64, i64> = BTreeMap::new();
        let mut current: HashMap<String, i64> = HashMap::new();
        for line in text.lines().chain(std::iter::once("")) {
            let line = line.trim().to_lowercase();
            if line.is_empty() {
                if let (Some(id), Some(cores)) = (current.get("physical id"), current.get("cpu cores")) {
                    mapping.insert(*id, *cores);
                }
                current.clear();
            } else if line.starts_with("physical id") || line.starts_with("cpu cores") {
                if let Some((key, value)) = line.split_once(':') {
                    if let Ok(value) = value.trim().parse() {
                        current.insert(key.trim().to_owned(), value);
                    }
                }
            }
        }
        let total: i64 = mapping.values().sum();
        (total > 0).then_some(total as usize)
    }

    fn cpu_times_percpu(&self) -> PsResult<Vec<CpuTimes>> {
        let text = read_text(&self.path("proc/stat")).map_err(|e| os_error(&e))?;
        let ticks = clock_ticks();
        Ok(text.lines().filter(|l| l.starts_with("cpu") && l[3..].starts_with(|c: char| c.is_ascii_digit())).map(|line| {
            let v: Vec<f64> = line.split_whitespace().skip(1).map(|f| f.parse::<f64>().unwrap_or(0.0) / ticks).collect();
            let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
            CpuTimes { user: at(0), nice: at(1), system: at(2), idle: at(3), iowait: at(4), irq: at(5), softirq: at(6),
                       steal: at(7), guest: at(8), guest_nice: at(9) }
        }).collect())
    }

    fn cpu_freq(&self) -> PsResult<Option<CpuFreq>> {
        let mut paths = self.sorted_numbered("sys/devices/system/cpu/cpufreq", "policy", "");
        if paths.is_empty() {
            paths = self.sorted_numbered("sys/devices/system/cpu", "cpu", "cpufreq");
        }
        let cpuinfo = self.cpuinfo_mhz();
        let mut rows: Vec<(f64, Option<f64>, Option<f64>)> = Vec::new();
        if paths.is_empty() {
            rows = cpuinfo.iter().map(|mhz| (*mhz, Some(0.0), Some(0.0))).collect();
        }
        for (index, path) in paths.iter().enumerate() {
            let read = |name: &str| read_text(&path.join(name)).ok().and_then(|t| t.trim().parse::<f64>().ok());
            let current = if paths.len() == cpuinfo.len() { Some(cpuinfo[index] * 1000.0) } else { read("scaling_cur_freq") };
            let Some(current) = current.or_else(|| read("cpuinfo_cur_freq")) else {
                let online = self.path(&format!("sys/devices/system/cpu/cpu{index}/online"));
                if read_text(&online).map(|t| t == "0\n").unwrap_or(false) {
                    rows.push((0.0, Some(0.0), Some(0.0)));
                    continue;
                }
                return Err(PsError::Os("can't find current frequency file".into()));
            };
            // `int(curr) / 1000`.
            rows.push((current.trunc() / 1000.0, read("scaling_min_freq").map(|v| v.trunc() / 1000.0),
                       read("scaling_max_freq").map(|v| v.trunc() / 1000.0)));
        }
        let count = rows.len() as f64;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() == 1 {
            let (current, min, max) = rows[0];
            return Ok(Some(CpuFreq { current: Some(current), min, max }));
        }
        let current = rows.iter().map(|r| r.0).sum::<f64>() / count;
        let unknown = rows.iter().any(|r| r.1.is_none() || r.2.is_none());
        let (min, max) = if unknown { (None, None) } else {
            (Some(rows.iter().map(|r| r.1.unwrap()).sum::<f64>() / count), Some(rows.iter().map(|r| r.2.unwrap()).sum::<f64>() / count))
        };
        Ok(Some(CpuFreq { current: Some(current), min, max }))
    }

    fn virtual_memory(&self) -> PsResult<VirtualMemory> {
        let mems = self.meminfo()?;
        let get = |key: &str| mems.get(key).copied();
        let total = get("MemTotal").ok_or_else(|| PsError::Os("'MemTotal:'".into()))?;
        let free = get("MemFree").ok_or_else(|| PsError::Os("'MemFree:'".into()))?;
        let buffers = get("Buffers").unwrap_or(0);
        let cached = get("Cached").map(|c| c + get("SReclaimable").unwrap_or(0)).unwrap_or(0);
        let mut available = match get("MemAvailable") {
            Some(value) if value != 0 => value as i64,
            _ => {
                // calculate_avail_vmem: free + inactive_file + active_file + reclaimable - watermarks.
                let low = std::fs::read_to_string(self.path("proc/zoneinfo")).ok().map(|t| t.lines()
                    .filter_map(|l| l.trim().strip_prefix("low ").and_then(|v| v.trim().parse::<i64>().ok())).sum::<i64>() * page_size() as i64);
                match (get("Active(file)"), get("Inactive(file)"), get("SReclaimable"), low) {
                    (Some(active), Some(inactive), Some(reclaimable), Some(low)) => {
                        let file = active as i64 + inactive as i64;
                        free as i64 - low + file - (file / 2).min(low) + reclaimable as i64 - (reclaimable as i64 / 2).min(low)
                    }
                    _ => free as i64 + cached as i64,
                }
            }
        };
        if available < 0 {
            available = 0;
        } else if available as u64 > total {
            available = free as i64;
        }
        let available = available as u64;
        Ok(VirtualMemory { total, used: total - available, available, free,
                           percent: usage_percent((total - available) as f64, total as f64), cached, buffers })
    }

    fn swap_memory(&self) -> PsResult<SwapMemory> {
        let mems = self.meminfo()?;
        let total = mems.get("SwapTotal").copied().unwrap_or(0);
        let free = mems.get("SwapFree").copied().unwrap_or(0);
        let used = total.saturating_sub(free);
        Ok(SwapMemory { total, used, free, percent: usage_percent(used as f64, total as f64) })
    }

    fn pids(&self) -> PsResult<Vec<i64>> {
        let mut pids: Vec<i64> = std::fs::read_dir(self.path("proc")).map_err(|e| os_error(&e))?.flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<i64>().ok())).collect();
        pids.sort_unstable();
        Ok(pids)
    }

    fn process(&self, pid: i64) -> PsResult<Box<dyn Process>> {
        if pid < 0 {
            return Err(PsError::Os(format!("pid must be a positive integer (got {pid})")));
        }
        let dir = self.path(&format!("proc/{pid}"));
        let boot = self.btime()?;
        let mut process = ProcProcess { pid, dir, boot, live: self.live(), created: 0.0, users: self.users.clone(),
                                        source_root: self.root.clone() };
        let stat = process.stat()?;
        process.created = process.start_time(&stat)?;
        Ok(Box::new(process))
    }

    fn disk_io_counters(&self) -> PsResult<(Vec<(String, DiskCounters)>, Option<DiskCounters>)> {
        let text = read_text(&self.path("proc/diskstats")).map_err(|e| os_error(&e))?;
        let mut devices = Vec::new();
        let (mut read, mut write, mut any) = (0u64, 0u64, false);
        for line in text.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 14 {
                continue;
            }
            let name = fields[2].to_owned();
            let sectors = |i: usize| fields[i].parse::<u64>().ok().map(|v| v * 512);
            let counters = DiskCounters { read_bytes: sectors(5), write_bytes: sectors(9) };
            // Totals include whole disks only (/sys/block/<name> exists).
            if self.path(&format!("sys/block/{}", name.replace('/', "!"))).exists() {
                read += counters.read_bytes.unwrap_or(0);
                write += counters.write_bytes.unwrap_or(0);
                any = true;
            }
            devices.push((name, counters));
        }
        let aggregate = any.then_some(DiskCounters { read_bytes: Some(read), write_bytes: Some(write) });
        Ok((devices, aggregate))
    }

    fn disk_partitions(&self) -> PsResult<Vec<Partition>> {
        let mut fstypes = BTreeSet::new();
        if let Ok(text) = read_text(&self.path("proc/filesystems")) {
            for line in text.lines() {
                let line = line.trim();
                if !line.starts_with("nodev") {
                    fstypes.insert(line.to_owned());
                } else if line.split('\t').nth(1) == Some("zfs") {
                    fstypes.insert("zfs".to_owned());
                }
            }
        }
        let mounts = ["proc/self/mounts", "proc/mounts"].iter().map(|p| self.path(p)).find(|p| p.exists())
            .unwrap_or_else(|| self.path("proc/self/mounts"));
        let text = read_text(&mounts).map_err(|e| os_error(&e))?;
        Ok(text.lines().filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 {
                return None;
            }
            let device = if fields[0] == "none" { String::new() } else { unescape_mount(fields[0]) };
            let fstype = fields[2].to_owned();
            if device.is_empty() || !fstypes.contains(&fstype) {
                return None;
            }
            Some(Partition { device, mountpoint: unescape_mount(fields[1]), fstype })
        }).collect())
    }

    fn disk_usage(&self, mountpoint: &str) -> PsResult<DiskUsage> {
        let path = std::ffi::CString::new(mountpoint).map_err(|_| PsError::Os("embedded null byte".into()))?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
            let error = io::Error::last_os_error();
            return Err(PsError::Os(tessel_core::pyerr::os_error_path(&error, Path::new(mountpoint))));
        }
        let frsize = st.f_frsize as u64;
        let total = st.f_blocks as u64 * frsize;
        let avail_root = st.f_bfree as u64 * frsize;
        let avail_user = st.f_bavail as u64 * frsize;
        let used = total.saturating_sub(avail_root);
        Ok(DiskUsage { total, used, free: avail_user, percent: usage_percent(used as f64, (used + avail_user) as f64) })
    }

    fn net_io_counters(&self) -> PsResult<Vec<(String, NetCounters)>> {
        let text = read_text(&self.path("proc/net/dev")).map_err(|e| os_error(&e))?;
        Ok(text.lines().skip(2).filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let fields: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            Some((name.trim().to_owned(), NetCounters { bytes_recv: fields.first().copied(), bytes_sent: fields.get(8).copied() }))
        }).collect())
    }

    fn net_if_stats(&self) -> PsResult<Vec<(String, NetStats)>> {
        let names: Vec<String> = self.net_io_counters()?.into_iter().map(|(n, _)| n).collect();
        Ok(names.into_iter().filter_map(|name| {
            let dir = self.path(&format!("sys/class/net/{name}"));
            if !dir.exists() {
                return None; // ENODEV: skipped like psutil.
            }
            let read = |f: &str| read_text(&dir.join(f)).ok().map(|t| t.trim().to_owned());
            let flags = read("flags").and_then(|f| i64::from_str_radix(f.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
            let oper = read("operstate").unwrap_or_default();
            // IFF_RUNNING = IFF_UP and operationally up (or unknown, like lo).
            let running = flags & libc::IFF_UP as i64 != 0 && (oper == "up" || oper == "unknown");
            let speed = read("speed").and_then(|s| s.parse::<i64>().ok()).map(|s| s.max(0)).unwrap_or(0);
            let mtu = read("mtu").and_then(|s| s.parse::<i64>().ok());
            Some((name, NetStats { isup: Some(running), speed: Some(speed), mtu }))
        }).collect())
    }

    fn net_if_addrs(&self) -> PsResult<Vec<(String, Vec<NetAddress>)>> {
        if !self.live() {
            return Ok(Vec::new());
        }
        Ok(interface_addresses())
    }

    fn boot_time(&self) -> PsResult<f64> {
        self.btime()
    }
}

fn sockaddr_text(address: *const libc::sockaddr) -> Option<(i32, String)> {
    if address.is_null() {
        return None;
    }
    let family = unsafe { (*address).sa_family } as i32;
    match family {
        libc::AF_INET => {
            let raw = unsafe { *(address as *const libc::sockaddr_in) };
            Some((family, std::net::Ipv4Addr::from(u32::from_be(raw.sin_addr.s_addr)).to_string()))
        }
        libc::AF_INET6 => {
            let raw = unsafe { *(address as *const libc::sockaddr_in6) };
            Some((family, std::net::Ipv6Addr::from(raw.sin6_addr.s6_addr).to_string()))
        }
        libc::AF_PACKET => {
            let raw = unsafe { *(address as *const libc::sockaddr_ll) };
            let mac: Vec<String> = raw.sll_addr[..(raw.sll_halen as usize).min(8)].iter().map(|b| format!("{b:02x}")).collect();
            Some((family, mac.join(":")))
        }
        _ => None,
    }
}

/// `psutil.net_if_addrs()` via getifaddrs.
pub fn interface_addresses() -> Vec<(String, Vec<NetAddress>)> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Vec::new();
    }
    let mut result: Vec<(String, Vec<NetAddress>)> = Vec::new();
    let mut cursor = list;
    while !cursor.is_null() {
        let entry = unsafe { &*cursor };
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }.to_string_lossy().into_owned();
        if let Some((family, address)) = sockaddr_text(entry.ifa_addr) {
            let netmask = sockaddr_text(entry.ifa_netmask).map(|(_, t)| t);
            let broadcast = if entry.ifa_flags & libc::IFF_BROADCAST as u32 != 0 {
                sockaddr_text(entry.ifa_ifu).map(|(_, t)| t)
            } else {
                None
            };
            let family = match family { libc::AF_INET => "AF_INET".into(), libc::AF_INET6 => "AF_INET6".into(),
                                        libc::AF_PACKET => "AF_LINK".into(), other => other.to_string() };
            let item = NetAddress { family, address, netmask, broadcast };
            match result.iter_mut().find(|(n, _)| *n == name) {
                Some((_, items)) => items.push(item),
                None => result.push((name, vec![item])),
            }
        }
        cursor = entry.ifa_next;
    }
    unsafe { libc::freeifaddrs(list) };
    result
}
