//! test_monitor_backend.py: deterministic sampling with a fake psutil, and
//! process control limited to a child this test spawned (recorded by PID).
//! Plus parser checks of the real /proc reader against a fake /proc tree.

use crate::backend::*;
use crate::procfs::ProcFs;
use crate::source::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct FakeProcessState {
    pid: i64,
    created: f64,
    cpu: f64,
    read: u64,
    write: u64,
    rss: u64,
    denied: BTreeSet<&'static str>,
    vanished: Option<&'static str>,
    actions: Vec<(String, Option<i32>)>,
    private_reads: Vec<&'static str>,
    alive: bool,
    action_error: Option<PsError>,
}

impl FakeProcessState {
    fn new(pid: i64, created: f64) -> Self {
        Self { pid, created, cpu: 4.0, read: 100, write: 200, rss: 100, denied: BTreeSet::new(), vanished: None,
               actions: Vec::new(), private_reads: Vec::new(), alive: true, action_error: None }
    }
}

struct FakeState {
    processes: BTreeMap<i64, Arc<Mutex<FakeProcessState>>>,
    cpu_values: Vec<CpuTimes>,
    disks: Vec<(String, DiskCounters)>,
    aggregate: Option<DiskCounters>,
    network: Vec<(String, NetCounters)>,
    stats: Vec<(String, NetStats)>,
    partitions: Vec<Partition>,
    capacity_reads: Vec<String>,
    address_reads: usize,
    fresh_reads: Vec<i64>,
    deny_cpu_times: bool,
    deny_disk_io: bool,
}

#[derive(Clone)]
struct FakePsutil(Arc<Mutex<FakeState>>);

impl FakePsutil {
    fn new() -> Self {
        let core = CpuTimes { user: 100.0, idle: 100.0, guest: 50.0, ..Default::default() };
        Self(Arc::new(Mutex::new(FakeState {
            processes: BTreeMap::from([(500, Arc::new(Mutex::new(FakeProcessState::new(500, 10.0))))]),
            cpu_values: vec![core; 4],
            disks: vec![("sda".into(), DiskCounters { read_bytes: Some(1000), write_bytes: Some(500) })],
            aggregate: Some(DiskCounters { read_bytes: Some(1000), write_bytes: Some(500) }),
            network: vec![("eth0".into(), NetCounters { bytes_recv: Some(1000), bytes_sent: Some(2000) })],
            stats: vec![("eth0".into(), NetStats { isup: Some(true), speed: Some(1000), mtu: Some(1500) })],
            partitions: vec![Partition { device: "/dev/sda1".into(), mountpoint: "/".into(), fstype: "ext4".into() }],
            capacity_reads: Vec::new(), address_reads: 0, fresh_reads: Vec::new(), deny_cpu_times: false, deny_disk_io: false,
        })))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().unwrap()
    }

    fn process(&self, pid: i64) -> Arc<Mutex<FakeProcessState>> {
        self.state().processes[&pid].clone()
    }

    fn set_process(&self, state: FakeProcessState) {
        self.state().processes.insert(state.pid, Arc::new(Mutex::new(state)));
    }
}

struct FakeProcess(Arc<Mutex<FakeProcessState>>);

impl FakeProcess {
    fn read<T>(&self, key: &'static str, value: T) -> PsResult<T> {
        let state = self.0.lock().unwrap();
        if state.vanished == Some(key) {
            return Err(PsError::NoSuchProcess(state.pid));
        }
        if state.denied.contains(key) {
            return Err(PsError::AccessDenied(Some(state.pid)));
        }
        Ok(value)
    }

    fn action(&self, method: &str, value: Option<i32>) -> PsResult<()> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.action_error.clone() {
            return Err(error);
        }
        state.actions.push((method.into(), value));
        Ok(())
    }

    fn get(&self) -> FakeProcessState {
        self.0.lock().unwrap().clone()
    }
}

