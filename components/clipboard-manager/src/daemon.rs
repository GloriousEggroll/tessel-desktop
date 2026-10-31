//! The data-control client: reads each new selection, and owns the kept copy
//! once the selection's owner is gone.

use crate::capture::{formats_to_read, Capture, Contents};
use crate::MARKER_MIME;
use clipboard_settings::{Settings, Store};
use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};
use wayland_client::backend::{ObjectId, WaylandError};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
    ext_data_control_offer_v1 as ext_offer, ext_data_control_source_v1 as ext_source,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer, zwlr_data_control_source_v1 as wlr_source,
};

/// A selection that has not delivered all of its formats by then is kept with what arrived.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
/// A paste whose reader stops reading is dropped after this.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const READ_CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clipboard = 0,
    Primary = 1,
}

const KINDS: [Kind; 2] = [Kind::Clipboard, Kind::Primary];

enum Manager {
    Ext(ext_manager::ExtDataControlManagerV1),
    Wlr(wlr_manager::ZwlrDataControlManagerV1),
}

enum Device {
    Ext(ext_device::ExtDataControlDeviceV1),
    Wlr(wlr_device::ZwlrDataControlDeviceV1),
}

enum Offer {
    Ext(ext_offer::ExtDataControlOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
}

enum Source {
    Ext(ext_source::ExtDataControlSourceV1),
    Wlr(wlr_source::ZwlrDataControlSourceV1),
}

impl Manager {
    fn name(&self) -> &'static str {
        match self {
            Manager::Ext(_) => "ext-data-control-v1",
            Manager::Wlr(_) => "wlr-data-control-unstable-v1",
        }
    }

    fn supports_primary(&self) -> bool {
        match self {
            Manager::Ext(_) => true,
            Manager::Wlr(m) => m.version() >= 2,
        }
    }

    fn device(&self, seat: &wl_seat::WlSeat, qh: &QueueHandle<State>) -> Device {
        match self {
            Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        }
    }

    fn source(&self, qh: &QueueHandle<State>) -> Source {
        match self {
            Manager::Ext(m) => Source::Ext(m.create_data_source(qh, ())),
            Manager::Wlr(m) => Source::Wlr(m.create_data_source(qh, ())),
        }
    }
}

impl Device {
    fn set(&self, kind: Kind, source: &Source) {
        match (self, source, kind) {
            (Device::Ext(d), Source::Ext(s), Kind::Clipboard) => d.set_selection(Some(s)),
            (Device::Ext(d), Source::Ext(s), Kind::Primary) => d.set_primary_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s), Kind::Clipboard) => d.set_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s), Kind::Primary) => d.set_primary_selection(Some(s)),
            _ => {}
        }
    }
}

impl Offer {
    fn id(&self) -> ObjectId {
        match self {
            Offer::Ext(o) => o.id(),
            Offer::Wlr(o) => o.id(),
        }
    }

    fn receive(&self, mime: String, fd: BorrowedFd) {
        match self {
            Offer::Ext(o) => o.receive(mime, fd),
            Offer::Wlr(o) => o.receive(mime, fd),
        }
    }

    fn destroy(&self) {
        match self {
            Offer::Ext(o) => o.destroy(),
            Offer::Wlr(o) => o.destroy(),
        }
    }
}

impl Source {
    fn id(&self) -> ObjectId {
        match self {
            Source::Ext(s) => s.id(),
            Source::Wlr(s) => s.id(),
        }
    }

    fn offer(&self, mime: String) {
        match self {
            Source::Ext(s) => s.offer(mime),
            Source::Wlr(s) => s.offer(mime),
        }
    }

    fn destroy(&self) {
        match self {
            Source::Ext(s) => s.destroy(),
            Source::Wlr(s) => s.destroy(),
        }
    }
}

struct PendingRead {
    file: File,
    kind: Kind,
    generation: u64,
    mime: String,
    data: Vec<u8>,
}

