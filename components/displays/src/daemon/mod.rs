//! `tessel-displays --daemon`: keeps every monitor as Tessel's display settings
//! say, through the compositor's wlr-output-management protocol. It applies the
//! layout at login, when a monitor is plugged in or removed, when the lid moves
//! and when cfg.yaml or a client asks for a change.
//!
//! Clients (the Displays window, Brightness, Global Settings) talk to it over
//! `$XDG_RUNTIME_DIR/tessel-displays.sock` in the protocol way-displays used:
//! `OP: <OP>\n<yaml>`, answered with YAML, then the socket closes.

pub mod config;
pub mod layout;

use config::{Cfg, Level, Partial};
use glib::ControlFlow;
use tessel_core::tr;
use layout::{Head, Lid, Mode, Plan};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1 as ConfigHead,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1 as Configuration},
    zwlr_output_head_v1::{self, AdaptiveSyncState, ZwlrOutputHeadV1 as HeadProxy},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1 as Manager},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1 as ModeProxy},
};

const MANAGER_VERSION: u32 = 4;
const MAX_CANCELLED_RETRIES: u32 = 5;
const LID_POLL_SECONDS: u32 = 2;
const RC_WARN: i64 = 1;
const RC_ERROR: i64 = 2;
const RC_BAD_REQUEST: i64 = 3;
const RC_IN_PROGRESS: i64 = 4;

