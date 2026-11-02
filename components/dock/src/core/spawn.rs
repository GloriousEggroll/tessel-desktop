//! Injectable process spawning (`Gio.SubprocessLauncher` /
//! `Gio.Subprocess.new` with `STDOUT_SILENCE | STDERR_PIPE`, then
//! `communicate_utf8_async`). Every launch in `core` goes through a
//! [`Spawner`] so tests never start real processes.
//!
//! API: [`Spawner::spawn`], [`GioSpawner`] (real), [`FakeSpawner`] (records
//! `calls`, completes them on demand via `finish`).

use std::cell::RefCell;
use std::rc::Rc;

/// Finished child: `Ok((successful, stderr))` or a communicate error.
pub type Finished = Result<(bool, String), glib::Error>;
pub type FinishFn = Box<dyn FnOnce(Finished)>;

pub trait Spawner {
    /// Start `argv`. `environment` replaces the child's environment
    /// (`set_environ`); `None` inherits ours. An `Err` is the spawn failure
    /// (Python's raised `GLib.Error`); otherwise `on_finish` runs later on the
    /// thread-default main context.
    fn spawn(&self, argv: &[String], environment: Option<&[(String, String)]>, on_finish: FinishFn) -> Result<(), glib::Error>;
}

/// Real spawner. The child's completion is never cancelled (its lifetime
/// belongs to the launched service, not to the dock).
#[derive(Default, Clone, Copy, Debug)]
pub struct GioSpawner;

impl Spawner for GioSpawner {
    fn spawn(&self, argv: &[String], environment: Option<&[(String, String)]>, on_finish: FinishFn) -> Result<(), glib::Error> {
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_PIPE);
        if let Some(environment) = environment {
            let items: Vec<std::ffi::OsString> = environment.iter().map(|(k, v)| format!("{k}={v}").into()).collect();
            launcher.set_environ(&items);
        }
        let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| std::ffi::OsStr::new(a.as_str())).collect();
        let process = launcher.spawn(&args)?;
        let child = process.clone();
        process.communicate_utf8_async(None, None::<&gio::Cancellable>, move |result| {
            on_finish(result.map(|(_stdout, stderr)| (child.is_successful(), stderr.map(|s| s.to_string()).unwrap_or_default())));
        });
        Ok(())
    }
}

/// One recorded [`FakeSpawner`] call.
pub struct SpawnCall {
    pub argv: Vec<String>,
    pub environment: Option<Vec<(String, String)>>,
    pub on_finish: Option<FinishFn>,
}

/// Test spawner: records calls; `fail_next` makes the next spawn fail.
#[derive(Default, Clone)]
pub struct FakeSpawner {
    pub calls: Rc<RefCell<Vec<SpawnCall>>>,
    pub fail_next: Rc<RefCell<Option<glib::Error>>>,
}

impl FakeSpawner {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn count(&self) -> usize {
        self.calls.borrow().len()
    }
    pub fn argv(&self, index: usize) -> Vec<String> {
        self.calls.borrow()[index].argv.clone()
    }
    pub fn last_argv(&self) -> Vec<String> {
        self.calls.borrow().last().map(|c| c.argv.clone()).unwrap_or_default()
    }
    pub fn last_environment(&self) -> Option<Vec<(String, String)>> {
        self.calls.borrow().last().and_then(|c| c.environment.clone())
    }
    /// Complete call `index` with `result`.
    pub fn finish(&self, index: usize, result: Finished) {
        let callback = self.calls.borrow_mut()[index].on_finish.take().expect("call already finished");
        callback(result);
    }
    pub fn finish_last(&self, result: Finished) {
        let index = self.count() - 1;
        self.finish(index, result);
    }
}

impl Spawner for FakeSpawner {
    fn spawn(&self, argv: &[String], environment: Option<&[(String, String)]>, on_finish: FinishFn) -> Result<(), glib::Error> {
        let failure = self.fail_next.borrow_mut().take();
        self.calls.borrow_mut().push(SpawnCall {
            argv: argv.to_vec(),
            environment: environment.map(<[_]>::to_vec),
            on_finish: if failure.is_some() { None } else { Some(on_finish) },
        });
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// `GLib.Error("text")` as PyGObject builds it in tests (domain `g-io-error-quark`, code 0 … here FAILED).
pub fn error(message: &str) -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::Failed, message)
}
