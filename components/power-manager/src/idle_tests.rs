//! Port of test_power_idle.py: fake processes, no real locks or display changes.

use crate::config::defaults;
use crate::fakes::{FakeProcesses, FakeSystem};
use crate::idle::{dim_delay, IdleController};
use crate::util::{shell_split, FakeScheduler};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

const SELF: &str = "/opt/power manager/power-manager";

struct Fx {
    root: PathBuf,
    backend: Rc<FakeSystem>,
    processes: Rc<FakeProcesses>,
    scheduler: Rc<FakeScheduler>,
    errors: Rc<RefCell<Vec<String>>>,
    settings: Value,
    controller: IdleController,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.controller.close();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture(name: &str) -> Fx {
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("power idle '{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let lock = root.join("waybar/scripts/screenlock.sh");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend = FakeSystem::ready();
    backend.set(|s| { s.has_battery = true; s.on_battery = false; });
    let processes = Rc::new(FakeProcesses::default());
    let scheduler = Rc::new(FakeScheduler::default());
    let errors = Rc::new(RefCell::new(Vec::new()));
    let e = errors.clone();
    let controller = IdleController::new(backend.clone(), move |m| e.borrow_mut().push(m.into()), processes.clone(),
                                         scheduler.clone(), vec![SELF.into()]);
    controller.set_lock_script(lock);
    Fx { root, backend, processes, scheduler, errors, settings: defaults(), controller }
}

impl Fx {
    fn start(&self) -> Vec<String> {
        self.controller.start(&self.settings).unwrap();
        assert!(self.controller.ready(), "{}", self.controller.error());
        self.processes.last()
    }

    fn actions(&self) -> BTreeMap<String, i64> {
        let command = self.processes.last();
        command.iter().enumerate().filter(|(_, w)| *w == "timeout")
            .map(|(i, _)| (shell_split(&command[i + 2])[2].clone(), command[i + 1].parse().unwrap())).collect()
    }

    fn update(&self) {
        self.controller.update(&self.settings).unwrap();
    }
}

fn map(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn default_timers_use_tokens_and_direct_synchronous_restore_and_lock() {
    let f = fixture("default");
    let command = f.start();
    assert_eq!(command[..4], ["/usr/bin/swayidle", "-w", "-C", "/dev/null"]);
    assert_eq!(f.actions(), map(&[("dim", 270), ("lock", 300), ("display-off", 600)]));
    let token = f.controller.token();
    assert_eq!(token.len(), 32);
    assert!(f.controller.allows("lock", &token));
    assert!(f.controller.allows("display-off", &token));
    assert!(!f.controller.allows("suspend", &token));
    assert!(!f.controller.allows("lock", "stale"));
    for (i, word) in command.iter().enumerate() {
        if word == "timeout" {
            let arguments = shell_split(&command[i + 2]);
            assert_eq!(arguments[arguments.len() - 2..], ["--policy-token".to_owned(), token.clone()]);
            assert_eq!(arguments[0], SELF);
        }
    }
    let after = |key: &str| shell_split(&command[command.iter().position(|w| w == key).unwrap() + 1]);
    assert_eq!(after("after-resume"), ["/usr/bin/wlopm", "--on", "*"]);
    assert_eq!(after("before-sleep"), ["/usr/bin/systemctl", "--user", "start", "power-manager-lock.service"]);
    assert_eq!(after("lock"), ["/usr/bin/systemctl", "--user", "start", "power-manager-lock.service"]);
    assert!(!command.join(" ").contains("wlr-randr"));
    // Exactly the Python argv shape: args are shlex.join'ed absolute paths.
    assert_eq!(command[command.iter().position(|w| w == "after-resume").unwrap() + 1], "/usr/bin/wlopm --on '*'");
}

#[test]
fn every_timeout_cancels_pending_action_on_resume_without_waiting_for_primary() {
    let mut f = fixture("resume");
    f.settings["ac"]["sleep_after"] = 900.into();
    let command = f.start();
    let mut checked = Vec::new();
    for (i, word) in command.iter().enumerate() {
        if word != "timeout" {
            continue;
        }
        let timeout_args = shell_split(&command[i + 2]);
        let action = timeout_args[2].clone();
        assert_eq!(command[i + 3], "resume");
        let resume = &command[i + 4];
        let (forwarded, suffix) = resume.split_once(" </dev/null >/dev/null 2>&1 &").unwrap();
        let arguments = shell_split(forwarded);
        assert_eq!(arguments[..1], timeout_args[..1]);
        assert_eq!(arguments[1..], ["--idle-resume".to_owned(), action.clone(), "--policy-token".into(), f.controller.token()]);
        if action == "display-off" {
            assert_eq!(shell_split(suffix), ["/usr/bin/wlopm", "--on", "*"]);
        } else {
            assert_eq!(suffix, "");
        }
        checked.push(action);
    }
    assert_eq!(checked, ["dim", "lock", "display-off", "suspend"]);
}

#[test]
fn unchanged_refresh_and_inactive_policy_edits_do_not_reset_idle_clock() {
    let mut f = fixture("unchanged");
    f.start();
    let token = f.controller.token();
    f.controller.refresh();
    f.settings["battery"]["sleep_after"] = 300.into();
    f.update();
    assert_eq!(f.processes.commands.borrow().len(), 1);
    assert_eq!(f.controller.token(), token);
    assert!(!f.processes.terminated(f.processes.pids.borrow()[0]));
}

#[test]
fn source_switch_restarts_only_owned_child_and_rejects_old_callbacks_immediately() {
    let mut f = fixture("switch");
    f.settings["battery"] = serde_json::json!({"display_off": 90, "sleep_after": 300, "sleep_action": "hibernate"});
    f.start();
    let old = f.controller.token();
    f.backend.set(|s| s.on_battery = true);
    // Safety applies even before the application delivers its change event.
    assert!(!f.controller.allows("display-off", &old));
    f.controller.refresh();
    let pids = f.processes.pids.borrow().clone();
    assert_eq!(pids.len(), 2);
    assert!(f.processes.terminated(pids[0]));
    assert!(!f.processes.terminated(pids[1]));
    assert_eq!(f.actions(), map(&[("dim", 60), ("lock", 300), ("display-off", 90), ("hibernate", 300)]));
    assert!(!f.controller.allows("hibernate", &old));
    assert!(f.controller.allows("hibernate", &f.controller.token()));
    assert!(!f.controller.allows("suspend", &f.controller.token()));
}

#[test]
fn unknown_source_and_nonautomatic_capabilities_never_arm_sleep() {
    let mut f = fixture("caps");
    f.settings["ac"]["sleep_after"] = 600.into();
    f.start();
    assert_eq!(f.actions()["suspend"], 600);
    for capability in ["challenge", "no", "na", "", "False"] {
        let old = f.controller.token();
        f.backend.set(|s| { s.capabilities.insert("suspend".into(), capability.into()); });
        assert!(!f.controller.allows("suspend", &old));
        f.controller.refresh();
        assert!(!f.actions().contains_key("suspend"));
        assert!(!f.controller.allows("suspend", &f.controller.token()));
    }
    f.backend.set(|s| { s.capabilities.remove("suspend"); });
    f.controller.refresh();
    assert!(!f.actions().contains_key("suspend"));
    f.backend.set(|s| { s.capabilities.insert("suspend".into(), "yes".into()); s.power_source_known = false; });
    f.controller.refresh();
    assert!(!f.actions().contains_key("suspend"));
    f.backend.set(|s| s.power_source_known = true);
    f.controller.refresh();
    assert_eq!(f.actions()["suspend"], 600);
}

#[test]
fn disabling_timers_preserves_before_sleep_lock_and_restores_displays() {
    let mut f = fixture("disable");
    f.start();
    let old = f.controller.token();
    f.settings["lock_after"] = 0.into();
    f.settings["ac"]["display_off"] = 0.into();
    f.update();
    assert!(f.actions().is_empty());
    assert!(f.processes.last().contains(&"before-sleep".to_owned()));
    assert!(f.processes.last().contains(&"after-resume".to_owned()));
    assert!(!f.controller.allows("lock", &f.controller.token()));
    assert!(!f.controller.allows("display-off", &old));
}

#[test]
fn missing_dependencies_or_locker_surface_errors_without_launching() {
    let f = fixture("missing");
    f.processes.missing.set(true);
    f.controller.start(&f.settings).unwrap();
    assert!(!f.controller.ready());
    assert!(f.errors.borrow().last().unwrap().contains("requires swayidle and wlopm"));
    f.processes.missing.set(false);
    std::fs::remove_file(f.root.join("waybar/scripts/screenlock.sh")).unwrap();
    f.controller.start(&f.settings).unwrap();
    assert!(!f.controller.ready());
    assert!(f.errors.borrow().last().unwrap().contains("screen lock command"));
    assert!(f.processes.commands.borrow().is_empty());
}

#[test]
fn spawn_failure_and_child_exit_clear_readiness_and_revoke_token() {
    let f = fixture("spawnfail");
    f.processes.fail_spawn.replace(Some("cannot connect".into()));
    f.controller.start(&f.settings).unwrap();
    assert!(!f.controller.ready());
    assert_eq!(f.controller.token(), "");
    assert!(f.errors.borrow().last().unwrap().contains("cannot connect"));
    f.processes.fail_spawn.replace(None);
    f.start();
    let token = f.controller.token();
    let pid = *f.processes.pids.borrow().last().unwrap();
    f.processes.exit(pid, 1);
    assert!(!f.controller.allows("lock", &token));
    assert!(!f.controller.ready());
    assert_eq!(f.controller.token(), "");
    assert!(f.errors.borrow().last().unwrap().contains("exit status 1"));
    f.controller.refresh();
    assert_eq!(f.processes.commands.borrow().len(), 1, "A broken compositor must not cause a restart loop");
}

#[test]
fn close_is_idempotent_and_late_backend_updates_cannot_restart_child() {
    let f = fixture("close");
    f.start();
    let token = f.controller.token();
    f.controller.close();
    f.controller.close();
    f.update();
    assert!(f.processes.terminated(f.processes.pids.borrow()[0]));
    assert!(!f.controller.allows("lock", &token));
    assert_eq!(f.processes.commands.borrow().len(), 1);
}

#[test]
fn replacement_never_blocks_and_hung_child_is_killed_by_timer() {
    let mut f = fixture("hung");
    f.start();
    let child = f.processes.pids.borrow()[0];
    f.settings["ac"]["display_off"] = 120.into();
    f.update();
    assert!(f.processes.terminated(child));
    assert!(!f.processes.killed(child));
    assert_eq!(f.processes.commands.borrow().len(), 2);
    assert_eq!(f.scheduler.delays(), [1000]);
    let id = f.scheduler.ids()[0];
    f.scheduler.fire(id);
    assert!(f.processes.killed(child));
}

#[test]
fn child_watch_reaps_and_reports_exit_without_polling() {
    let f = fixture("watch");
    f.start();
    let child = f.processes.pids.borrow()[0];
    assert_eq!(f.processes.watched.borrow()[0].0, child);
    assert!(f.controller.allows("lock", &f.controller.token()));
    assert_eq!(crate::idle::exit_code(256), 1);
    f.controller.child_exited(child, crate::idle::exit_code(256));
    assert!(!f.controller.ready());
    assert!(f.errors.borrow().last().unwrap().contains("exit status 1"));
    // A replaced child's exit is only reaped, never reported.
    f.controller.child_exited(1, 0);
    assert_eq!(f.errors.borrow().len(), 1);
    // An exited child no longer needs a kill timer.
    f.controller.close();
    assert!(f.scheduler.delays().is_empty());
}

#[test]
fn dim_step_precedes_lock_or_display_off_and_can_be_disabled() {
    let mut f = fixture("dim");
    f.start();
    assert!(f.controller.allows("dim", &f.controller.token()));
    f.settings["lock_after"] = 0.into();
    f.update();
    assert_eq!(f.actions(), map(&[("dim", 570), ("display-off", 600)]));
    f.settings["dim_before_lock"] = false.into();
    f.update();
    assert_eq!(f.actions(), map(&[("display-off", 600)]));
    assert!(!f.controller.allows("dim", &f.controller.token()));
    f.settings["dim_before_lock"] = true.into();
    f.settings["lock_after"] = 40.into();
    f.update();
    assert_eq!(f.actions()["dim"], 20);
    assert_eq!(dim_delay(0, 0, true), 0);
    assert_eq!(dim_delay(61, 0, true), 31);
}

#[test]
fn resume_forwarding_quotes_arguments_closes_streams_and_does_not_wait() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("power resume '$ {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // Execute only a synthetic helper, never the installed manager.
    let helper = root.join("power-manager");
    std::fs::write(&helper, "#!/bin/sh\nroot=$(dirname \"$0\")\n\
        { for a in \"$@\"; do printf '%s\\n' \"$a\"; done; printf 'stdin:'; cat; } > \"$root/started.tmp\"\n\
        mv \"$root/started.tmp\" \"$root/started\"\n\
        echo 'output that must be discarded'\necho 'error that must be discarded' >&2\n\
        i=0; while [ ! -e \"$root/release\" ] && [ $i -lt 300 ]; do sleep 0.01; i=$((i+1)); done\n\
        touch \"$root/done\"\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let controller = IdleController::new(FakeSystem::ready(), |_| {}, Rc::new(FakeProcesses::default()),
                                         Rc::new(FakeScheduler::default()), vec![helper.to_string_lossy().into_owned()]);
    let token = "policy with spaces ' $(false)";
    controller.set_token(token);
    let command = controller.resume_command("display-off", None);
    let start = Instant::now();
    let output = std::process::Command::new("/bin/sh").arg("-c").arg(&command).stdin(std::process::Stdio::null()).output().unwrap();
    assert!(output.status.success());
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!((output.stdout.as_slice(), output.stderr.as_slice()), (&b""[..], &b""[..]));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !root.join("started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = std::fs::read_to_string(root.join("started")).unwrap();
    assert_eq!(started, format!("--idle-resume\ndisplay-off\n--policy-token\n{token}\nstdin:"));
    assert!(!root.join("done").exists(), "The shell must return while forwarding is pending");
    std::fs::write(root.join("release"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !root.join("done").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_dir_all(&root);
}