struct PendingWrite {
    file: File,
    data: Rc<Vec<u8>>,
    written: usize,
    started: Instant,
}

#[derive(Default)]
struct Slot {
    /// Bumped by every foreign selection: reads of older ones are dropped.
    generation: u64,
    capture: Option<(Capture, Instant)>,
    /// The owner went away before the capture finished.
    restore_when_done: bool,
    kept: Option<Rc<Contents>>,
    owned: Option<Source>,
}

pub struct State {
    store: Store,
    settings: Settings,
    stamp: Option<SystemTime>,
    verbose: bool,
    seat: Option<wl_seat::WlSeat>,
    ext_global: Option<(u32, u32)>,
    wlr_global: Option<(u32, u32)>,
    manager: Option<Manager>,
    device: Option<Device>,
    offers: HashMap<ObjectId, Vec<String>>,
    slots: [Slot; 2],
    sources: HashMap<ObjectId, Rc<Contents>>,
    reads: Vec<PendingRead>,
    writes: Vec<PendingWrite>,
    finished: bool,
}

fn pipe() -> std::io::Result<(File, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: fds is a valid two-element array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 returned two new descriptors that nothing else owns.
    unsafe { Ok((File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))) }
}

fn set_nonblocking(fd: &OwnedFd) {
    // SAFETY: fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

fn mtime(store: &Store) -> Option<SystemTime> {
    std::fs::metadata(&store.path).and_then(|m| m.modified()).ok()
}

impl State {
    pub fn new(store: Store, verbose: bool) -> Self {
        let settings = store.load();
        let stamp = mtime(&store);
        State {
            store, settings, stamp, verbose, seat: None, ext_global: None, wlr_global: None, manager: None,
            device: None, offers: HashMap::new(), slots: Default::default(), sources: HashMap::new(),
            reads: Vec::new(), writes: Vec::new(), finished: false,
        }
    }

    fn log(&self, message: impl FnOnce() -> String) {
        if self.verbose {
            eprintln!("wayclip: {}", message());
        }
    }

    pub fn protocol(&self) -> Option<&'static str> {
        self.manager.as_ref().map(Manager::name)
    }

    fn enabled(&self, kind: Kind) -> bool {
        match kind {
            Kind::Clipboard => self.settings.keep_clipboard,
            Kind::Primary => self.settings.keep_primary,
        }
    }

    /// Bind the manager (ext preferred) and create the seat's device.
    fn bind(&mut self, registry: &wl_registry::WlRegistry, qh: &QueueHandle<State>) -> Result<(), String> {
        let manager = if let Some((name, _)) = self.ext_global {
            Manager::Ext(registry.bind(name, 1, qh, ()))
        } else if let Some((name, version)) = self.wlr_global {
            Manager::Wlr(registry.bind(name, version.min(2), qh, ()))
        } else {
            return Err("the compositor offers no data-control protocol (ext-data-control-v1 or \
                        wlr-data-control-unstable-v1)".into());
        };
        let seat = self.seat.as_ref().ok_or("the compositor offers no seat")?;
        self.device = Some(manager.device(seat, qh));
        self.manager = Some(manager);
        Ok(())
    }

    fn selection(&mut self, kind: Kind, offer: Option<Offer>, qh: &QueueHandle<State>) {
        let Some(offer) = offer else {
            self.log(|| format!("{kind:?} cleared"));
            let slot = &mut self.slots[kind as usize];
            if slot.capture.is_some() {
                slot.restore_when_done = true;
            } else {
                self.restore(kind, qh);
            }
            return;
        };
        let offered = self.offers.remove(&offer.id()).unwrap_or_default();
        let Some(formats) = formats_to_read(&offered) else {
            offer.destroy();
            return;
        };
        let enabled = self.enabled(kind);
        let budget = self.settings.max_bytes();
        let slot = &mut self.slots[kind as usize];
        slot.generation += 1;
        slot.capture = None;
        slot.restore_when_done = false;
        slot.kept = None;
        let generation = slot.generation;
        self.reads.retain(|r| r.kind != kind);
        if !enabled || formats.is_empty() {
            offer.destroy();
            return;
        }
        let mut started = Vec::new();
        for mime in formats {
            let Ok((file, write_end)) = pipe() else { continue };
            // libwayland duplicates the descriptor when the request is marshalled.
            offer.receive(mime.clone(), write_end.as_fd());
            drop(write_end);
            self.reads.push(PendingRead { file, kind, generation, mime: mime.clone(), data: Vec::new() });
            started.push(mime);
        }
        offer.destroy();
        self.log(|| format!("{kind:?} reading {}", started.join(", ")));
        self.slots[kind as usize].capture = Some((Capture::new(started, budget), Instant::now()));
    }

    /// Offer the kept copy as wayclip's own selection.
    fn restore(&mut self, kind: Kind, qh: &QueueHandle<State>) {
        if !self.enabled(kind) {
            return;
        }
        let (Some(manager), Some(device)) = (&self.manager, &self.device) else { return };
        let slot = &mut self.slots[kind as usize];
        let Some(contents) = slot.kept.clone() else { return };
        if slot.owned.is_some() || (kind == Kind::Primary && !manager.supports_primary()) {
            return;
        }
        let source = manager.source(qh);
        for mime in contents.mimes() {
            source.offer(mime.to_owned());
        }
        source.offer(MARKER_MIME.to_owned());
        device.set(kind, &source);
        self.sources.insert(source.id(), contents);
        slot.owned = Some(source);
        self.log(|| format!("{kind:?} restored"));
    }

    fn send(&mut self, source: ObjectId, mime: &str, fd: OwnedFd) {
        let Some(data) = self.sources.get(&source).and_then(|c| c.get(mime)).cloned() else { return };
        set_nonblocking(&fd);
        self.writes.push(PendingWrite { file: File::from(fd), data, written: 0, started: Instant::now() });
    }

    fn cancelled(&mut self, id: ObjectId) {
        self.sources.remove(&id);
        for slot in &mut self.slots {
            if slot.owned.as_ref().is_some_and(|s| s.id() == id) {
                if let Some(source) = slot.owned.take() {
                    source.destroy();
                }
            }
        }
    }

    /// Finish captures whose formats all arrived (or that timed out).
    fn finish_captures(&mut self, qh: &QueueHandle<State>) {
        for kind in KINDS {
            let slot = &mut self.slots[kind as usize];
            let Some((capture, started)) = &slot.capture else { continue };
            let timed_out = started.elapsed() > CAPTURE_TIMEOUT;
            if capture.pending() > 0 && !timed_out {
                continue;
            }
            let generation = slot.generation;
            let (capture, _) = slot.capture.take().unwrap();
            if timed_out {
                self.reads.retain(|r| r.kind != kind || r.generation != generation);
            }
            let slot = &mut self.slots[kind as usize];
            slot.kept = capture.into_contents(self.settings.skip_secrets).map(Rc::new);
            let restore = std::mem::take(&mut slot.restore_when_done);
            let size: usize = slot.kept.iter().flat_map(|c| c.formats.iter()).map(|(_, d)| d.len()).sum();
            self.log(|| format!("{kind:?} kept {size} bytes"));
            if restore {
                self.restore(kind, qh);
            }
        }
    }

    /// Serve the descriptors poll() reported: `revents[i]` belongs to
    /// `reads[i]`, then the writes.
    fn service(&mut self, revents: &[libc::c_short]) {
        let (read_events, write_events) = revents.split_at(self.reads.len().min(revents.len()));
        let mut finished = Vec::new();
        for (index, events) in read_events.iter().enumerate() {
            if *events == 0 {
                continue;
            }
            let read = &mut self.reads[index];
            let budget_left = self.slots[read.kind as usize].capture.as_ref()
                .is_some_and(|(c, _)| c.fits(read.data.len()));
            let mut chunk = vec![0u8; READ_CHUNK];
            let outcome = loop {
                if !budget_left || !self.slots[read.kind as usize].capture.as_ref()
                    .is_some_and(|(c, _)| c.fits(read.data.len())) {
                    break Some(false);
                }
                match read.file.read(&mut chunk) {
                    Ok(0) => break Some(true),
                    Ok(n) => read.data.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break None,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break Some(false),
                }
            };
            if let Some(ok) = outcome {
                finished.push((index, ok));
            }
        }
        for (index, ok) in finished.into_iter().rev() {
            let read = self.reads.remove(index);
            let slot = &mut self.slots[read.kind as usize];
            if slot.generation != read.generation {
                continue;
            }
            if let Some((capture, _)) = &mut slot.capture {
                if ok {
                    capture.finish(&read.mime, read.data);
                } else {
                    capture.fail();
                }
            }
        }
        let mut done = Vec::new();
        for (index, events) in write_events.iter().enumerate() {
            let write = &mut self.writes[index];
            if *events != 0 {
                loop {
                    if write.written >= write.data.len() {
                        done.push(index);
                        break;
                    }
                    match write.file.write(&write.data[write.written..]) {
                        Ok(n) => write.written += n,
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => {
                            done.push(index);
                            break;
                        }
                    }
                }
            } else if write.started.elapsed() > WRITE_TIMEOUT {
                done.push(index);
            }
        }
        for index in done.into_iter().rev() {
            self.writes.remove(index);
        }
    }

    /// Re-read the settings file when it changed; forget copies of kinds turned off.
    fn reload_settings(&mut self) {
        let stamp = mtime(&self.store);
        if stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        self.settings = self.store.load();
        for kind in KINDS {
            if !self.enabled(kind) {
                let slot = &mut self.slots[kind as usize];
                slot.kept = None;
                slot.capture = None;
                slot.restore_when_done = false;
                self.reads.retain(|r| r.kind != kind);
            }
        }
        self.log(|| format!("settings {:?}", self.settings));
    }

    fn busy(&self) -> bool {
        !self.reads.is_empty() || !self.writes.is_empty() || self.slots.iter().any(|s| s.capture.is_some())
    }
}