pub fn socket_path() -> PathBuf {
    tessel_core::paths::runtime_dir().join("tessel-displays.sock")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase { Idle, Outstanding, Succeeded, Failed, Cancelled }

/// What the change in flight was, for telling how to handle its outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delta { None, Mode(u32, u32), Vrr(u32) }

/// A client waiting for a change to finish.
struct Operation {
    client: UnixStream,
    command: String,
    rc: i64,
    messages: Vec<Value>,
}

struct Daemon {
    cfg: Cfg,
    /// The file's text as last read or written, so our own write is not "a change".
    file_text: String,
    heads: Vec<Head>,
    head_proxies: HashMap<u32, HeadProxy>,
    mode_proxies: HashMap<u32, ModeProxy>,
    mode_owner: HashMap<u32, u32>,
    manager: Option<Manager>,
    serial: Option<u32>,
    phase: Phase,
    delta: Delta,
    summary: String,
    retries: u32,
    /// A change the compositor refused: not tried again until something changes.
    blocked: bool,
    lid: Lid,
    operation: Option<Operation>,
}

impl Daemon {
    fn new(cfg: Cfg, file_text: String) -> Self {
        Self {
            cfg, file_text, heads: Vec::new(), head_proxies: HashMap::new(), mode_proxies: HashMap::new(), mode_owner: HashMap::new(),
            manager: None, serial: None, phase: Phase::Idle, delta: Delta::None, summary: String::new(), retries: 0, blocked: false,
            lid: None, operation: None,
        }
    }

    fn log(&mut self, level: Level, message: &str) {
        if level >= self.cfg.threshold {
            eprintln!("tessel-displays: {}: {message}", level.name());
        }
        if let Some(operation) = &mut self.operation {
            if level >= Level::Warning {
                let mut item = Map::new();
                item.insert(level.name().into(), message.into());
                operation.messages.push(Value::Object(item));
                operation.rc = operation.rc.max(if level == Level::Warning { RC_WARN } else { RC_ERROR });
            }
        }
    }

    fn callback(&self, level: Level, message: &str) {
        if self.cfg.callback.is_empty() || level < self.cfg.threshold {
            return;
        }
        let mut command = std::process::Command::new("/bin/sh");
        let title = match level {
            Level::Warning => tr!("Display warning"),
            Level::Error => tr!("Display error"),
            _ => tr!("Display settings"),
        };
        command.args(["-c", &self.cfg.callback]).env("CALLBACK_MSG", message).env("CALLBACK_LEVEL", level.name())
            .env("CALLBACK_TITLE", title)
            .stdin(std::process::Stdio::null());
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                libc::signal(libc::SIGCHLD, libc::SIG_DFL);
                Ok(())
            });
        }
        if let Err(error) = command.spawn() {
            eprintln!("tessel-displays: ERROR: could not run CALLBACK_CMD: {error}");
        }
    }

    fn head_mut(&mut self, id: u32) -> Option<&mut Head> {
        self.heads.iter_mut().find(|head| head.id == id)
    }

    /// One step of the state machine, after every wake-up.
    fn act(&mut self, qh: &QueueHandle<Self>) {
        match self.phase {
            Phase::Outstanding => return,
            Phase::Succeeded => {
                self.phase = Phase::Idle;
                self.handle_success();
            }
            Phase::Failed => {
                self.phase = Phase::Idle;
                self.handle_failure();
            }
            Phase::Cancelled => {
                self.phase = Phase::Idle;
                self.retries += 1;
                if self.retries <= MAX_CANCELLED_RETRIES {
                    let message = tr!("Display changes were cancelled. Retrying (attempt {attempt}).", attempt = self.retries.to_string());
                    self.log(Level::Warning, &message);
                } else {
                    let message = tr!("Display changes were cancelled after {retries} retries.", retries = MAX_CANCELLED_RETRIES.to_string());
                    self.log(Level::Warning, &message);
                    self.callback(Level::Warning, &message);
                    self.blocked = true;
                }
            }
            Phase::Idle => {}
        }
        if self.serial.is_none() {
            return;
        }
        layout::desire(&self.cfg, &mut self.heads, self.lid);
        for head in &mut self.heads {
            // Without the protocol's adaptive sync there is nothing to ask for.
            if self.head_proxies.get(&head.id).is_some_and(|proxy| proxy.version() < 4) {
                head.vrr_failed = true;
                head.des.vrr = head.cur.vrr;
            }
        }
        if !self.blocked {
            self.apply(qh);
        }
    }

    fn handle_success(&mut self) {
        self.retries = 0;
        match self.delta {
            // A successful mode change is not always reported.
            Delta::Mode(head, mode) => {
                if let Some(head) = self.head_mut(head) {
                    head.cur.mode = Some(mode);
                }
            }
            // Some compositors report refused adaptive sync as success.
            Delta::Vrr(id) => {
                if self.heads.iter().find(|head| head.id == id).is_some_and(|head| head.des.vrr != head.cur.vrr) {
                    return self.handle_failure();
                }
            }
            Delta::None => {}
        }
        let summary = std::mem::take(&mut self.summary);
        self.log(Level::Info, &format!("Changes successful: {summary}"));
        let message = if summary.is_empty() { tr!("Display changes applied.") } else { summary };
        self.callback(Level::Info, &message);
        self.delta = Delta::None;
    }

    fn handle_failure(&mut self) {
        let delta = std::mem::replace(&mut self.delta, Delta::None);
        match delta {
            Delta::Mode(id, mode_id) => {
                let Some(head) = self.head_mut(id) else { return };
                let name = head.name.clone();
                if let Some(index) = head.modes.iter().position(|mode| mode.id == mode_id) {
                    let mode = head.modes.remove(index);
                    head.modes_failed.push(mode);
                    // The current mode may be misreported after a failure.
                    head.cur.mode = None;
                    let message = tr!("{name}: could not set {width}x{height} at {hz} Hz. Trying another mode.", name = name.as_str(),
                                      width = mode.width.to_string(), height = mode.height.to_string(), hz = mode.hz().to_string());
                    self.log(Level::Error, &message);
                    self.callback(Level::Error, &message);
                }
            }
            Delta::Vrr(id) => {
                let Some(head) = self.head_mut(id) else { return };
                head.vrr_failed = true;
                let message = tr!("{name}: could not change adaptive sync.", name = head.name.as_str());
                self.log(Level::Warning, &message);
                self.callback(Level::Warning, &message);
            }
            Delta::None => {
                // Not fatal: exiting would only repeat this through the service's
                // restarts. Wait until something changes instead.
                let message = tr!("The compositor refused the display layout: {details}", details = self.summary.as_str());
                self.log(Level::Error, &message);
                self.callback(Level::Error, &message);
                self.blocked = true;
            }
        }
    }

    fn apply(&mut self, qh: &QueueHandle<Self>) {
        let plan = layout::plan(&self.heads);
        if plan == Plan::Nothing {
            return;
        }
        let (Some(manager), Some(serial)) = (self.manager.clone(), self.serial) else { return };
        let configuration = manager.create_configuration(serial, qh, ());
        self.delta = Delta::None;
        match plan {
            Plan::Nothing => unreachable!(),
            Plan::Reapply(index) => {
                let head = &mut self.heads[index];
                head.reapply = false;
                self.summary = tr!("{name}: reapplying settings.", name = head.name.as_str());
                if let Some(proxy) = self.head_proxies.get(&head.id) {
                    configuration.disable_head(proxy);
                }
            }
            Plan::Mode(index) => {
                let head = &self.heads[index];
                let Some(mode_id) = head.des.mode else { return configuration.destroy() };
                let (Some(proxy), Some(mode)) = (self.head_proxies.get(&head.id), self.mode_proxies.get(&mode_id)) else { return configuration.destroy() };
                self.delta = Delta::Mode(head.id, mode_id);
                self.summary = describe_mode(head, mode_id);
                configuration.enable_head(proxy, qh, ()).set_mode(mode);
            }
            Plan::Vrr(index) => {
                let head = &self.heads[index];
                let Some(proxy) = self.head_proxies.get(&head.id) else { return configuration.destroy() };
                self.delta = Delta::Vrr(head.id);
                self.summary = if head.des.vrr {
                    tr!("{name}: adaptive sync on.", name = head.name.as_str())
                } else {
                    tr!("{name}: adaptive sync off.", name = head.name.as_str())
                };
                let state = if head.des.vrr { AdaptiveSyncState::Enabled } else { AdaptiveSyncState::Disabled };
                configuration.enable_head(proxy, qh, ()).set_adaptive_sync(state);
            }
            Plan::All(indices) => {
                let mut summary = Vec::new();
                for index in indices {
                    let head = &self.heads[index];
                    let Some(proxy) = self.head_proxies.get(&head.id) else { continue };
                    if head.des.enabled {
                        let change = configuration.enable_head(proxy, qh, ());
                        change.set_scale(f64::from(head.des.scale) / 256.0);
                        change.set_position(head.des.x, head.des.y);
                        change.set_transform(transform(head.des.transform));
                        summary.push(tr!("{name}: scale {scale} at position {x}, {y}.", name = head.name.as_str(), scale = (f64::from(head.des.scale) / 256.0).to_string(),
                                         x = head.des.x.to_string(), y = head.des.y.to_string()));
                    } else {
                        configuration.disable_head(proxy);
                        summary.push(tr!("{name}: off.", name = head.name.as_str()));
                    }
                }
                self.summary = summary.join("\n");
            }
        }
        configuration.apply();
        self.phase = Phase::Outstanding;
    }

    // ------------------------------------------------------------- state

    fn head_value(&self, head: &Head) -> Value {
        let mode_value = |mode: &Mode| json!({"WIDTH": mode.width, "HEIGHT": mode.height, "REFRESH_MHZ": mode.mhz});
        let state = |state: &layout::State| {
            let mut out = Map::new();
            if state.scale != 0 {
                out.insert("SCALE".into(), json!(f64::from(state.scale) / 256.0));
            }
            out.insert("ENABLED".into(), state.enabled.into());
            out.insert("X".into(), state.x.into());
            out.insert("Y".into(), state.y.into());
            out.insert("VRR".into(), state.vrr.into());
            out.insert("TRANSFORM".into(), config::transform_name(state.transform).into());
            if let Some(mode) = state.mode.and_then(|id| head.modes.iter().find(|mode| mode.id == id)) {
                out.insert("MODE".into(), mode_value(mode));
            }
            Value::Object(out)
        };
        let mut out = Map::new();
        out.insert("NAME".into(), head.name.clone().into());
        out.insert("DESCRIPTION".into(), head.description.clone().into());
        out.insert("MAKE".into(), head.make.clone().into());
        out.insert("MODEL".into(), head.model.clone().into());
        out.insert("SERIAL_NUMBER".into(), head.serial.clone().into());
        out.insert("WIDTH_MM".into(), head.width_mm.into());
        out.insert("HEIGHT_MM".into(), head.height_mm.into());
        out.insert("CURRENT".into(), state(&head.cur));
        out.insert("DESIRED".into(), state(&head.des));
        out.insert("OVERRIDES".into(), match head.override_enabled {
            Some(enabled) => json!({"DISABLED": enabled}),
            None => json!({}),
        });
        if let Some(mode) = head.preferred.and_then(|id| head.modes.iter().find(|mode| mode.id == id)) {
            out.insert("MODE_PREFERRED".into(), mode_value(mode));
        }
        out.insert("MODES".into(), head.modes.iter().map(mode_value).collect());
        out.insert("MODES_FAILED".into(), head.modes_failed.iter().map(mode_value).collect());
        Value::Object(out)
    }

    fn reply(&self, done: bool, with_state: bool, messages: Vec<Value>, rc: i64) -> Value {
        let mut out = Map::new();
        out.insert("DONE".into(), done.into());
        if with_state {
            out.insert("CFG".into(), self.cfg.to_value());
            let mut state = Map::new();
            if let Some(closed) = self.lid {
                state.insert("LID".into(), json!({"CLOSED": closed, "DEVICE_PATH": ""}));
            }
            if !self.heads.is_empty() {
                state.insert("HEADS".into(), self.heads.iter().map(|head| self.head_value(head)).collect());
            }
            out.insert("STATE".into(), Value::Object(state));
        }
        if !messages.is_empty() {
            out.insert("MESSAGES".into(), Value::Array(messages));
        }
        out.insert("RC".into(), rc.into());
        Value::Object(out)
    }

    // --------------------------------------------------------------- ipc

    fn finish_operation(&mut self) {
        if self.phase != Phase::Idle {
            return;
        }
        let Some(operation) = self.operation.take() else { return };
        let reply = self.reply(true, true, operation.messages, operation.rc);
        send(operation.client, &operation.command, &reply);
    }

    fn receive(&mut self, listener: &UnixListener) {
        let Ok((mut client, _)) = listener.accept() else { return };
        let _ = client.set_nonblocking(false);
        let _ = client.set_write_timeout(Some(Duration::from_secs(2)));
        if self.operation.is_some() {
            let reply = self.reply(true, false, Vec::new(), RC_IN_PROGRESS);
            return send(client, "", &reply);
        }
        let Some(text) = read_request(&mut client) else { return };
        let Ok(request) = crate::compat::yaml::load(&text) else {
            let reply = self.reply(true, false, Vec::new(), RC_BAD_REQUEST);
            return send(client, "", &reply);
        };
        let command = request.get("OP").and_then(Value::as_str).unwrap_or("").to_owned();
        let mut from = Partial::from_value(request.get("CFG").unwrap_or(&Value::Null));
        self.operation = Some(Operation { client, command: command.clone(), rc: 0, messages: Vec::new() });
        match command.as_str() {
            "GET" | "LIST" => {}
            "CFG_WRITE" => self.write_config(),
            "REAPPLY" => {
                self.blocked = false;
                for head in &mut self.heads {
                    head.reapply = true;
                }
            }
            "CFG_SET" | "CFG_DEL" | "CFG_TOGGLE" => {
                layout::apply_disabled_overrides(&self.cfg, &mut self.heads, &command, &mut from.disabled);
                let merged = match command.as_str() {
                    "CFG_SET" => self.cfg.merged_set(&from),
                    "CFG_DEL" => self.cfg.merged_del(&from),
                    _ => self.cfg.merged_toggle(&from),
                };
                if merged != self.cfg {
                    self.cfg = merged;
                }
                self.blocked = false;
            }
            _ => {
                let operation = self.operation.take().unwrap();
                let reply = self.reply(true, false, Vec::new(), RC_BAD_REQUEST);
                send(operation.client, "", &reply);
            }
        }
    }

    fn write_config(&mut self) {
        let text = self.cfg.file_text();
        let path = config::config_path();
        match write_atomic(&path, &text) {
            Ok(()) => {
                self.file_text = text;
                self.log(Level::Info, &format!("Wrote {}", path.display()));
            }
            Err(error) => {
                let message = tr!("Could not write {path}: {error}", path = path.display().to_string(), error = error.to_string());
                self.log(Level::Error, &message);
            }
        }
    }

    /// cfg.yaml changed on disk. A file that does not parse changes nothing.
    fn reload(&mut self) {
        let path = config::config_path();
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        if text == self.file_text {
            return;
        }
        self.file_text = text;
        match Cfg::load(&self.file_text) {
            Ok(cfg) => {
                self.cfg = cfg;
                self.blocked = false;
            }
            Err(error) => {
                let message = tr!("Keeping the current display layout: {path} could not be read: {error}", path = path.display().to_string(), error = error);
                self.log(Level::Error, &message);
            }
        }
    }
}