impl Process for FakeProcess {
    fn pid(&self) -> i64 { self.get().pid }
    fn create_time(&self) -> PsResult<f64> { let c = self.get().created; self.read("create_time", c) }
    fn name(&self) -> PsResult<String> { self.read("name", "test-app".into()) }
    fn username(&self) -> PsResult<String> { self.read("username", "test-user".into()) }
    fn status(&self) -> PsResult<String> { self.read("status", "running".into()) }
    fn cpu_times(&self) -> PsResult<ProcCpuTimes> { let c = self.get().cpu; self.read("cpu_times", ProcCpuTimes { user: c, system: 0.0 }) }
    fn rss(&self) -> PsResult<u64> { let r = self.get().rss; self.read("memory_info", r) }
    fn io_counters(&self) -> PsResult<ProcIo> {
        let s = self.get();
        self.read("io_counters", ProcIo { read_bytes: Some(s.read), write_bytes: Some(s.write) })
    }
    fn num_threads(&self) -> PsResult<i64> { self.read("num_threads", 2) }
    fn exe(&self) -> PsResult<String> { self.read("exe", "/usr/bin/test-app".into()) }
    fn cmdline(&self) -> PsResult<Vec<String>> {
        self.0.lock().unwrap().private_reads.push("cmdline");
        self.read("cmdline", vec!["test-app".into(), "--explicit-details".into()])
    }
    fn cwd(&self) -> PsResult<String> { self.read("cwd", "/tmp".into()) }
    fn ppid(&self) -> PsResult<i64> { Ok(2) }
    fn nice(&self) -> PsResult<i64> { Ok(0) }
    fn is_running(&self) -> bool { self.get().alive }
    fn terminate(&self) -> PsResult<()> { self.action("terminate", None) }
    fn kill(&self) -> PsResult<()> { self.action("kill", None) }
    fn suspend(&self) -> PsResult<()> { self.action("suspend", None) }
    fn resume(&self) -> PsResult<()> { self.action("resume", None) }
    fn set_nice(&self, value: i32) -> PsResult<()> { self.action("nice", Some(value)) }
}

impl Source for FakePsutil {
    fn cpu_count(&self, logical: bool) -> Option<usize> { Some(if logical { 4 } else { 2 }) }
    fn cpu_times_percpu(&self) -> PsResult<Vec<CpuTimes>> {
        let state = self.state();
        if state.deny_cpu_times { Err(PsError::AccessDenied(None)) } else { Ok(state.cpu_values.clone()) }
    }
    fn cpu_freq(&self) -> PsResult<Option<CpuFreq>> { Ok(Some(CpuFreq { current: Some(3000.0), min: Some(800.0), max: Some(4500.0) })) }
    fn virtual_memory(&self) -> PsResult<VirtualMemory> {
        Ok(VirtualMemory { total: 1000, used: 600, available: 400, free: 100, percent: 60.0, cached: 200, buffers: 100 })
    }
    fn swap_memory(&self) -> PsResult<SwapMemory> { Ok(SwapMemory { total: 100, used: 20, free: 80, percent: 20.0 }) }
    fn pids(&self) -> PsResult<Vec<i64>> { Ok(self.state().processes.keys().copied().collect()) }
    fn process(&self, pid: i64) -> PsResult<Box<dyn Process>> {
        let mut state = self.state();
        state.fresh_reads.push(pid);
        state.processes.get(&pid).map(|p| Box::new(FakeProcess(p.clone())) as Box<dyn Process>).ok_or(PsError::NoSuchProcess(pid))
    }
    fn disk_io_counters(&self) -> PsResult<(Vec<(String, DiskCounters)>, Option<DiskCounters>)> {
        let state = self.state();
        if state.deny_disk_io { Err(PsError::AccessDenied(None)) } else { Ok((state.disks.clone(), state.aggregate)) }
    }
    fn disk_partitions(&self) -> PsResult<Vec<Partition>> { Ok(self.state().partitions.clone()) }
    fn disk_usage(&self, mountpoint: &str) -> PsResult<DiskUsage> {
        self.state().capacity_reads.push(mountpoint.into());
        Ok(DiskUsage { total: 10000, used: 4000, free: 6000, percent: 40.0 })
    }
    fn net_io_counters(&self) -> PsResult<Vec<(String, NetCounters)>> { Ok(self.state().network.clone()) }
    fn net_if_stats(&self) -> PsResult<Vec<(String, NetStats)>> { Ok(self.state().stats.clone()) }
    fn net_if_addrs(&self) -> PsResult<Vec<(String, Vec<NetAddress>)>> {
        self.state().address_reads += 1;
        Ok(vec![("eth0".into(), vec![NetAddress { family: "2".into(), address: "192.0.2.1".into(),
                                                  netmask: Some("255.255.255.0".into()), broadcast: None }])])
    }
    fn boot_time(&self) -> PsResult<f64> { Ok(10.0) }
}

struct Fixture {
    ps: FakePsutil,
    clock: Arc<Mutex<f64>>,
}

impl Fixture {
    fn new() -> Self {
        Self { ps: FakePsutil::new(), clock: Arc::new(Mutex::new(100.0)) }
    }