fn io_error(error: WaylandError) -> String {
    format!("Wayland connection failed: {error}")
}

/// Connect, bind and report the protocol in use (`wayclip --check`).
pub fn check() -> Result<&'static str, String> {
    let (_conn, _queue, state) = connect(Store::new(None), false)?;
    Ok(state.protocol().unwrap_or("none"))
}

fn connect(store: Store, verbose: bool)
    -> Result<(Connection, wayland_client::EventQueue<State>, State), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("Cannot connect to the Wayland display: {e}"))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut state = State::new(store, verbose);
    let registry = conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    state.bind(&registry, &qh)?;
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    Ok((conn, queue, state))
}

/// Run until the connection fails or the compositor ends the device.
pub fn run(store: Store, verbose: bool) -> Result<(), String> {
    let (conn, mut queue, mut state) = connect(store, verbose)?;
    let qh = queue.handle();
    state.log(|| format!("using {}", state.protocol().unwrap_or("none")));
    loop {
        queue.dispatch_pending(&mut state).map_err(|e| e.to_string())?;
        state.finish_captures(&qh);
        if state.finished {
            return Err("the compositor ended the clipboard device".into());
        }
        match conn.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(io_error(e)),
        }
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = Vec::with_capacity(1 + state.reads.len() + state.writes.len());
        fds.push(libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 });
        fds.extend(state.reads.iter().map(|r| libc::pollfd { fd: r.file.as_raw_fd(), events: libc::POLLIN, revents: 0 }));
        fds.extend(state.writes.iter().map(|w| libc::pollfd { fd: w.file.as_raw_fd(), events: libc::POLLOUT, revents: 0 }));
        let timeout = if state.busy() { 250 } else { 2000 };
        // SAFETY: fds is a valid, initialised pollfd array of the given length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            drop(guard);
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll failed: {error}"));
        }
        if fds[0].revents != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(io_error(e)),
            }
        } else {
            drop(guard);
        }
        let revents: Vec<libc::c_short> = fds[1..].iter().map(|f| f.revents).collect();
        state.service(&revents);
        state.reload_settings();
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(),
             _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "ext_data_control_manager_v1" => state.ext_global = Some((name, version)),
                "zwlr_data_control_manager_v1" => state.wlr_global = Some((name, version)),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ext_manager::ExtDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ext_manager::ExtDataControlManagerV1, _: ext_manager::Event, _: &(), _: &Connection,
             _: &QueueHandle<Self>) {}
}