fn describe_mode(head: &Head, mode_id: u32) -> String {
    match head.modes.iter().find(|mode| mode.id == mode_id) {
        Some(mode) => tr!("{name}: {width}x{height} at {hz} Hz.", name = head.name.as_str(), width = mode.width.to_string(),
                          height = mode.height.to_string(), hz = mode.hz().to_string()),
        None => head.name.clone(),
    }
}

fn transform(value: i32) -> wl_output::Transform {
    match value {
        1 => wl_output::Transform::_90,
        2 => wl_output::Transform::_180,
        3 => wl_output::Transform::_270,
        4 => wl_output::Transform::Flipped,
        5 => wl_output::Transform::Flipped90,
        6 => wl_output::Transform::Flipped180,
        7 => wl_output::Transform::Flipped270,
        _ => wl_output::Transform::Normal,
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let directory = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".cfg.yaml.{}", std::process::id()));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

/// Clients send the whole request, then wait for the answer without closing.
fn read_request(client: &mut UnixStream) -> Option<String> {
    let _ = client.set_read_timeout(Some(Duration::from_secs(2)));
    let mut data = Vec::new();
    let mut chunk = [0u8; 65536];
    let count = client.read(&mut chunk).ok()?;
    data.extend_from_slice(&chunk[..count]);
    // Anything longer arrives right behind the first chunk.
    let _ = client.set_read_timeout(Some(Duration::from_millis(20)));
    while let Ok(count) = client.read(&mut chunk) {
        if count == 0 || data.len() > 16 * 1024 * 1024 {
            break;
        }
        data.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8(data).ok()
}

/// GET answers with a mapping, other operations with a one-item sequence.
fn send(mut client: UnixStream, command: &str, reply: &Value) {
    let value = if command == "GET" { reply.clone() } else { json!([reply]) };
    let _ = client.write_all(crate::compat::yaml::safe_dump(&value).as_bytes());
}

// --------------------------------------------------------------- wayland

impl Dispatch<wl_registry::WlRegistry, ()> for Daemon {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            if interface == Manager::interface().name && state.manager.is_none() {
                state.manager = Some(registry.bind(name, version.min(MANAGER_VERSION), qh, ()));
            }
        }
    }
}

