//! Port of test_brightness_idle.py: no monitor, brightness, lock, or sleep actions.

use crate::backend_tests::temp_root;
use tessel_core::scheduler::FakeScheduler;
use tessel_core::shell::shell_split;
use crate::idle::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

#[derive(Default)]
pub struct ChildState {
    pub status: Cell<Option<i32>>,
    pub terminated: Cell<bool>,
    pub killed: Cell<bool>,
    pub hang: Cell<bool>,
    pub refuse_terminate: Cell<bool>,
}

struct FakeChild(Rc<ChildState>);

impl Child for FakeChild {
    fn poll(&mut self) -> Option<i32> {
        self.0.status.get()
    }
    fn terminate(&mut self) -> Result<(), String> {
        if self.0.refuse_terminate.get() {
            return Err("cannot terminate".into());
        }
        self.0.terminated.set(true);
        if !self.0.hang.get() {
            self.0.status.set(Some(0));
        }
        Ok(())
    }
    fn kill(&mut self) -> Result<(), String> {
        self.0.killed.set(true);
        self.0.status.set(Some(-9));
        Ok(())
    }
    fn wait(&mut self, _timeout: Duration) -> Result<Option<i32>, String> {
        Ok(self.0.status.get())
    }
}

#[derive(Default)]
pub struct FakeSpawner {
    pub missing: Cell<bool>,
    pub fail: RefCell<Option<String>>,
    pub children: RefCell<Vec<Rc<ChildState>>>,
    pub commands: RefCell<Vec<Vec<String>>>,
}

impl Spawner for FakeSpawner {
    fn which(&self, _name: &str) -> Option<PathBuf> {
        (!self.missing.get()).then(|| PathBuf::from("/usr/bin/swayidle"))
    }
    fn spawn(&self, argv: &[String]) -> Result<Box<dyn Child>, String> {
        if let Some(error) = self.fail.borrow().clone() {
            return Err(error);
        }
        let state = Rc::new(ChildState::default());
        self.children.borrow_mut().push(state.clone());
        self.commands.borrow_mut().push(argv.to_vec());
        Ok(Box::new(FakeChild(state)))
    }
}

struct Fx {
    spawner: Rc<FakeSpawner>,
    errors: Rc<RefCell<Vec<String>>>,
    controller: IdleController,
}

impl Fx {
    fn new() -> Self {
        let spawner = Rc::new(FakeSpawner::default());
        let errors: Rc<RefCell<Vec<String>>> = Rc::default();
        let e = errors.clone();
        let controller = IdleController::new(move |m| e.borrow_mut().push(m.into()), spawner.clone(), Rc::new(FakeScheduler::default()),
                                              vec!["/opt/tessel/tessel-brightness".into()]);
        controller.set_watching(false);
        Self { spawner, errors, controller }
    }

    fn child(&self, index: usize) -> Rc<ChildState> {
        self.spawner.children.borrow()[index].clone()
    }

    fn count(&self) -> usize {
        self.spawner.children.borrow().len()
    }
}

#[test]
fn defaults_are_off_and_do_not_require_swayidle() {
    let fx = Fx::new();
    fx.spawner.missing.set(true);
    fx.controller.start(false, DEFAULT_DELAY).unwrap();
    assert!(fx.controller.ready());
    assert_eq!(fx.controller.token(), "");
    assert_eq!(fx.count(), 0);
    assert!(fx.errors.borrow().is_empty());
    assert!(!fx.controller.allows(""));
}

#[test]
fn listener_only_dims_and_restores_without_touching_sleep_or_lock_policy() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    let command = fx.spawner.commands.borrow().last().unwrap().clone();
    assert_eq!(command[..6], ["/usr/bin/swayidle", "-w", "-C", "/dev/null", "timeout", "120"]);
    assert_eq!(command.iter().filter(|w| *w == "timeout").count(), 1);
    let dim = shell_split(&command[6]);
    assert!(dim[0].ends_with("/tessel-brightness"));
    assert_eq!(dim[1..], ["--idle-dim".to_owned(), fx.controller.token()]);
    for event in ["resume", "before-sleep", "after-resume"] {
        let restore = &command[command.iter().position(|w| w == event).unwrap() + 1];
        assert!(restore.ends_with(" </dev/null >/dev/null 2>&1 &"));
        let args = shell_split(restore.strip_suffix(" </dev/null >/dev/null 2>&1 &").unwrap());
        assert_eq!(args[..1], dim[..1]);
        assert_eq!(args[1..], ["--idle-restore".to_owned(), fx.controller.token()]);
    }
    for forbidden in ["tessel-power", "wlopm", "wlr-randr", "screenlock", "suspend", "hibernate"] {
        assert!(!command.join(" ").contains(forbidden));
    }
}

#[test]
fn unchanged_policy_preserves_idle_clock_and_only_owns_its_listener() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    let token = fx.controller.token();
    fx.controller.update(true, 120).unwrap();
    fx.controller.start(true, 120).unwrap();
    assert_eq!(fx.count(), 1);
    assert!(!fx.child(0).terminated.get());
    assert_eq!(fx.controller.token(), token);
    fx.controller.update(true, 180).unwrap();
    assert!(fx.child(0).terminated.get());
    assert!(!fx.child(1).terminated.get());
    assert!(!fx.controller.allows(&token));
    assert!(fx.controller.allows(&fx.controller.token()));
}

#[test]
fn disabled_and_closed_policies_revoke_queued_events() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    let token = fx.controller.token();
    fx.controller.update(false, 120).unwrap();
    assert!(fx.controller.ready());
    assert!(!fx.controller.allows(&token));
    assert!(fx.child(0).terminated.get());
    fx.controller.close();
    fx.controller.update(true, 120).unwrap();
    assert_eq!(fx.count(), 1);
    assert!(!fx.controller.ready());
    fx.controller.start(true, 120).unwrap();
    assert_eq!(fx.count(), 2);
}