    fn sampler_with(&self, gpu_runner: Option<GpuRunner>, gpu_command: Option<PathBuf>, drm_root: Option<PathBuf>,
                    include_addresses: bool) -> Sampler {
        let clock = self.clock.clone();
        Sampler::new(Options {
            source: Box::new(self.ps.clone()), clock: Box::new(move || *clock.lock().unwrap()), wall_clock: Box::new(|| 1010.0),
            gpu_runner: gpu_runner.unwrap_or_else(|| Box::new(run_command)), gpu_command, drm_root, include_addresses,
            system_info: Some(serde_json::Map::new()),
        })
    }

    fn sampler(&self) -> Sampler {
        self.sampler_with(None, None, None, false)
    }

    fn advance(&self, seconds: f64) {
        *self.clock.lock().unwrap() += seconds;
    }
}

fn serializable(value: &Value) {
    // json.dumps(result, allow_nan=False): serde_json cannot hold NaN; this checks the shape serializes.
    serde_json::to_string(value).unwrap();
}

#[test]
fn first_sample_has_zero_rates_and_no_private_arguments() {
    let f = Fixture::new();
    let result = f.sampler().sample();
    assert_eq!(result["interval"], json!(0.0));
    assert_eq!(result["cpu"]["percent"], json!(0.0));
    assert_eq!(result["cpu"]["per_core"], json!([0.0, 0.0, 0.0, 0.0]));
    assert_eq!(result["cpu"]["frequency"], json!({"current": 3000.0, "min": 800.0, "max": 4500.0}));
    assert_eq!(result["processes"][0]["cpu_percent"], json!(0.0));
    assert_eq!(result["processes"][0]["memory_percent"], json!(10.0));
    assert_eq!(result["disks"]["read_bytes_per_sec"], json!(0.0));
    assert_eq!(result["network"]["recv_bytes_per_sec"], json!(0.0));
    assert_eq!(result["system"]["uptime"], json!(1000.0));
    assert_eq!(result["gpus"], json!([]));
    assert!(f.ps.process(500).lock().unwrap().private_reads.is_empty());
    assert_eq!(f.ps.state().address_reads, 0);
    assert!(result["processes"][0].get("command_line").is_none());
    serializable(&result);
}

#[test]
fn cpu_normalization_guest_accounting_and_io_rates() {
    let f = Fixture::new();
    let mut sampler = f.sampler();
    sampler.sample();
    f.advance(2.0);
    {
        let process = f.ps.process(500);
        let mut p = process.lock().unwrap();
        p.cpu += 2.0; // One core fully occupied, 25% of a four-core machine.
        p.read += 400;
        p.write += 100;
    }
    {
        let mut s = f.ps.state();
        s.cpu_values[0].user += 10.0;
        s.cpu_values[0].guest += 5.0;
        s.cpu_values[0].idle += 10.0;
        for core in &mut s.cpu_values[1..] {
            core.idle += 20.0;
        }
        s.disks[0].1.read_bytes = Some(2000);
        s.aggregate.as_mut().unwrap().read_bytes = Some(2000);
        s.network[0].1.bytes_recv = Some(1600);
    }
    let result = sampler.sample();
    assert_eq!(result["cpu"]["percent"], json!(12.5));
    assert_eq!(result["cpu"]["per_core"], json!([50.0, 0.0, 0.0, 0.0]));
    assert_eq!(result["processes"][0]["cpu_percent"], json!(25.0));
    assert_eq!(result["processes"][0]["read_bytes_per_sec"], json!(200.0));
    assert_eq!(result["processes"][0]["write_bytes_per_sec"], json!(50.0));
    assert_eq!(result["disks"]["read_bytes_per_sec"], json!(500.0));
    assert_eq!(result["network"]["recv_bytes_per_sec"], json!(300.0));
    assert_eq!(f.ps.state().capacity_reads, vec!["/".to_owned()]);
}

#[test]
fn pid_reuse_death_and_new_processes_have_clean_baselines() {
    let f = Fixture::new();
    let mut sampler = f.sampler();
    sampler.sample();
    let mut replacement = FakeProcessState::new(500, 20.0);
    replacement.cpu = 1000.0;
    replacement.read = 999999;
    f.ps.state().processes.clear();
    f.ps.set_process(replacement);
    f.ps.set_process(FakeProcessState::new(501, 10.0));
    f.advance(2.0);
    let result = sampler.sample();
    let rows = result["processes"].as_array().unwrap();
    assert!(rows.iter().all(|r| r["cpu_percent"] == json!(0.0)));
    assert!(rows.iter().all(|r| r["read_bytes_per_sec"] == json!(0.0)));
    assert_eq!(rows[0]["create_time"], json!(20.0));
    f.ps.state().processes.clear();
    f.advance(2.0);
    assert_eq!(sampler.sample()["processes"], json!([]));
    assert_eq!(sampler.tracked_processes(), 0);
}