impl Dispatch<Manager, ()> for Daemon {
    fn event(state: &mut Self, _: &Manager, event: zwlr_output_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                let id = head.id().protocol_id();
                state.head_proxies.insert(id, head);
                state.heads.push(Head { id, ..Default::default() });
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                state.serial = Some(serial);
                // Monitors changed, so a refused layout may work now.
                state.blocked = false;
            }
            zwlr_output_manager_v1::Event::Finished => state.manager = None,
            _ => {}
        }
    }

    event_created_child!(Daemon, Manager, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (HeadProxy, ()),
    ]);
}

impl Dispatch<HeadProxy, ()> for Daemon {
    fn event(state: &mut Self, proxy: &HeadProxy, event: zwlr_output_head_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwlr_output_head_v1::Event as E;
        let id = proxy.id().protocol_id();
        if let E::Finished = event {
            state.heads.retain(|head| head.id != id);
            state.head_proxies.remove(&id);
            if proxy.version() >= 3 {
                proxy.release();
            }
            return;
        }
        if let E::Mode { mode } = event {
            let mode_id = mode.id().protocol_id();
            state.mode_owner.insert(mode_id, id);
            state.mode_proxies.insert(mode_id, mode);
            if let Some(head) = state.head_mut(id) {
                head.modes.push(Mode { id: mode_id, width: 0, height: 0, mhz: 0 });
            }
            return;
        }
        let Some(head) = state.head_mut(id) else { return };
        match event {
            E::Name { name } => head.name = name,
            E::Description { description } => head.description = description,
            E::PhysicalSize { width, height } => (head.width_mm, head.height_mm) = (width, height),
            E::Enabled { enabled } => head.cur.enabled = enabled != 0,
            E::CurrentMode { mode } => head.cur.mode = Some(mode.id().protocol_id()),
            E::Position { x, y } => (head.cur.x, head.cur.y) = (x, y),
            E::Transform { transform } => {
                if let WEnum::Value(transform) = transform {
                    head.cur.transform = u32::from(transform) as i32;
                }
            }
            E::Scale { scale } => head.cur.scale = (scale * 256.0).round() as i32,
            E::Make { make } => head.make = make,
            E::Model { model } => head.model = model,
            E::SerialNumber { serial_number } => head.serial = serial_number,
            E::AdaptiveSync { state } => head.cur.vrr = matches!(state, WEnum::Value(AdaptiveSyncState::Enabled)),
            _ => {}
        }
    }

