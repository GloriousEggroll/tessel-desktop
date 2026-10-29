//! The psutil surface monitor_backend.py uses, as a trait. `procfs::ProcFs` is
//! the real Linux implementation (direct /proc + /sys reads under a root path);
//! tests supply fakes exactly like the Python `FakePsutil`.

use std::fmt;

/// psutil's exception hierarchy, reduced to what the backend distinguishes.
/// `Zombie` is a `NoSuchProcess` subclass in psutil.
#[derive(Debug, Clone, PartialEq)]
pub enum PsError {
    NoSuchProcess(i64),
    Zombie(i64),
    AccessDenied(Option<i64>),
    /// OSError / ValueError / NotImplementedError text.
    Os(String),
}

impl PsError {
    pub fn is_no_such_process(&self) -> bool {
        matches!(self, PsError::NoSuchProcess(_) | PsError::Zombie(_))
    }
}

impl fmt::Display for PsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PsError::NoSuchProcess(pid) => write!(f, "process no longer exists (pid={pid})"),
            PsError::Zombie(pid) => write!(f, "process still exists but it's a zombie (pid={pid})"),
            PsError::AccessDenied(Some(pid)) => write!(f, "(pid={pid})"),
            PsError::AccessDenied(None) => f.write_str(""),
            PsError::Os(text) => f.write_str(text),
        }
    }
}

pub type PsResult<T> = Result<T, PsError>;

/// `/proc/stat` per-CPU times in seconds (all psutil fields; missing = 0).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: f64,
    pub nice: f64,
    pub system: f64,
    pub idle: f64,
    pub iowait: f64,
    pub irq: f64,
    pub softirq: f64,
    pub steal: f64,
    pub guest: f64,
    pub guest_nice: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuFreq {
    pub current: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VirtualMemory {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub free: u64,
    pub percent: f64,
    pub cached: u64,
    pub buffers: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SwapMemory {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub percent: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DiskCounters {
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Partition {
    pub device: String,
    pub mountpoint: String,
    pub fstype: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiskUsage {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub percent: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetCounters {
    pub bytes_recv: Option<u64>,
    pub bytes_sent: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetStats {
    pub isup: Option<bool>,
    pub speed: Option<i64>,
    pub mtu: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetAddress {
    /// `AF_INET`, `AF_INET6`, `AF_LINK` or the numeric family.
    pub family: String,
    pub address: String,
    pub netmask: Option<String>,
    pub broadcast: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcCpuTimes {
    pub user: f64,
    pub system: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcIo {
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
}

/// One `psutil.Process`. Control methods recheck identity (psutil's
/// `_raise_if_pid_reused`) before signalling.
pub trait Process {
    fn pid(&self) -> i64;
    fn create_time(&self) -> PsResult<f64>;
    fn name(&self) -> PsResult<String>;
    fn username(&self) -> PsResult<String>;
    fn status(&self) -> PsResult<String>;
    fn cpu_times(&self) -> PsResult<ProcCpuTimes>;
    /// `memory_info().rss`.
    fn rss(&self) -> PsResult<u64>;
    fn io_counters(&self) -> PsResult<ProcIo>;
    fn num_threads(&self) -> PsResult<i64>;
    fn exe(&self) -> PsResult<String>;
    /// Only read for an explicit Properties request.
    fn cmdline(&self) -> PsResult<Vec<String>>;
    fn cwd(&self) -> PsResult<String>;
    fn ppid(&self) -> PsResult<i64>;
    fn nice(&self) -> PsResult<i64>;
    fn is_running(&self) -> bool;
    fn terminate(&self) -> PsResult<()>;
    fn kill(&self) -> PsResult<()>;
    fn suspend(&self) -> PsResult<()>;
    fn resume(&self) -> PsResult<()>;
    fn set_nice(&self, value: i32) -> PsResult<()>;
}

pub trait Source: Send {
    /// `os.cpu_count()` (logical) or physical cores.
    fn cpu_count(&self, logical: bool) -> Option<usize>;
    fn cpu_times_percpu(&self) -> PsResult<Vec<CpuTimes>>;
    fn cpu_freq(&self) -> PsResult<Option<CpuFreq>>;
    fn virtual_memory(&self) -> PsResult<VirtualMemory>;
    fn swap_memory(&self) -> PsResult<SwapMemory>;
    fn pids(&self) -> PsResult<Vec<i64>>;
    /// A fresh `psutil.Process(pid)`.
    fn process(&self, pid: i64) -> PsResult<Box<dyn Process>>;
    /// `(perdisk, aggregate)` of `disk_io_counters(…, nowrap=True)`.
    fn disk_io_counters(&self) -> PsResult<(Vec<(String, DiskCounters)>, Option<DiskCounters>)>;
    fn disk_partitions(&self) -> PsResult<Vec<Partition>>;
    fn disk_usage(&self, mountpoint: &str) -> PsResult<DiskUsage>;
    fn net_io_counters(&self) -> PsResult<Vec<(String, NetCounters)>>;
    fn net_if_stats(&self) -> PsResult<Vec<(String, NetStats)>>;
    fn net_if_addrs(&self) -> PsResult<Vec<(String, Vec<NetAddress>)>>;
    fn boot_time(&self) -> PsResult<f64>;
}