#[test]
fn vanishing_process_and_permission_denied_fields() {
    let f = Fixture::new();
    f.ps.process(500).lock().unwrap().vanished = Some("memory_info");
    let mut denied = FakeProcessState::new(501, 10.0);
    denied.denied = ["username", "cpu_times", "memory_info", "io_counters", "exe"].into_iter().collect();
    f.ps.set_process(denied);
    let result = f.sampler().sample();
    let rows = result["processes"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["pid"], json!(501));
    for key in ["user", "cpu_percent", "rss", "memory_percent", "read_bytes_per_sec", "executable"] {
        assert!(rows[0][key].is_null(), "{key}");
    }
}

#[test]
fn hotplug_counter_resets_and_nonpositive_elapsed() {
    let f = Fixture::new();
    let mut sampler = f.sampler();
    sampler.sample();
    f.advance(2.0);
    {
        let mut s = f.ps.state();
        s.disks = vec![("new".into(), DiskCounters { read_bytes: Some(9999999), write_bytes: Some(9999999) })];
        s.aggregate = Some(DiskCounters { read_bytes: Some(9999999), write_bytes: Some(9999999) });
        s.network[0].1.bytes_recv = Some(1);
        s.network.push(("new".into(), NetCounters { bytes_recv: Some(9999999), bytes_sent: Some(9999999) }));
    }
    f.ps.process(500).lock().unwrap().cpu = 1.0;
    let result = sampler.sample();
    assert_eq!(result["disks"]["read_bytes_per_sec"], json!(0.0));
    assert_eq!(result["disks"]["devices"][0]["read_bytes_per_sec"], json!(0.0));
    assert_eq!(result["network"]["recv_bytes_per_sec"], json!(0.0));
    assert_eq!(result["processes"][0]["cpu_percent"], json!(0.0));
    f.ps.process(500).lock().unwrap().cpu += 5.0;
    assert_eq!(sampler.sample()["processes"][0]["cpu_percent"], json!(0.0));
}

#[test]
fn network_addresses_are_only_explicitly_requested() {
    let f = Fixture::new();
    let result = f.sampler_with(None, None, None, true).sample();
    assert_eq!(f.ps.state().address_reads, 1);
    assert_eq!(result["network"]["interfaces"][0]["addresses"][0]["address"], json!("192.0.2.1"));
    serializable(&result);
}

#[test]
fn remote_mount_usage_is_not_queried() {
    let f = Fixture::new();
    f.ps.state().partitions.push(Partition { device: "server:/home".into(), mountpoint: "/network".into(), fstype: "nfs4".into() });
    let result = f.sampler().sample();
    assert_eq!(f.ps.state().capacity_reads, vec!["/".to_owned()]);
    assert!(result["disks"]["partitions"][1]["total"].is_null());
    assert!(result["disks"]["partitions"][1]["error"].as_str().unwrap().contains("Network filesystem"));
}

#[test]
fn no_disk_counters_is_unavailable_not_faked_zero() {
    let f = Fixture::new();
    f.ps.state().disks.clear();
    f.ps.state().aggregate = None;
    let result = f.sampler().sample();
    assert!(result["disks"]["read_bytes_per_sec"].is_null());
    assert_eq!(result["disks"]["devices"], json!([]));
}

#[test]
fn global_metric_failure_does_not_remove_other_metrics() {
    let f = Fixture::new();
    f.ps.state().deny_cpu_times = true;
    f.ps.state().deny_disk_io = true;
    let result = f.sampler().sample();
    assert!(result["cpu"]["percent"].is_null());
    assert!(result["disks"]["read_bytes_per_sec"].is_null());
    assert_eq!(result["processes"].as_array().unwrap().len(), 1);
    assert_eq!(result["warnings"].as_array().unwrap().len(), 2);
}

#[test]
fn process_that_exits_after_read_is_not_published() {
    let f = Fixture::new();
    f.ps.process(500).lock().unwrap().alive = false;
    assert_eq!(f.sampler().sample()["processes"], json!([]));
}

// ---------------------------------------------------------------- GPU tests