    event_created_child!(Daemon, HeadProxy, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ModeProxy, ()),
    ]);
}

impl Dispatch<ModeProxy, ()> for Daemon {
    fn event(state: &mut Self, proxy: &ModeProxy, event: zwlr_output_mode_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwlr_output_mode_v1::Event as E;
        let id = proxy.id().protocol_id();
        let Some(&owner) = state.mode_owner.get(&id) else { return };
        if let E::Finished = event {
            state.mode_owner.remove(&id);
            state.mode_proxies.remove(&id);
            if let Some(head) = state.head_mut(owner) {
                head.modes.retain(|mode| mode.id != id);
                head.modes_failed.retain(|mode| mode.id != id);
            }
            if proxy.version() >= 3 {
                proxy.release();
            }
            return;
        }
        let Some(head) = state.head_mut(owner) else { return };
        if let E::Preferred = event {
            head.preferred = Some(id);
            return;
        }
        let Some(mode) = head.modes.iter_mut().find(|mode| mode.id == id) else { return };
        match event {
            E::Size { width, height } => (mode.width, mode.height) = (width, height),
            E::Refresh { refresh } => mode.mhz = refresh,
            _ => {}
        }
    }
}

impl Dispatch<Configuration, ()> for Daemon {
    fn event(state: &mut Self, proxy: &Configuration, event: zwlr_output_configuration_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        state.phase = match event {
            zwlr_output_configuration_v1::Event::Succeeded => Phase::Succeeded,
            zwlr_output_configuration_v1::Event::Failed => Phase::Failed,
            zwlr_output_configuration_v1::Event::Cancelled => Phase::Cancelled,
            _ => return,
        };
        proxy.destroy();
    }
}

