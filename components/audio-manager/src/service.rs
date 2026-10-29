//! Restore managed PipeWire loopbacks and follow their selected output devices
//! (loopback_service.py `LoopbackService` + `main`).

use crate::backend::{AudioBackend, BackendOptions};
use crate::store::{strerror, with_suffix, Entries, LoopbackMonitor, LoopbackStore};
use crate::supervisor::{spawn_loopback, LoopbackSupervisor, Spawn};
use crate::timer::{self, Timeout};
use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::rc::{Rc, Weak};

pub struct Inner {
    pub store: LoopbackStore,
    pub supervisor: RefCell<LoopbackSupervisor>,
    pub backend: RefCell<Option<AudioBackend>>,
    pub entries: RefCell<Entries>,
    monitor: RefCell<Option<LoopbackMonitor>>,
    timer: RefCell<Option<Timeout>>,
    closed: Cell<bool>,
    last_error: RefCell<Option<String>>,
}

#[derive(Clone)]
pub struct LoopbackService(pub Rc<Inner>);

impl LoopbackService {
    /// Production service: `pw-loopback` from PATH (missing → error, as Python's RuntimeError).
    pub fn new(store: LoopbackStore) -> Result<Self, String> {
        let executable = glib::find_program_in_path("pw-loopback")
            .ok_or("pw-loopback is missing. Install the PipeWire command-line tools.")?
            .to_string_lossy()
            .into_owned();
        let mut options = BackendOptions::native(false);
        options.loopback_store = store.clone();
        Ok(Self::with(store, Box::new(move |entry| spawn_loopback(entry, &executable)), options))
    }

    pub fn with(store: LoopbackStore, spawn: Spawn, options: BackendOptions) -> Self {
        let service = Rc::new(Inner {
            store,
            supervisor: RefCell::new(LoopbackSupervisor::new(spawn, Box::new(timer::monotonic))),
            backend: RefCell::new(None),
            entries: RefCell::default(),
            monitor: RefCell::new(None),
            timer: RefCell::new(None),
            closed: Cell::new(false),
            last_error: RefCell::new(None),
        });
        let weak = Rc::downgrade(&service);
        let backend = AudioBackend::new(
            move || Self::upgrade(&weak, |s| s.changed()),
            |message| crate::log!("WARNING", "{}", message),
            options,
        );
        *service.backend.borrow_mut() = Some(backend);
        Self(service)
    }

    fn upgrade(weak: &Weak<Inner>, f: impl FnOnce(LoopbackService)) {
        if let Some(inner) = weak.upgrade() {
            f(LoopbackService(inner));
        }
    }

    pub fn backend(&self) -> AudioBackend {
        self.0.backend.borrow().clone().expect("backend")
    }

    pub fn changed(&self) {
        if self.0.closed.get() {
            return;
        }
        let backend = self.backend();
        let state = crate::api::AudioApi::state(&backend);
        self.0.supervisor.borrow_mut().reconcile(&self.0.entries.borrow(), &state.nodes, state.ready);
    }

    pub fn reload(&self) {
        match self.0.store.load() {
            Ok(entries) => {
                *self.0.entries.borrow_mut() = entries;
                *self.0.last_error.borrow_mut() = None;
            }
            Err(error) => {
                if self.0.last_error.borrow().as_deref() != Some(error.0.as_str()) {
                    crate::log!("ERROR", "{} Keeping the last valid loopback configuration.", error.0);
                    *self.0.last_error.borrow_mut() = Some(error.0);
                }
            }
        }
        self.changed();
    }

    pub fn start(&self) -> Result<(), String> {
        let weak = Rc::downgrade(&self.0);
        let monitor = LoopbackMonitor::new(&self.0.store, move || Self::upgrade(&weak, |s| s.reload()))?;
        *self.0.monitor.borrow_mut() = Some(monitor);
        self.reload();
        let weak = Rc::downgrade(&self.0);
        *self.0.timer.borrow_mut() = Some(timer::repeat(500, move || {
            Self::upgrade(&weak, |s| s.changed());
            weak.strong_count() > 0
        }));
        self.backend().start();
        Ok(())
    }

    pub fn close(&self) {
        if self.0.closed.replace(true) {
            return;
        }
        if let Some(timer) = self.0.timer.borrow_mut().take() {
            timer.cancel();
        }
        if let Some(monitor) = self.0.monitor.borrow_mut().take() {
            monitor.close();
        }
        self.backend().close();
        self.0.supervisor.borrow_mut().close();
    }
}

/// `loopback_service.main(argv)`; returns the exit status.
pub fn main(argv: Vec<String>) -> i32 {
    let prog = argv.first().map(|a| std::path::Path::new(a).file_name().map_or(a.clone(), |n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "loopback_service.py".into());
    let config = match crate::cli::service_arguments(&argv[1.min(argv.len())..], &prog) {
        Ok(config) => config,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    let store = LoopbackStore::new(config);
    if let Err(error) = store.ensure_directory() {
        crate::log!("ERROR", "{}", crate::supervisor::python_os_error(&error));
        return 1;
    }
    // The service and an accidental manual launch must not create duplicates.
    let lock_path = with_suffix(&store.path, ".service.lock");
    let lock = match std::fs::OpenOptions::new().create(true).read(true).append(true).mode(0o600).open(&lock_path) {
        Ok(lock) => lock,
        Err(error) => {
            crate::log!("ERROR", "Cannot open {}: {}", lock_path.display(), strerror(&error));
            return 1;
        }
    };
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        crate::log!("INFO", "The loopback service is already running for this configuration.");
        return 0;
    }
    let main_loop = glib::MainLoop::new(None, false);
    let service = match LoopbackService::new(store) {
        Ok(service) => service,
        Err(error) => {
            crate::log!("ERROR", "{}", error);
            return 1;
        }
    };
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let main_loop = main_loop.clone();
        timer::unix_signal(signal, move || {
            main_loop.quit();
            false
        });
    }
    let started = service.start();
    if let Err(error) = &started {
        crate::log!("ERROR", "Cannot monitor saved loopbacks: {}", error);
    } else {
        main_loop.run();
    }
    service.close();
    drop(lock);
    if started.is_ok() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::options;

    #[test]
    fn service_retains_last_valid_config_during_external_partial_write() {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("am-service-{}-{}", std::process::id(), crate::store::uuid4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = LoopbackStore::new(Some(dir.join("loopbacks.json")));
        store.add("Headset", "bluez_output.headset", "Headset").unwrap();
        let service = LoopbackService::with(store.clone(), Box::new(|_| Err(std::io::Error::other("no"))), options(store.clone()));
        service.reload();
        let saved = service.0.entries.borrow().clone();
        assert_eq!(saved.len(), 1);
        std::fs::write(&store.path, "{").unwrap();
        service.reload();
        assert_eq!(*service.0.entries.borrow(), saved);
        assert_eq!(service.0.last_error.borrow().as_deref(), Some("The saved loopback file is not valid JSON."));
        std::fs::remove_file(&store.path).unwrap();
        service.reload();
        assert!(service.0.entries.borrow().is_empty());
        service.close();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