#[test]
fn nvidia_query_is_bounded_cached_and_preserves_unsupported_values() {
    let f = Fixture::new();
    let calls: Arc<Mutex<Vec<(Vec<String>, Duration)>>> = Arc::default();
    let record = calls.clone();
    let runner: GpuRunner = Box::new(move |argv, timeout| {
        record.lock().unwrap().push((argv.to_vec(), timeout));
        Ok(RunOutput { returncode: 0, stderr: String::new(),
                       stdout: "GPU-123, NVIDIA Test, 43, 8192, 1024, 65\nGPU-456, Second GPU, N/A, N/A, N/A, N/A\n".into() })
    });
    let mut sampler = f.sampler_with(Some(runner), Some("/test/nvidia-smi".into()), None, false);
    let result = sampler.sample();
    assert_eq!(calls.lock().unwrap()[0].1, GPU_TIMEOUT);
    assert_eq!(calls.lock().unwrap()[0].0, ["/test/nvidia-smi", "--query-gpu=uuid,name,utilization.gpu,memory.total,memory.used,temperature.gpu", "--format=csv,noheader,nounits"]);
    assert_eq!(result["gpus"][0]["memory_total"], json!(8192i64 * 1024 * 1024));
    assert_eq!(result["gpus"][0]["temp_c"], json!(65.0));
    assert!(result["gpus"][1]["utilization_percent"].is_null());
    assert!(result["gpus"][1]["memory_used"].is_null());
    f.advance(1.0);
    sampler.sample();
    assert_eq!(calls.lock().unwrap().len(), 1);
    f.advance(3.0);
    sampler.sample();
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[test]
fn nvidia_timeout_becomes_warning_without_aborting_sample() {
    let f = Fixture::new();
    let runner: GpuRunner = Box::new(|argv, timeout| Err(format!("Command '{argv:?}' timed out after {} seconds", timeout.as_secs_f64())));
    let result = f.sampler_with(Some(runner), Some("/test/nvidia-smi".into()), None, false).sample();
    assert_eq!(result["gpus"], json!([]));
    assert!(result["warnings"][0].as_str().unwrap().contains("NVIDIA"));
    assert_eq!(result["processes"].as_array().unwrap().len(), 1);
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("system-monitor-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn real_timeout_only_runs_our_private_helper() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let temp = scratch("gpu");
    let helper = temp.join("test-gpu-query");
    std::fs::write(&helper, "#!/bin/sh\nexec sleep 10\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started = Instant::now();
    let result = f.sampler_with(None, Some(helper), None, false).sample();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(result["gpus"], json!([]));
    assert!(result["warnings"].as_array().unwrap().iter().any(|m| m.as_str().unwrap().contains("timed out")));
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn drm_supported_values_and_connector_filtering() {
    let f = Fixture::new();
    let temp = scratch("drm");
    let device = temp.join("card1/device");
    std::fs::create_dir_all(device.join("hwmon/hwmon0")).unwrap();
    for (name, value) in [("vendor", "0x1002"), ("gpu_busy_percent", "25"), ("mem_info_vram_total", "8192"), ("mem_info_vram_used", "1024")] {
        std::fs::write(device.join(name), value).unwrap();
    }
    std::fs::write(device.join("hwmon/hwmon0/temp1_input"), "65000").unwrap();
    std::os::unix::fs::symlink(temp.join("card1"), temp.join("card1-DP-1")).unwrap();
    std::fs::create_dir_all(temp.join("card2/device")).unwrap();
    let result = f.sampler_with(None, None, Some(temp.clone()), false).sample();
    let gpus = result["gpus"].as_array().unwrap();
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0]["name"], json!("AMD GPU (card1)"));
    assert_eq!(gpus[0]["utilization_percent"], json!(25.0));
    assert_eq!(gpus[0]["temp_c"], json!(65.0));
    assert_eq!(gpus[0]["memory_used"], json!(1024));
    let _ = std::fs::remove_dir_all(&temp);
}

// --------------------------------------------------------- process controls

#[test]
fn pid_reuse_rejects_every_control_before_mutation() {
    let ps = FakePsutil::new();
    for result in [end_process(500, Some(9.0), false, &ps), suspend_process(500, Some(9.0), &ps), resume_process(500, Some(9.0), &ps),
                   set_process_priority(500, Some(9.0), 10, &ps)] {
        assert!(result.unwrap_err().0.contains("reused"));
    }
    assert!(ps.process(500).lock().unwrap().actions.is_empty());
}

#[test]
fn protected_invalid_and_missing_identity_targets() {
    let ps = FakePsutil::new();
    let own = std::process::id() as i64;
    for (pid, created) in [(0, Some(10.0)), (1, Some(10.0)), (-1, Some(10.0)), (own, Some(10.0)), (500, None), (500, Some(f64::NAN))] {
        assert!(end_process(pid, created, false, &ps).is_err());
    }
    assert!(ps.process(500).lock().unwrap().actions.is_empty());
    assert!(ps.state().fresh_reads.is_empty());
}

#[test]
fn only_explicit_actions_are_sent() {
    let ps = FakePsutil::new();
    assert!(end_process(500, Some(10.0), false, &ps).unwrap());
    assert!(end_process(500, Some(10.0), true, &ps).unwrap());
    suspend_process(500, Some(10.0), &ps).unwrap();
    resume_process(500, Some(10.0), &ps).unwrap();
    set_process_priority(500, Some(10.0), 5, &ps).unwrap();
    let actions = ps.process(500).lock().unwrap().actions.clone();
    assert_eq!(actions, vec![("terminate".into(), None), ("kill".into(), None), ("suspend".into(), None),
                             ("resume".into(), None), ("nice".into(), Some(5))]);
}

#[test]
fn permissions_and_exit_race_are_informative() {
    let ps = FakePsutil::new();
    ps.process(500).lock().unwrap().action_error = Some(PsError::AccessDenied(Some(500)));
    assert!(end_process(500, Some(10.0), false, &ps).unwrap_err().0.contains("permission"));
    ps.process(500).lock().unwrap().action_error = Some(PsError::NoSuchProcess(500));
    assert!(end_process(500, Some(10.0), false, &ps).unwrap_err().0.contains("already exited"));
    assert!(ps.process(500).lock().unwrap().actions.is_empty());
}

#[test]
fn priority_validation_is_before_mutation() {
    // Python also rejects True, "5" and 1.5; the i64 parameter makes those unrepresentable here.
    let ps = FakePsutil::new();
    for nice in [-21, 20] {
        assert!(set_process_priority(500, Some(10.0), nice, &ps).is_err());
    }
    assert!(ps.process(500).lock().unwrap().actions.is_empty());
}

#[test]
fn details_explicitly_reads_command_line_but_never_environment() {
    let ps = FakePsutil::new();
    let result = process_details(500, Some(10.0), &ps).unwrap();
    assert_eq!(result["command_line"], json!(["test-app", "--explicit-details"]));
    assert_eq!(ps.process(500).lock().unwrap().private_reads, vec!["cmdline"]);
    assert!(ps.process(500).lock().unwrap().actions.is_empty());
    let keys: Vec<&str> = result.keys().map(String::as_str).collect();
    assert_eq!(keys, ["pid", "create_time", "name", "user", "status", "executable", "command_line", "working_directory",
                      "parent_pid", "nice", "threads"]);
    ps.process(500).lock().unwrap().denied = ["cmdline", "exe"].into_iter().collect();
    assert!(process_details(500, Some(10.0), &ps).unwrap()["command_line"].is_null());
}

#[test]
fn details_rechecks_identity_after_reads() {
    let ps = FakePsutil::new();
    ps.process(500).lock().unwrap().alive = false;
    assert!(process_details(500, Some(10.0), &ps).unwrap_err().0.contains("already exited"));
}

/// Spawns its own `sleep 30` child, records its PID, and acts only on that PID.
#[test]
fn real_control_only_terminates_our_private_child() {
    let mut child = std::process::Command::new("sleep").arg("30").stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let pid = child.id() as i64;
    let live = ProcFs::new("/");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let created = live.process(pid).unwrap().create_time().unwrap();
        // A wrong identity never signals.
        assert!(end_process(pid, Some(created + 1.0), false, &live).unwrap_err().0.contains("reused"));
        assert!(child.try_wait().unwrap().is_none());
        suspend_process(pid, Some(created), &live).unwrap();
        let stopped = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            live.process(pid).unwrap().status().unwrap() == "stopped"
        });
        assert!(stopped);
        resume_process(pid, Some(created), &live).unwrap();
        set_process_priority(pid, Some(created), 5, &live).unwrap();
        assert_eq!(live.process(pid).unwrap().nice().unwrap(), 5);
        let details = process_details(pid, Some(created), &live).unwrap();
        assert_eq!(details["command_line"], json!(["sleep", "30"]));
        assert!(end_process(pid, Some(created), false, &live).unwrap());
        let status = child.wait().unwrap();
        assert!(status.code().is_none());
    }));
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