#[test]
fn update_before_start_and_invalid_preferences_never_launch_a_listener() {
    let fx = Fx::new();
    fx.controller.update(true, 120).unwrap();
    assert_eq!(fx.count(), 0);
    for (enabled, delay) in [(true, 0), (true, -1), (false, 86401)] {
        assert!(fx.controller.start(enabled, delay).is_err());
    }
    assert_eq!(fx.count(), 0);
}

#[test]
fn child_failure_revokes_token_and_reports_restore_needed_without_restart_loop() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    let token = fx.controller.token();
    fx.child(0).status.set(Some(1));
    assert!(!fx.controller.allows(&token));
    assert_eq!(fx.controller.token(), "");
    assert!(!fx.controller.ready());
    assert!(fx.errors.borrow().last().unwrap().contains("exit status 1"));
    fx.controller.update(true, 120).unwrap();
    assert_eq!(fx.count(), 1);
}

#[test]
fn child_watch_polls_every_second_and_reports_unexpected_exit() {
    let spawner = Rc::new(FakeSpawner::default());
    let scheduler = Rc::new(FakeScheduler::default());
    let errors: Rc<RefCell<Vec<String>>> = Rc::default();
    let e = errors.clone();
    let controller = IdleController::new(move |m| e.borrow_mut().push(m.into()), spawner.clone(), scheduler.clone(), vec!["x".into()]);
    controller.start(true, 60).unwrap();
    assert_eq!(scheduler.delays(), vec![1000]);
    scheduler.fire(scheduler.ids()[0]);
    assert_eq!(scheduler.delays(), vec![1000], "still running: keep polling");
    spawner.children.borrow()[0].status.set(Some(2));
    scheduler.fire(scheduler.ids()[0]);
    assert!(scheduler.pending.borrow().is_empty());
    assert_eq!(*errors.borrow(), vec!["Automatic dimming stopped unexpectedly (exit status 2).".to_owned()]);
    assert!(!controller.has_watch());
}

#[test]
fn dependency_and_spawn_errors_are_visible_and_can_recover_by_toggling() {
    let fx = Fx::new();
    fx.spawner.missing.set(true);
    fx.controller.start(true, 120).unwrap();
    assert!(!fx.controller.ready());
    assert!(fx.errors.borrow().last().unwrap().contains("requires swayidle"));
    fx.spawner.missing.set(false);
    fx.controller.update(false, 120).unwrap();
    fx.spawner.fail.replace(Some("no compositor".into()));
    fx.controller.update(true, 120).unwrap();
    assert!(!fx.controller.ready());
    assert!(fx.errors.borrow().last().unwrap().contains("no compositor"));
    fx.spawner.fail.replace(None);
    fx.controller.update(false, 120).unwrap();
    fx.controller.update(true, 120).unwrap();
    assert!(fx.controller.ready());
}

#[test]
fn invalid_token_types_and_unicode_are_safely_rejected() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    for token in ["", &"☀".repeat(32), "stale"] {
        assert!(!fx.controller.allows(token));
    }
    assert!(fx.controller.allows(&fx.controller.token()));
}

#[test]
fn hung_owned_listener_is_killed_before_replacement() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    fx.child(0).hang.set(true);
    fx.controller.update(true, 180).unwrap();
    assert!(fx.child(0).killed.get());
    assert_eq!(fx.count(), 2);
}

#[test]
fn cannot_launch_duplicate_if_owned_listener_cannot_be_stopped() {
    let fx = Fx::new();
    fx.controller.start(true, 120).unwrap();
    fx.child(0).refuse_terminate.set(true);
    fx.controller.update(true, 180).unwrap();
    assert_eq!(fx.count(), 1);
    assert!(!fx.controller.ready());
    assert!(fx.errors.borrow().last().unwrap().contains("Could not stop"));
    fx.child(0).refuse_terminate.set(false);
}

#[test]
fn restore_callback_quotes_arguments_closes_streams_and_returns_while_pending() {
    let root = temp_root("brightness restore '$ ");
    // Execute only a synthetic helper, never the installed manager.
    let script = root.join("tessel-brightness");
    std::fs::write(&script, "#!/bin/sh\nroot=$(dirname \"$0\")\n\
        { for a in \"$@\"; do printf '%s\\n' \"$a\"; done; printf 'stdin:'; cat; } > \"$root/started.tmp\"\n\
        mv \"$root/started.tmp\" \"$root/started\"\n\
        echo 'discarded output'\necho 'discarded error' >&2\n\
        i=0; while [ ! -e \"$root/release\" ] && [ $i -lt 300 ]; do sleep 0.01; i=$((i+1)); done\n\
        touch \"$root/done\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let controller = IdleController::new(|_| {}, Rc::new(FakeSpawner::default()), Rc::new(FakeScheduler::default()),
                                         vec![script.to_string_lossy().into_owned()]);
    controller.set_token("policy with spaces ' $(false)");
    let command = controller.command("--idle-restore", true);
    let output = std::process::Command::new("/bin/sh").args(["-c", &command]).stdin(std::process::Stdio::null()).output().unwrap();
    assert!(output.status.success());
    assert_eq!((output.stdout.as_slice(), output.stderr.as_slice()), (&b""[..], &b""[..]));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !root.join("started").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = std::fs::read_to_string(root.join("started")).unwrap();
    assert_eq!(started, "--idle-restore\npolicy with spaces ' $(false)\nstdin:");
    assert!(!root.join("done").exists());
    std::fs::write(root.join("release"), "").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !root.join("done").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_dir_all(&root);
}