impl Dispatch<ConfigHead, ()> for Daemon {
    fn event(_: &mut Self, _: &ConfigHead, _: <ConfigHead as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

// ------------------------------------------------------------------- lid

/// logind's view of the lid: `None` without a lid or with LAPTOP_LID_MONITOR off.
fn read_lid(bus: Option<&gio::DBusConnection>, monitor: bool) -> Lid {
    if !monitor {
        return None;
    }
    let property = |name: &str| -> Option<bool> {
        use glib::prelude::*;
        let reply = bus?.call_sync(Some("org.freedesktop.login1"), "/org/freedesktop/login1", "org.freedesktop.DBus.Properties", "Get",
            Some(&("org.freedesktop.login1.Manager", name).to_variant()), None, gio::DBusCallFlags::NONE, 1000, None::<&gio::Cancellable>).ok()?;
        reply.child_value(0).as_variant()?.get::<bool>()
    };
    if property("LidPresent") == Some(true) { property("LidClosed") } else { None }
}

// ------------------------------------------------------------------ main

fn read_config() -> (Cfg, String) {
    for path in config::read_candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        match Cfg::load(&text) {
            Ok(cfg) => {
                let own = path == config::config_path();
                return (cfg, if own { text } else { String::new() });
            }
            Err(error) => eprintln!("tessel-displays: ERROR: {} does not parse, using defaults: {error}", path.display()),
        }
        break;
    }
    (Cfg::default(), String::new())
}

/// Held for the daemon's life: a second copy must not fight over the monitors.
fn take_lock() -> Option<std::fs::File> {
    let path = tessel_core::paths::runtime_dir().join("tessel-displays.lock");
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    // SAFETY: flock on a descriptor this function owns.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

fn watch_config_directory() -> Option<OwnedFd> {
    let directory = config::config_path().parent()?.to_path_buf();
    std::fs::create_dir_all(&directory).ok()?;
    let path = std::ffi::CString::new(directory.to_string_lossy().as_bytes()).ok()?;
    // SAFETY: a new descriptor and a valid path.
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        libc::inotify_add_watch(fd, path.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO);
        Some(OwnedFd::from_raw_fd(fd))
    }
}

struct Loop {
    daemon: RefCell<Daemon>,
    queue: RefCell<EventQueue<Daemon>>,
    conn: Connection,
    quit: glib::MainLoop,
}

impl Loop {
    /// Dispatch what was read, take the next step, tell any waiting client.
    fn settle(&self) {
        let mut daemon = self.daemon.borrow_mut();
        let mut queue = self.queue.borrow_mut();
        let qh = queue.handle();
        if queue.dispatch_pending(&mut daemon).is_err() {
            return self.quit.quit();
        }
        if daemon.manager.is_none() {
            eprintln!("tessel-displays: the compositor's output manager went away");
            return self.quit.quit();
        }
        daemon.act(&qh);
        daemon.finish_operation();
        let _ = self.conn.flush();
    }
}

/// `tessel-displays --daemon`. Returns the process exit code.
pub fn run() -> i32 {
    // SAFETY: at startup, before any thread or child exists. Callbacks reset this.
    unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    let Some(_lock) = take_lock() else {
        eprintln!("tessel-displays: another daemon is already running");
        return 1;
    };
    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(error) => {
            eprintln!("tessel-displays: could not connect to the compositor: {error}");
            return 1;
        }
    };
    let (cfg, file_text) = read_config();
    let bus = gio::bus_get_sync(gio::BusType::System, None::<&gio::Cancellable>).ok();
    let mut daemon = Daemon::new(cfg, file_text);
    daemon.lid = read_lid(bus.as_ref(), daemon.cfg.lid_monitor);