// ------------------------------------------------- real reader on a fake /proc

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn fake_proc() -> PathBuf {
    let root = scratch("procfs");
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    write(&root, "proc/stat", "cpu  10 0 10 80 0 0 0 0 0 0\ncpu0 100 5 50 800 20 1 2 3 40 0\ncpu1 100 0 50 800 0 0 0 0 0 0\nintr 1\nbtime 1000\n");
    write(&root, "proc/meminfo", "MemTotal:       1000 kB\nMemFree:         100 kB\nMemAvailable:    400 kB\nBuffers:          50 kB\nCached:          150 kB\nSReclaimable:     10 kB\nSwapTotal:       200 kB\nSwapFree:        150 kB\n");
    write(&root, "proc/42/stat", &format!("42 (my app) S 7 42 42 0 -1 0 0 0 0 0 {} {} 0 0 20 3 4 0 {} 0 0\n",
                                         (2.0 * ticks) as i64, ticks as i64, (5.0 * ticks) as i64));
    write(&root, "proc/42/statm", "100 10 5 1 0 50 0\n");
    write(&root, "proc/42/status", "Name:\tmy app\nUid:\t4242\t4242\t4242\t4242\n");
    write(&root, "proc/42/io", "rchar: 1\nwchar: 2\nread_bytes: 4096\nwrite_bytes: 8192\n");
    write(&root, "proc/42/cmdline", "/usr/bin/my app\0--flag\0");
    write(&root, "etc/passwd", "tester:x:4242:4242::/home/tester:/bin/sh\n");
    write(&root, "proc/diskstats", "   8       0 sda 10 0 100 0 20 0 200 0 0 0 0 0 0 0 0 0 0\n   8       1 sda1 5 0 50 0 10 0 100 0 0 0 0 0 0 0 0 0 0\n");
    std::fs::create_dir_all(root.join("sys/block/sda")).unwrap();
    write(&root, "proc/filesystems", "nodev\tsysfs\n\text4\nnodev\tzfs\n");
    write(&root, "proc/self/mounts", "/dev/sda1 /mnt/my\\040disk ext4 rw 0 0\nsysfs /sys sysfs rw 0 0\nnone /x ext4 rw 0 0\npool /tank zfs rw 0 0\n");
    write(&root, "proc/net/dev", "Inter-|   Receive\n face |bytes    packets\n    lo: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n  eth0: 300 3 0 0 0 0 0 0 400 4 0 0 0 0 0 0\n");
    write(&root, "sys/class/net/eth0/flags", "0x1003\n");
    write(&root, "sys/class/net/eth0/operstate", "up\n");
    write(&root, "sys/class/net/eth0/speed", "1000\n");
    write(&root, "sys/class/net/eth0/mtu", "1500\n");
    write(&root, "sys/class/net/lo/flags", "0x9\n");
    write(&root, "sys/class/net/lo/operstate", "unknown\n");
    write(&root, "sys/class/net/lo/speed", "-1\n");
    write(&root, "sys/class/net/lo/mtu", "65536\n");
    write(&root, "sys/devices/system/cpu/cpu0/topology/core_cpus_list", "0-1\n");
    write(&root, "sys/devices/system/cpu/cpu1/topology/core_cpus_list", "0-1\n");
    for (policy, current) in [("policy0", "1000000"), ("policy1", "3000000")] {
        write(&root, &format!("sys/devices/system/cpu/cpufreq/{policy}/scaling_cur_freq"), current);
        write(&root, &format!("sys/devices/system/cpu/cpufreq/{policy}/scaling_min_freq"), "800000");
        write(&root, &format!("sys/devices/system/cpu/cpufreq/{policy}/scaling_max_freq"), "4000000");
    }
    root
}