impl Dispatch<wlr_manager::ZwlrDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &wlr_manager::ZwlrDataControlManagerV1, _: wlr_manager::Event, _: &(), _: &Connection,
             _: &QueueHandle<Self>) {}
}

impl Dispatch<ext_device::ExtDataControlDeviceV1, ()> for State {
    fn event(state: &mut Self, _: &ext_device::ExtDataControlDeviceV1, event: ext_device::Event, _: &(),
             _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            ext_device::Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            ext_device::Event::Selection { id } => state.selection(Kind::Clipboard, id.map(Offer::Ext), qh),
            ext_device::Event::PrimarySelection { id } => state.selection(Kind::Primary, id.map(Offer::Ext), qh),
            ext_device::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(State, ext_device::ExtDataControlDeviceV1, [
        ext_device::EVT_DATA_OFFER_OPCODE => (ext_offer::ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<wlr_device::ZwlrDataControlDeviceV1, ()> for State {
    fn event(state: &mut Self, _: &wlr_device::ZwlrDataControlDeviceV1, event: wlr_device::Event, _: &(),
             _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wlr_device::Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            wlr_device::Event::Selection { id } => state.selection(Kind::Clipboard, id.map(Offer::Wlr), qh),
            wlr_device::Event::PrimarySelection { id } => state.selection(Kind::Primary, id.map(Offer::Wlr), qh),
            wlr_device::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(State, wlr_device::ZwlrDataControlDeviceV1, [
        wlr_device::EVT_DATA_OFFER_OPCODE => (wlr_offer::ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ext_offer::ExtDataControlOfferV1, ()> for State {
    fn event(state: &mut Self, offer: &ext_offer::ExtDataControlOfferV1, event: ext_offer::Event, _: &(),
             _: &Connection, _: &QueueHandle<Self>) {
        if let ext_offer::Event::Offer { mime_type } = event {
            state.offers.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<wlr_offer::ZwlrDataControlOfferV1, ()> for State {
    fn event(state: &mut Self, offer: &wlr_offer::ZwlrDataControlOfferV1, event: wlr_offer::Event, _: &(),
             _: &Connection, _: &QueueHandle<Self>) {
        if let wlr_offer::Event::Offer { mime_type } = event {
            state.offers.entry(offer.id()).or_default().push(mime_type);
        }
    }
}

impl Dispatch<ext_source::ExtDataControlSourceV1, ()> for State {
    fn event(state: &mut Self, source: &ext_source::ExtDataControlSourceV1, event: ext_source::Event, _: &(),
             _: &Connection, _: &QueueHandle<Self>) {
        match event {
            ext_source::Event::Send { mime_type, fd } => state.send(source.id(), &mime_type, fd),
            ext_source::Event::Cancelled => state.cancelled(source.id()),
            _ => {}
        }
    }
}

impl Dispatch<wlr_source::ZwlrDataControlSourceV1, ()> for State {
    fn event(state: &mut Self, source: &wlr_source::ZwlrDataControlSourceV1, event: wlr_source::Event, _: &(),
             _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wlr_source::Event::Send { mime_type, fd } => state.send(source.id(), &mime_type, fd),
            wlr_source::Event::Cancelled => state.cancelled(source.id()),
            _ => {}
        }
    }
}