    let mut queue = conn.new_event_queue::<Daemon>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    // Twice: the first binds the manager, the second brings its heads and modes.
    for _ in 0..2 {
        if let Err(error) = queue.roundtrip(&mut daemon) {
            eprintln!("tessel-displays: {error}");
            return 1;
        }
    }
    if daemon.manager.is_none() {
        eprintln!("tessel-displays: this compositor has no output management");
        return 1;
    }

    let socket = socket_path();
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("tessel-displays: could not listen on {}: {error}", socket.display());
            return 1;
        }
    };
    let _ = listener.set_nonblocking(true);

    let quit = glib::MainLoop::new(None, false);
    let state = Rc::new(Loop { daemon: RefCell::new(daemon), queue: RefCell::new(queue), conn: conn.clone(), quit: quit.clone() });

    let source = state.clone();
    let wayland_fd = conn.backend().poll_fd().as_raw_fd();
    tessel_core::glib_compat::unix_fd_add_local(wayland_fd, glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR, move |_, _| {
        let guard = source.queue.borrow().prepare_read();
        if let Some(guard) = guard {
            if guard.read().is_err() {
                source.quit.quit();
                return ControlFlow::Break;
            }
        }
        source.settle();
        ControlFlow::Continue
    });

    let source = state.clone();
    tessel_core::glib_compat::unix_fd_add_local(listener.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
        source.daemon.borrow_mut().receive(&listener);
        source.settle();
        ControlFlow::Continue
    });

    let inotify = watch_config_directory();
    if let Some(fd) = &inotify {
        let source = state.clone();
        let raw = fd.as_raw_fd();
        tessel_core::glib_compat::unix_fd_add_local(raw, glib::IOCondition::IN, move |_, _| {
            let mut buffer = [0u8; 4096];
            // SAFETY: the descriptor outlives the main loop; the buffer is ours.
            while unsafe { libc::read(raw, buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
            source.daemon.borrow_mut().reload();
            source.settle();
            ControlFlow::Continue
        });
    }

    if bus.is_some() {
        let source = state.clone();
        glib::timeout_add_seconds_local(LID_POLL_SECONDS, move || {
            let monitor = source.daemon.borrow().cfg.lid_monitor;
            let lid = read_lid(bus.as_ref(), monitor);
            let changed = {
                let mut daemon = source.daemon.borrow_mut();
                let changed = daemon.lid != lid;
                if changed {
                    daemon.lid = lid;
                    daemon.blocked = false;
                }
                changed
            };
            if changed {
                source.settle();
            }
            ControlFlow::Continue
        });
    }

    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let quit = quit.clone();
        tessel_core::glib_compat::unix_signal_add_local(signal, move || {
            quit.quit();
            ControlFlow::Break
        });
    }

    state.settle();
    quit.run();
    let _ = std::fs::remove_file(&socket);
    drop(inotify);
    0
}