#[test]
fn proc_reader_parses_a_fake_proc_tree() {
    let root = fake_proc();
    let ps = ProcFs::new(&root);
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    assert_eq!(ps.cpu_count(true), Some(2));
    assert_eq!(ps.cpu_count(false), Some(1));
    let cpus = ps.cpu_times_percpu().unwrap();
    assert_eq!(cpus.len(), 2);
    assert_eq!(cpus[0].user, 100.0 / ticks);
    assert_eq!(cpus[0].guest, 40.0 / ticks);
    let frequency = ps.cpu_freq().unwrap().unwrap();
    assert_eq!((frequency.current, frequency.min, frequency.max), (Some(2000.0), Some(800.0), Some(4000.0)));
    let memory = ps.virtual_memory().unwrap();
    assert_eq!((memory.total, memory.available, memory.used, memory.free, memory.cached, memory.buffers),
               (1024000, 409600, 614400, 102400, 163840, 51200));
    assert_eq!(memory.percent, 60.0);
    let swap = ps.swap_memory().unwrap();
    assert_eq!((swap.total, swap.used, swap.percent), (204800, 51200, 25.0));
    assert_eq!(ps.boot_time().unwrap(), 1000.0);
    assert_eq!(ps.pids().unwrap(), vec![42]);
    let process = ps.process(42).unwrap();
    assert_eq!(process.create_time().unwrap(), 1005.0);
    assert_eq!(process.name().unwrap(), "my app");
    assert_eq!(process.username().unwrap(), "tester");
    assert_eq!(process.status().unwrap(), "sleeping");
    assert_eq!(process.cpu_times().unwrap(), ProcCpuTimes { user: 2.0, system: 1.0 });
    assert_eq!(process.rss().unwrap(), 10 * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64);
    assert_eq!(process.io_counters().unwrap(), ProcIo { read_bytes: Some(4096), write_bytes: Some(8192) });
    assert_eq!(process.num_threads().unwrap(), 4);
    assert_eq!(process.ppid().unwrap(), 7);
    assert_eq!(process.nice().unwrap(), 3);
    assert_eq!(process.cmdline().unwrap(), ["/usr/bin/my app", "--flag"]);
    assert_eq!(process.exe().unwrap(), ""); // No link in the fake tree: kernel-thread rule.
    assert!(process.is_running());
    // Signals never leave a fake root.
    assert!(process.terminate().is_err());
    assert_eq!(ps.process(43).err(), Some(PsError::NoSuchProcess(43)));
    let (disks, aggregate) = ps.disk_io_counters().unwrap();
    assert_eq!(disks.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["sda", "sda1"]);
    assert_eq!(aggregate, Some(DiskCounters { read_bytes: Some(51200), write_bytes: Some(102400) }));
    let partitions = ps.disk_partitions().unwrap();
    assert_eq!(partitions, vec![Partition { device: "/dev/sda1".into(), mountpoint: "/mnt/my disk".into(), fstype: "ext4".into() },
                                Partition { device: "pool".into(), mountpoint: "/tank".into(), fstype: "zfs".into() }]);
    let net = ps.net_io_counters().unwrap();
    assert_eq!(net, vec![("lo".into(), NetCounters { bytes_recv: Some(100), bytes_sent: Some(200) }),
                         ("eth0".into(), NetCounters { bytes_recv: Some(300), bytes_sent: Some(400) })]);
    let stats = ps.net_if_stats().unwrap();
    assert_eq!(stats, vec![("lo".into(), NetStats { isup: Some(true), speed: Some(0), mtu: Some(65536) }),
                           ("eth0".into(), NetStats { isup: Some(true), speed: Some(1000), mtu: Some(1500) })]);
    // A whole sample over the fake tree.
    let mut sampler = Sampler::new(Options { source: Box::new(ProcFs::new(&root)), clock: Box::new(|| 5.0), wall_clock: Box::new(|| 2000.0),
                                             gpu_runner: Box::new(run_command), gpu_command: None, drm_root: None,
                                             include_addresses: false, system_info: Some(serde_json::Map::new()) });
    let result = sampler.sample();
    assert_eq!(result["system"]["uptime"], json!(1000.0));
    assert_eq!(result["processes"][0]["user"], json!("tester"));
    assert_eq!(result["processes"][0]["executable"], json!(""));
    assert_eq!(result["memory"]["total"], json!(1024000));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rounding_and_rates_follow_python() {
    assert_eq!(percent(12.345), 12.35);
    assert_eq!(percent(150.0), 100.0);
    assert_eq!(percent(-3.0), 0.0);
    assert_eq!(rate(Some(10.0), Some(20.0), 1.0), 0.0);
    assert_eq!(rate(Some(10.0), None, 1.0), 0.0);
    assert_eq!(rate(Some(30.0), Some(10.0), 0.0), 0.0);
    assert_eq!(rate(Some(30.0), Some(10.0), 2.0), 10.0);
    assert_eq!(number_text(" 43 "), Some(43.0));
    assert_eq!(number_text("N/A"), None);
    assert_eq!(number_text("inf"), None);
    assert_eq!(csv_rows("a, b,\"c, d\"\n\n").unwrap(), vec![vec!["a".to_owned(), "b".into(), "c, d".into()]]);
}

/// Live read-only smoke check against the real /proc (never signals).
#[test]
fn live_sample_is_well_formed() {
    let mut sampler = Sampler::new(Options { gpu_command: None, ..Options::default() });
    let first = sampler.sample();
    assert!(first["processes"].as_array().unwrap().iter().any(|p| p["pid"] == json!(std::process::id())));
    assert!(first["memory"]["total"].as_u64().unwrap() > 0);
    let second = sampler.sample();
    assert!(second["interval"].as_f64().unwrap() >= 0.0);
}

/// Manual parity aid: `SYSTEM_MONITOR_DUMP=<file> cargo test -p tessel-system-monitor -- --ignored dump`
/// writes one live sample (read-only) for comparison with the Python Sampler.
#[test]
#[ignore]
fn dump_live_sample() {
    let Some(path) = std::env::var_os("SYSTEM_MONITOR_DUMP") else { return };
    let mut sampler = Sampler::new(Options::default());
    sampler.sample();
    std::thread::sleep(Duration::from_millis(500));
    std::fs::write(path, serde_json::to_string_pretty(&sampler.sample()).unwrap()).unwrap();
}