#[cfg(test)]
mod tests {
    /// Every message the daemon can show, in every language, with its placeholders intact.
    const MESSAGES: [&str; 17] = [
        "Display settings", "Display warning", "Display error", "Display changes applied.", "{name}: reapplying settings.",
        "{name}: {width}x{height} at {hz} Hz.", "{name}: adaptive sync on.", "{name}: adaptive sync off.",
        "{name}: scale {scale} at position {x}, {y}.", "{name}: off.", "Display changes were cancelled. Retrying (attempt {attempt}).",
        "Display changes were cancelled after {retries} retries.", "{name}: could not set {width}x{height} at {hz} Hz. Trying another mode.",
        "{name}: could not change adaptive sync.", "The compositor refused the display layout: {details}",
        "Could not write {path}: {error}", "Keeping the current display layout: {path} could not be read: {error}",
    ];

    fn placeholders(text: &str) -> std::collections::BTreeSet<&str> {
        text.split('{').skip(1).filter_map(|part| part.split_once('}').map(|(name, _)| name)).collect()
    }

    #[test]
    fn messages_are_translated_into_every_language() {
        let source = include_str!("mod.rs");
        for message in MESSAGES {
            assert!(source.matches(message).count() >= 2, "{message} is not used");
        }
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../i18n/locale");
        let mut languages = 0;
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.file_stem().is_some_and(|stem| stem == "en") {
                continue;
            }
            languages += 1;
            let catalog: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for message in MESSAGES {
                let text = catalog[message].as_str().unwrap_or_else(|| panic!("{}: {message}", path.display()));
                assert_ne!(text, message, "{}", path.display());
                assert_eq!(placeholders(text), placeholders(message), "{}: {message}", path.display());
            }
        }
        assert_eq!(languages, 30);
    }
}
