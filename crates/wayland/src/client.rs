//! Protocol glue: binds the globals selected by [`Protocols`], feeds every
//! event into [`Model`], and exposes the requests (window actions, workspace
//! actions, image capture).
//!
//! [`Client`] owns a connection and its event queue. Drive it with
//! [`Client::roundtrip`] / [`Client::wait`] (blocking: helpers, tests) or hand
//! it to [`crate::source::GlibClient`] for the GLib main loop.

use crate::model::{self, GlobalName, HandleKey, Model, WindowId, WorkspaceId};
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};
use wayland_client::backend::{ObjectId, WaylandError};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{event_created_child, Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1 as listed_handle, ext_foreign_toplevel_list_v1 as toplevel_list,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1 as toplevel_sources, ext_image_capture_source_v1 as capture_source,
    ext_output_image_capture_source_manager_v1 as output_sources,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as copy_frame, ext_image_copy_capture_manager_v1 as copy_manager,
    ext_image_copy_capture_session_v1 as copy_session,
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1 as group_handle, ext_workspace_handle_v1 as workspace_handle,
    ext_workspace_manager_v1 as workspace_manager,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1 as xdg_manager, zxdg_output_v1 as xdg_output};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1 as wlr_handle, zwlr_foreign_toplevel_manager_v1 as wlr_manager,
};

/// Which protocol families a [`Client`] binds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Protocols {
    /// zwlr_foreign_toplevel_manager_v1 (≤ v3) + wl_seat (≤ v7).
    pub toplevels: bool,
    /// ext_workspace_manager_v1 v1.
    pub workspaces: bool,
    /// zxdg_output_manager_v1 (≤ v3); implied by `toplevels` / `workspaces`.
    pub xdg_output: bool,
    /// wl_shm, ext_foreign_toplevel_list_v1, ext_image_copy_capture_manager_v1
    /// and both ext image-capture source managers.
    pub capture: bool,
}

impl Protocols {
    pub const TOPLEVELS: Self = Self { toplevels: true, workspaces: false, xdg_output: true, capture: false };
    pub const WORKSPACES: Self = Self { toplevels: false, workspaces: true, xdg_output: true, capture: false };
    pub const CAPTURE: Self = Self { toplevels: false, workspaces: false, xdg_output: false, capture: true };
    pub const OUTPUTS: Self = Self { toplevels: false, workspaces: false, xdg_output: true, capture: false };
    pub const ALL: Self = Self { toplevels: true, workspaces: true, xdg_output: true, capture: true };
}

/// Why a window / workspace / capture request was not sent.
/// `Display` gives the helpers' (translatable) English messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The protocol is not offered (or not bound).
    Unsupported,
    /// Unknown, unpublished or closed window.
    NoWindow,
    NoSeat,
    NoWorkspace,
    CannotActivateWorkspace,
    NoGroup,
    NoOutput,
    NoSource,
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "The compositor does not support this request.",
            Self::NoWindow => "That window is no longer available.",
            Self::NoSeat => "No input seat is available to focus windows.",
            Self::NoWorkspace => "That workspace is no longer available.",
            Self::CannotActivateWorkspace => "This workspace cannot be activated.",
            Self::NoGroup => "That workspace group is no longer available.",
            Self::NoOutput => "That display is no longer available.",
            Self::NoSource => "That window can no longer be captured.",
        })
    }
}
impl std::error::Error for ActionError {}

/// A captured image in a wl_shm layout (little-endian `[B, G, R, A]` bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row (`width * 4`).
    pub stride: u32,
    /// `wl_shm` format: [`Frame::ARGB8888`] (premultiplied alpha) or [`Frame::XRGB8888`].
    pub format: u32,
    /// `wl_output.transform` of the content (0 = normal).
    pub transform: u32,
    pub data: Vec<u8>,
}

impl Frame {
    pub const ARGB8888: u32 = 0;
    pub const XRGB8888: u32 = 1;
    pub fn has_alpha(&self) -> bool { self.format == Self::ARGB8888 }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame").field("width", &self.width).field("height", &self.height).field("format", &self.format)
            .field("transform", &self.transform).field("bytes", &self.data.len()).finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// No usable shm format/size constraints (or larger than 16384² / 32 Mpx).
    Constraints,
    /// Shared memory could not be allocated.
    Memory,
    /// `ext_image_copy_capture_frame_v1.failed` with its reason.
    Failed(u32),
    /// The session stopped (source gone, e.g. window closed).
    Stopped,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Constraints => f.write_str("unsupported capture constraints"),
            Self::Memory => f.write_str("cannot allocate shared memory"),
            Self::Failed(reason) => write!(f, "capture failed (reason {reason})"),
            Self::Stopped => f.write_str("capture session stopped"),
        }
    }
}
impl std::error::Error for CaptureError {}

pub type CaptureId = u64;

/// Everything a client reports, in order.
#[derive(Debug)]
pub enum Event {
    Changed(model::Event),
    Captured(CaptureId, Result<Frame, CaptureError>),
    /// The connection failed (only from [`crate::source::GlibClient`]).
    Disconnected(String),
}

struct Mapping {
    pointer: *mut libc::c_void,
    size: usize,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.pointer, self.size) };
    }
}

struct Capture {
    session: copy_session::ExtImageCopyCaptureSessionV1,
    source: capture_source::ExtImageCaptureSourceV1,
    frame: Option<copy_frame::ExtImageCopyCaptureFrameV1>,
    buffer: Option<wl_buffer::WlBuffer>,
    mapping: Option<Mapping>,
    width: u32,
    height: u32,
    format: Option<u32>,
    transform: u32,
}

/// Bound globals plus the [`Model`]; implements every `Dispatch`.
pub struct State {
    pub model: Model,
    protocols: Protocols,
    qh: QueueHandle<State>,
    registry: wl_registry::WlRegistry,
    toplevel_manager: Option<(GlobalName, wlr_manager::ZwlrForeignToplevelManagerV1)>,
    toplevel_version: u32,
    seat: Option<(GlobalName, wl_seat::WlSeat)>,
    xdg: Option<(GlobalName, xdg_manager::ZxdgOutputManagerV1)>,
    outputs: Vec<(GlobalName, wl_output::WlOutput, Option<xdg_output::ZxdgOutputV1>)>,
    toplevels: HashMap<WindowId, wlr_handle::ZwlrForeignToplevelHandleV1>,
    toplevel_ids: HashMap<ObjectId, WindowId>,
    workspace_manager: Option<(GlobalName, workspace_manager::ExtWorkspaceManagerV1)>,
    groups: HashMap<WorkspaceId, group_handle::ExtWorkspaceGroupHandleV1>,
    group_ids: HashMap<ObjectId, WorkspaceId>,
    workspaces: HashMap<WorkspaceId, workspace_handle::ExtWorkspaceHandleV1>,
    workspace_ids: HashMap<ObjectId, WorkspaceId>,
    shm: Option<wl_shm::WlShm>,
    list: Option<(GlobalName, toplevel_list::ExtForeignToplevelListV1)>,
    copy: Option<copy_manager::ExtImageCopyCaptureManagerV1>,
    toplevel_sources: Option<toplevel_sources::ExtForeignToplevelImageCaptureSourceManagerV1>,
    output_sources: Option<output_sources::ExtOutputImageCaptureSourceManagerV1>,
    listed: HashMap<HandleKey, listed_handle::ExtForeignToplevelHandleV1>,
    listed_ids: HashMap<ObjectId, HandleKey>,
    /// Keep closed ext-foreign-toplevel handles in the model (overview helper).
    pub keep_closed_listed: bool,
    /// Maximum number of ext-foreign-toplevel handles tracked (None = all).
    pub listed_limit: Option<usize>,
    captures: HashMap<CaptureId, Capture>,
    next_capture: CaptureId,
    captured: Vec<(CaptureId, Result<Frame, CaptureError>)>,
}

fn bits<T: Into<u32> + Copy>(value: WEnum<T>) -> u32 {
    match value {
        WEnum::Value(v) => v.into(),
        WEnum::Unknown(raw) => raw,
    }
}

impl State {
    fn new(conn: &Connection, qh: QueueHandle<State>, protocols: Protocols) -> Self {
        let registry = conn.display().get_registry(&qh, ());
        Self {
            model: Model::new(), protocols, qh, registry, toplevel_manager: None, toplevel_version: 0, seat: None, xdg: None,
            outputs: Vec::new(), toplevels: HashMap::new(), toplevel_ids: HashMap::new(), workspace_manager: None,
            groups: HashMap::new(), group_ids: HashMap::new(), workspaces: HashMap::new(), workspace_ids: HashMap::new(),
            shm: None, list: None, copy: None, toplevel_sources: None, output_sources: None, listed: HashMap::new(),
            listed_ids: HashMap::new(), keep_closed_listed: false, listed_limit: None, captures: HashMap::new(),
            next_capture: 0, captured: Vec::new(),
        }
    }

    fn wants_outputs(&self) -> bool {
        let p = self.protocols;
        p.toplevels || p.workspaces || p.xdg_output || p.capture
    }
    fn wants_xdg(&self) -> bool { self.protocols.xdg_output || self.protocols.toplevels || self.protocols.workspaces }

    fn bind_logical(&mut self, index: usize) {
        if let (Some((_, manager)), Some((global, output, logical))) = (&self.xdg, self.outputs.get_mut(index)) {
            if logical.is_none() {
                *logical = Some(manager.get_xdg_output(output, &self.qh, *global));
            }
        }
    }

    // ---- queries ---------------------------------------------------------------
    pub fn protocols(&self) -> Protocols { self.protocols }
    /// Bound zwlr_foreign_toplevel_manager_v1 version (0 = unsupported).
    pub fn toplevel_version(&self) -> u32 { self.toplevel_version }
    pub fn has_toplevels(&self) -> bool { self.toplevel_manager.is_some() }
    pub fn has_seat(&self) -> bool { self.seat.is_some() }
    pub fn has_workspaces(&self) -> bool { self.workspace_manager.is_some() }
    pub fn has_xdg_output(&self) -> bool { self.xdg.is_some() }
    /// Toplevel capture needs shm, the ext list, copy manager and toplevel sources.
    pub fn can_capture_toplevels(&self) -> bool {
        self.shm.is_some() && self.list.is_some() && self.copy.is_some() && self.toplevel_sources.is_some()
    }
    pub fn can_capture_outputs(&self) -> bool { self.shm.is_some() && self.copy.is_some() && self.output_sources.is_some() }
    pub fn wl_output(&self, global: GlobalName) -> Option<&wl_output::WlOutput> {
        self.outputs.iter().find(|(g, ..)| *g == global).map(|(_, o, _)| o)
    }
    /// The `ext_foreign_toplevel_handle_v1` uniquely matching a foreign-toplevel
    /// window by app_id + title (the overview's identity rule), if any.
    pub fn listed_for_window(&self, id: WindowId) -> Option<HandleKey> {
        let window = self.model.window(id)?;
        let mut matches = self.model.listed().iter().filter(|t| {
            !t.closed && t.app_id.as_deref().unwrap_or("") == window.app_id && t.title.as_deref().unwrap_or("") == window.title
        });
        let first = matches.next()?.key;
        let twins = self.model.windows().filter(|w| w.app_id == window.app_id && w.title == window.title).count();
        (matches.next().is_none() && twins == 1).then_some(first)
    }

    // ---- window requests ----------------------------------------------------------
    fn handle(&self, id: WindowId) -> Result<&wlr_handle::ZwlrForeignToplevelHandleV1, ActionError> {
        if self.toplevel_manager.is_none() && self.toplevels.is_empty() {
            return Err(ActionError::Unsupported);
        }
        match (self.model.window(id), self.toplevels.get(&id)) {
            (Some(w), Some(handle)) if w.published => Ok(handle),
            _ => Err(ActionError::NoWindow),
        }
    }
    /// Focus a window; a minimized window is restored first (helper behaviour).
    pub fn activate(&self, id: WindowId) -> Result<(), ActionError> {
        let handle = self.handle(id)?;
        let Some((_, seat)) = &self.seat else { return Err(ActionError::NoSeat) };
        if self.model.window(id).is_some_and(|w| w.minimized) {
            handle.unset_minimized();
        }
        handle.activate(seat);
        Ok(())
    }
    pub fn set_minimized(&self, id: WindowId, minimized: bool) -> Result<(), ActionError> {
        let handle = self.handle(id)?;
        if minimized { handle.set_minimized() } else { handle.unset_minimized() }
        Ok(())
    }
    pub fn set_maximized(&self, id: WindowId, maximized: bool) -> Result<(), ActionError> {
        let handle = self.handle(id)?;
        if maximized { handle.set_maximized() } else { handle.unset_maximized() }
        Ok(())
    }
    /// `output`: an output name, or None to let the compositor choose. Needs v2.
    pub fn set_fullscreen(&self, id: WindowId, fullscreen: bool, output: Option<&str>) -> Result<(), ActionError> {
        let handle = self.handle(id)?;
        if handle.version() < 2 {
            return Err(ActionError::Unsupported);
        }
        if !fullscreen {
            handle.unset_fullscreen();
            return Ok(());
        }
        let target = match output {
            Some(name) => {
                let global = self.model.output_by_name(name).ok_or(ActionError::NoOutput)?.global;
                Some(self.wl_output(global).ok_or(ActionError::NoOutput)?)
            }
            None => None,
        };
        handle.set_fullscreen(target);
        Ok(())
    }
    pub fn close(&self, id: WindowId) -> Result<(), ActionError> {
        self.handle(id)?.close();
        Ok(())
    }
    /// Minimize-animation hint: the rectangle (surface-local) representing the
    /// window. The surface must belong to this client's connection — use a
    /// client made with `Client::for_gdk_display` and `gtk_wl_surface`.
    pub fn set_rectangle(&self, id: WindowId, surface: &wl_surface::WlSurface, x: i32, y: i32, width: i32, height: i32)
            -> Result<(), ActionError> {
        self.handle(id)?.set_rectangle(surface, x, y, width, height);
        Ok(())
    }

    // ---- workspace requests (each followed by a manager commit) ----------------------
    fn workspace_handle(&self, id: WorkspaceId) -> Result<(&workspace_manager::ExtWorkspaceManagerV1, &workspace_handle::ExtWorkspaceHandleV1), ActionError> {
        let Some((_, manager)) = &self.workspace_manager else { return Err(ActionError::Unsupported) };
        let handle = self.workspaces.get(&id).filter(|_| self.model.workspace(id).is_some()).ok_or(ActionError::NoWorkspace)?;
        Ok((manager, handle))
    }
    /// Only activatable, non-hidden workspaces (helper rule).
    pub fn activate_workspace(&self, id: WorkspaceId) -> Result<(), ActionError> {
        let (manager, handle) = self.workspace_handle(id)?;
        let workspace = self.model.workspace(id).ok_or(ActionError::NoWorkspace)?;
        if !workspace.can_activate() || workspace.hidden() {
            return Err(ActionError::CannotActivateWorkspace);
        }
        handle.activate();
        manager.commit();
        Ok(())
    }
    pub fn deactivate_workspace(&self, id: WorkspaceId) -> Result<(), ActionError> {
        let (manager, handle) = self.workspace_handle(id)?;
        handle.deactivate();
        manager.commit();
        Ok(())
    }
    pub fn remove_workspace(&self, id: WorkspaceId) -> Result<(), ActionError> {
        let (manager, handle) = self.workspace_handle(id)?;
        handle.remove();
        manager.commit();
        Ok(())
    }
    /// Move a workspace to another group (needs the `assign` capability).
    pub fn assign_workspace(&self, id: WorkspaceId, group: WorkspaceId) -> Result<(), ActionError> {
        let (manager, handle) = self.workspace_handle(id)?;
        let group = self.groups.get(&group).ok_or(ActionError::NoGroup)?;
        handle.assign(group);
        manager.commit();
        Ok(())
    }
    pub fn create_workspace(&self, group: WorkspaceId, name: &str) -> Result<(), ActionError> {
        let Some((_, manager)) = &self.workspace_manager else { return Err(ActionError::Unsupported) };
        let group = self.groups.get(&group).ok_or(ActionError::NoGroup)?;
        group.create_workspace(name.to_owned());
        manager.commit();
        Ok(())
    }

    // ---- image capture ---------------------------------------------------------------
    /// Capture one frame of an ext-foreign-toplevel handle. The result arrives
    /// as `Event::Captured(id, …)`.
    pub fn capture_listed(&mut self, key: HandleKey, paint_cursors: bool) -> Result<CaptureId, ActionError> {
        let (Some(_), Some(copy), Some(sources)) = (&self.shm, &self.copy, &self.toplevel_sources) else {
            return Err(ActionError::Unsupported);
        };
        let handle = self.listed.get(&key).filter(|_| self.model.listed_toplevel(key).is_some_and(|t| !t.closed))
            .ok_or(ActionError::NoSource)?;
        let source = sources.create_source(handle, &self.qh, ());
        let copy = copy.clone();
        Ok(self.start_capture(&copy, source, paint_cursors))
    }
    /// Capture one frame of a named output.
    pub fn capture_output(&mut self, name: &str, paint_cursors: bool) -> Result<CaptureId, ActionError> {
        let (Some(_), Some(copy), Some(sources)) = (&self.shm, &self.copy, &self.output_sources) else {
            return Err(ActionError::Unsupported);
        };
        let global = self.model.output_by_name(name).ok_or(ActionError::NoOutput)?.global;
        let output = self.wl_output(global).ok_or(ActionError::NoOutput)?;
        let source = sources.create_source(output, &self.qh, ());
        let copy = copy.clone();
        Ok(self.start_capture(&copy, source, paint_cursors))
    }
    fn start_capture(&mut self, copy: &copy_manager::ExtImageCopyCaptureManagerV1,
                     source: capture_source::ExtImageCaptureSourceV1, paint_cursors: bool) -> CaptureId {
        self.next_capture += 1;
        let id = self.next_capture;
        let options = if paint_cursors { copy_manager::Options::PaintCursors } else { copy_manager::Options::empty() };
        let session = copy.create_session(&source, options, &self.qh, id);
        self.captures.insert(id, Capture { session, source, frame: None, buffer: None, mapping: None, width: 0, height: 0,
                                           format: None, transform: 0 });
        id
    }
    /// Abandon a capture; no `Captured` event follows.
    pub fn cancel_capture(&mut self, id: CaptureId) {
        if let Some(capture) = self.captures.remove(&id) {
            Self::destroy_capture(capture);
        }
    }
    pub fn capture_pending(&self, id: CaptureId) -> bool { self.captures.contains_key(&id) }
    pub fn take_captured(&mut self) -> Vec<(CaptureId, Result<Frame, CaptureError>)> { std::mem::take(&mut self.captured) }

    fn destroy_capture(capture: Capture) {
        if let Some(frame) = capture.frame {
            frame.destroy();
        }
        capture.session.destroy();
        capture.source.destroy();
        if let Some(buffer) = capture.buffer {
            buffer.destroy();
        }
        drop(capture.mapping);
    }
    fn finish_capture(&mut self, id: CaptureId, result: Result<(), CaptureError>) {
        let Some(capture) = self.captures.remove(&id) else { return };
        let result = result.and_then(|()| {
            let mapping = capture.mapping.as_ref().ok_or(CaptureError::Memory)?;
            let data = unsafe { std::slice::from_raw_parts(mapping.pointer as *const u8, mapping.size) }.to_vec();
            Ok(Frame { width: capture.width, height: capture.height, stride: capture.width * 4,
                       format: capture.format.unwrap_or(Frame::ARGB8888), transform: capture.transform, data })
        });
        Self::destroy_capture(capture);
        self.captured.push((id, result));
    }
    /// `ext_image_copy_capture_session_v1.done`: allocate one shm buffer and
    /// request a single full-damage frame (overview_capture.c).
    fn constraints_done(&mut self, id: CaptureId) {
        let Some(capture) = self.captures.get_mut(&id) else { return };
        if capture.frame.is_some() {
            return;
        }
        let (width, height) = (capture.width, capture.height);
        let Some(format) = capture.format.filter(|_| width > 0 && height > 0 && width <= 16384 && height <= 16384
                                                       && (width as u64) * (height as u64) <= 32 * 1024 * 1024) else {
            return self.finish_capture(id, Err(CaptureError::Constraints));
        };
        let size = width as usize * height as usize * 4;
        let Some((fd, mapping)) = shared_memory(size) else { return self.finish_capture(id, Err(CaptureError::Memory)) };
        let shm = self.shm.as_ref().expect("capture requires wl_shm");
        let pool = shm.create_pool(fd.as_fd(), size as i32, &self.qh, ());
        let shm_format = if format == Frame::XRGB8888 { wl_shm::Format::Xrgb8888 } else { wl_shm::Format::Argb8888 };
        let buffer = pool.create_buffer(0, width as i32, height as i32, (width * 4) as i32, shm_format, &self.qh, ());
        pool.destroy();
        drop(fd);
        let capture = self.captures.get_mut(&id).unwrap();
        let frame = capture.session.create_frame(&self.qh, id);
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();
        capture.frame = Some(frame);
        capture.buffer = Some(buffer);
        capture.mapping = Some(mapping);
    }

    fn registry_global(&mut self, name: GlobalName, interface: &str, version: u32) {
        let qh = self.qh.clone();
        let p = self.protocols;
        match interface {
            "zwlr_foreign_toplevel_manager_v1" if p.toplevels && self.toplevel_manager.is_none() => {
                let version = version.min(3);
                self.toplevel_version = version;
                self.toplevel_manager = Some((name, self.registry.bind(name, version, &qh, ())));
            }
            "wl_seat" if p.toplevels && self.seat.is_none() => {
                self.seat = Some((name, self.registry.bind(name, version.min(7), &qh, ())));
            }
            "wl_output" if self.wants_outputs() => {
                let output = self.registry.bind(name, version.min(4), &qh, name);
                self.model.output_added(name);
                self.outputs.insert(0, (name, output, None));
                self.bind_logical(0);
            }
            "zxdg_output_manager_v1" if self.wants_xdg() && self.xdg.is_none() => {
                self.xdg = Some((name, self.registry.bind(name, version.min(3), &qh, ())));
                for index in 0..self.outputs.len() {
                    self.bind_logical(index);
                }
            }
            "ext_workspace_manager_v1" if p.workspaces && self.workspace_manager.is_none() => {
                self.workspace_manager = Some((name, self.registry.bind(name, 1, &qh, ())));
            }
            "wl_shm" if p.capture && self.shm.is_none() => self.shm = Some(self.registry.bind(name, 1, &qh, ())),
            "ext_foreign_toplevel_list_v1" if p.capture && self.list.is_none() => {
                self.list = Some((name, self.registry.bind(name, 1, &qh, ())));
            }
            "ext_image_copy_capture_manager_v1" if p.capture && self.copy.is_none() => {
                self.copy = Some(self.registry.bind(name, 1, &qh, ()));
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1" if p.capture && self.toplevel_sources.is_none() => {
                self.toplevel_sources = Some(self.registry.bind(name, 1, &qh, ()));
            }
            "ext_output_image_capture_source_manager_v1" if p.capture && self.output_sources.is_none() => {
                self.output_sources = Some(self.registry.bind(name, 1, &qh, ()));
            }
            _ => {}
        }
    }

    fn registry_remove(&mut self, name: GlobalName) {
        let window_global = [self.toplevel_manager.as_ref().map(|m| m.0), self.seat.as_ref().map(|s| s.0)].contains(&Some(name));
        let workspace_global = self.workspace_manager.as_ref().is_some_and(|m| m.0 == name);
        let xdg_global = self.xdg.as_ref().is_some_and(|m| m.0 == name);
        if window_global || (xdg_global && self.protocols.toplevels) {
            self.model.error("The compositor's window-management capabilities changed.", true);
            return;
        }
        if workspace_global || xdg_global {
            self.model.error("The compositor's workspace capabilities changed.", true);
            return;
        }
        if let Some(index) = self.outputs.iter().position(|(g, ..)| *g == name) {
            self.model.output_removed(name);
            let (_, output, logical) = self.outputs.remove(index);
            if let Some(logical) = logical {
                logical.destroy();
            }
            if output.version() >= 3 {
                output.release();
            }
        }
    }

    fn output_global(output: &wl_output::WlOutput) -> Option<GlobalName> { output.data::<GlobalName>().copied() }
}

/// Fold one offered `wl_shm` format into the choice so far: ARGB8888 wins (so
/// client-side-decorated windows keep transparent corners and shadows), then
/// XRGB8888; anything else is ignored (overview_capture.c).
pub fn prefer_format(current: Option<u32>, offered: u32) -> Option<u32> {
    if offered == Frame::ARGB8888 || (offered == Frame::XRGB8888 && current != Some(Frame::ARGB8888)) {
        Some(offered)
    } else {
        current
    }
}

fn shared_memory(size: usize) -> Option<(OwnedFd, Mapping)> {
    let fd = unsafe { libc::memfd_create(c"tessel-overview".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } < 0 {
        return None;
    }
    let pointer = unsafe {
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0)
    };
    if pointer == libc::MAP_FAILED {
        return None;
    }
    Some((fd, Mapping { pointer, size }))
}

// ---- Dispatch -------------------------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, _: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } => state.registry_global(name, &interface, version),
            wl_registry::Event::GlobalRemove { name } => state.registry_remove(name),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, GlobalName> for State {
    fn event(state: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event, global: &GlobalName, _: &Connection, _: &QueueHandle<Self>) {
        let (m, g) = (&mut state.model, *global);
        match event {
            wl_output::Event::Geometry { x, y, transform, .. } => m.output_geometry(g, x, y, bits(transform) as i32),
            wl_output::Event::Mode { flags, width, height, .. } => {
                m.output_mode(g, bits(flags) & u32::from(wl_output::Mode::Current) != 0, width, height)
            }
            wl_output::Event::Done => m.output_done(g),
            wl_output::Event::Scale { factor } => m.output_scale(g, factor),
            wl_output::Event::Name { name } => m.output_name(g, &name),
            wl_output::Event::Description { description } => m.output_description(g, &description),
            _ => {}
        }
    }
}

impl Dispatch<xdg_output::ZxdgOutputV1, GlobalName> for State {
    fn event(state: &mut Self, _: &xdg_output::ZxdgOutputV1, event: xdg_output::Event, global: &GlobalName, _: &Connection, _: &QueueHandle<Self>) {
        let (m, g) = (&mut state.model, *global);
        match event {
            xdg_output::Event::LogicalPosition { x, y } => m.logical_position(g, x, y),
            xdg_output::Event::LogicalSize { width, height } => m.logical_size(g, width, height),
            xdg_output::Event::Done => m.logical_done(g),
            xdg_output::Event::Name { name } => m.output_name(g, &name),
            xdg_output::Event::Description { description } => m.output_description(g, &description),
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for State {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    xdg_manager::ZxdgOutputManagerV1, wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer,
    copy_manager::ExtImageCopyCaptureManagerV1, toplevel_sources::ExtForeignToplevelImageCaptureSourceManagerV1,
    output_sources::ExtOutputImageCaptureSourceManagerV1, capture_source::ExtImageCaptureSourceV1,
);

impl Dispatch<wlr_manager::ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(state: &mut Self, _: &wlr_manager::ZwlrForeignToplevelManagerV1, event: wlr_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wlr_manager::Event::Toplevel { toplevel } => {
                let id = state.model.toplevel_new();
                state.toplevel_ids.insert(toplevel.id(), id);
                state.toplevels.insert(id, toplevel);
            }
            wlr_manager::Event::Finished => {
                state.model.error("The compositor stopped window management.", true);
            }
            _ => {}
        }
    }
    event_created_child!(State, wlr_manager::ZwlrForeignToplevelManagerV1, [
        wlr_manager::EVT_TOPLEVEL_OPCODE => (wlr_handle::ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<wlr_handle::ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(state: &mut Self, handle: &wlr_handle::ZwlrForeignToplevelHandleV1, event: wlr_handle::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(&id) = state.toplevel_ids.get(&handle.id()) else { return };
        let m = &mut state.model;
        match event {
            wlr_handle::Event::Title { title } => m.toplevel_title(id, &title),
            wlr_handle::Event::AppId { app_id } => m.toplevel_app_id(id, &app_id),
            wlr_handle::Event::OutputEnter { output } => {
                if let Some(g) = State::output_global(&output) {
                    m.toplevel_output_enter(id, g)
                }
            }
            wlr_handle::Event::OutputLeave { output } => {
                if let Some(g) = State::output_global(&output) {
                    m.toplevel_output_leave(id, g)
                }
            }
            wlr_handle::Event::State { state: raw } => {
                let states: Vec<u32> = raw.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect();
                m.toplevel_state(id, &states)
            }
            wlr_handle::Event::Done => m.toplevel_done(id),
            wlr_handle::Event::Closed => {
                m.toplevel_closed(id);
                state.toplevel_ids.remove(&handle.id());
                if let Some(handle) = state.toplevels.remove(&id) {
                    handle.destroy();
                }
            }
            wlr_handle::Event::Parent { parent } => {
                let parent = parent.and_then(|p| state.toplevel_ids.get(&p.id()).copied());
                state.model.toplevel_parent(id, parent)
            }
            _ => {}
        }
    }
}

impl Dispatch<workspace_manager::ExtWorkspaceManagerV1, ()> for State {
    fn event(state: &mut Self, manager: &workspace_manager::ExtWorkspaceManagerV1, event: workspace_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            workspace_manager::Event::WorkspaceGroup { workspace_group } => {
                let id = state.model.group_new();
                state.group_ids.insert(workspace_group.id(), id);
                state.groups.insert(id, workspace_group);
            }
            workspace_manager::Event::Workspace { workspace } => {
                let id = state.model.workspace_new();
                state.workspace_ids.insert(workspace.id(), id);
                state.workspaces.insert(id, workspace);
            }
            workspace_manager::Event::Done => state.model.workspaces_done(),
            workspace_manager::Event::Finished => {
                let _ = manager;
                state.workspace_manager = None;
                state.model.error("The compositor's workspace connection ended.", true);
            }
            _ => {}
        }
    }
    event_created_child!(State, workspace_manager::ExtWorkspaceManagerV1, [
        workspace_manager::EVT_WORKSPACE_GROUP_OPCODE => (group_handle::ExtWorkspaceGroupHandleV1, ()),
        workspace_manager::EVT_WORKSPACE_OPCODE => (workspace_handle::ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<group_handle::ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(state: &mut Self, handle: &group_handle::ExtWorkspaceGroupHandleV1, event: group_handle::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(&id) = state.group_ids.get(&handle.id()) else { return };
        match event {
            group_handle::Event::Capabilities { capabilities } => state.model.group_capabilities(id, bits(capabilities)),
            group_handle::Event::OutputEnter { output } => {
                if let Some(g) = State::output_global(&output) {
                    state.model.group_output_enter(id, g)
                }
            }
            group_handle::Event::OutputLeave { output } => {
                if let Some(g) = State::output_global(&output) {
                    state.model.group_output_leave(id, g)
                }
            }
            group_handle::Event::WorkspaceEnter { workspace } => {
                if let Some(&w) = state.workspace_ids.get(&workspace.id()) {
                    state.model.group_workspace_enter(id, w)
                }
            }
            group_handle::Event::WorkspaceLeave { workspace } => {
                if let Some(&w) = state.workspace_ids.get(&workspace.id()) {
                    state.model.group_workspace_leave(id, w)
                }
            }
            group_handle::Event::Removed => {
                state.model.group_removed(id);
                state.group_ids.remove(&handle.id());
                if let Some(handle) = state.groups.remove(&id) {
                    handle.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<workspace_handle::ExtWorkspaceHandleV1, ()> for State {
    fn event(state: &mut Self, handle: &workspace_handle::ExtWorkspaceHandleV1, event: workspace_handle::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(&id) = state.workspace_ids.get(&handle.id()) else { return };
        let m = &mut state.model;
        match event {
            workspace_handle::Event::Id { id: stable } => m.workspace_stable_id(id, &stable),
            workspace_handle::Event::Name { name } => m.workspace_name(id, &name),
            workspace_handle::Event::Coordinates { coordinates } => m.workspace_coordinates(id, &coordinates),
            workspace_handle::Event::State { state: bits_ } => m.workspace_state(id, bits(bits_)),
            workspace_handle::Event::Capabilities { capabilities } => m.workspace_capabilities(id, bits(capabilities)),
            workspace_handle::Event::Removed => {
                m.workspace_removed(id);
                state.workspace_ids.remove(&handle.id());
                if let Some(handle) = state.workspaces.remove(&id) {
                    handle.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<toplevel_list::ExtForeignToplevelListV1, ()> for State {
    fn event(state: &mut Self, _: &toplevel_list::ExtForeignToplevelListV1, event: toplevel_list::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let toplevel_list::Event::Toplevel { toplevel } = event {
            if state.listed_limit.is_some_and(|limit| state.listed.len() >= limit) {
                toplevel.destroy();
                return;
            }
            let key = state.model.listed_new();
            state.listed_ids.insert(toplevel.id(), key);
            state.listed.insert(key, toplevel);
        }
    }
    event_created_child!(State, toplevel_list::ExtForeignToplevelListV1, [
        toplevel_list::EVT_TOPLEVEL_OPCODE => (listed_handle::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<listed_handle::ExtForeignToplevelHandleV1, ()> for State {
    fn event(state: &mut Self, handle: &listed_handle::ExtForeignToplevelHandleV1, event: listed_handle::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(&key) = state.listed_ids.get(&handle.id()) else { return };
        match event {
            listed_handle::Event::Title { title } => state.model.listed_title(key, &title),
            listed_handle::Event::AppId { app_id } => state.model.listed_app_id(key, &app_id),
            listed_handle::Event::Identifier { identifier } => state.model.listed_identifier(key, &identifier),
            listed_handle::Event::Done => state.model.listed_done(key),
            listed_handle::Event::Closed => {
                state.model.listed_closed(key);
                if !state.keep_closed_listed {
                    state.model.listed_forget(key);
                    state.listed_ids.remove(&handle.id());
                    if let Some(handle) = state.listed.remove(&key) {
                        handle.destroy();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<copy_session::ExtImageCopyCaptureSessionV1, CaptureId> for State {
    fn event(state: &mut Self, _: &copy_session::ExtImageCopyCaptureSessionV1, event: copy_session::Event, id: &CaptureId, _: &Connection, _: &QueueHandle<Self>) {
        let id = *id;
        match event {
            copy_session::Event::BufferSize { width, height } => {
                if let Some(c) = state.captures.get_mut(&id) {
                    c.width = width;
                    c.height = height;
                }
            }
            copy_session::Event::ShmFormat { format } => {
                if let Some(c) = state.captures.get_mut(&id) {
                    c.format = prefer_format(c.format, bits(format));
                }
            }
            copy_session::Event::Done => state.constraints_done(id),
            copy_session::Event::Stopped => state.finish_capture(id, Err(CaptureError::Stopped)),
            _ => {}
        }
    }
}

impl Dispatch<copy_frame::ExtImageCopyCaptureFrameV1, CaptureId> for State {
    fn event(state: &mut Self, _: &copy_frame::ExtImageCopyCaptureFrameV1, event: copy_frame::Event, id: &CaptureId, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            copy_frame::Event::Transform { transform } => {
                if let Some(c) = state.captures.get_mut(id) {
                    c.transform = bits(transform);
                }
            }
            copy_frame::Event::Ready => state.finish_capture(*id, Ok(())),
            copy_frame::Event::Failed { reason } => state.finish_capture(*id, Err(CaptureError::Failed(bits(reason)))),
            _ => {}
        }
    }
}

// ---- Client --------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConnectError {
    /// No `WAYLAND_DISPLAY` socket (or `XDG_RUNTIME_DIR`) to connect to.
    NoCompositor(String),
    Wayland(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCompositor(s) | Self::Wayland(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for ConnectError {}

/// Connect like `wl_display_connect(NULL)`: `WAYLAND_SOCKET`, else
/// `WAYLAND_DISPLAY` (default `wayland-0`), relative to `XDG_RUNTIME_DIR`
/// unless absolute.
pub fn connect() -> Result<Connection, ConnectError> {
    if std::env::var_os("WAYLAND_SOCKET").is_some() {
        return Connection::connect_to_env().map_err(|e| ConnectError::Wayland(e.to_string()));
    }
    let name = std::env::var_os("WAYLAND_DISPLAY").filter(|n| !n.is_empty()).unwrap_or_else(|| "wayland-0".into());
    let name = std::path::PathBuf::from(name);
    let path = if name.is_absolute() {
        name
    } else {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())
            .ok_or_else(|| ConnectError::NoCompositor("XDG_RUNTIME_DIR is not set".into()))?;
        std::path::PathBuf::from(runtime).join(name)
    };
    let stream = std::os::unix::net::UnixStream::connect(&path)
        .map_err(|e| ConnectError::NoCompositor(format!("{}: {e}", path.display())))?;
    Connection::from_socket(stream).map_err(|e| ConnectError::Wayland(e.to_string()))
}

/// A connection, its event queue and the [`State`]. Derefs to [`State`], so
/// requests (`activate`, `capture_listed`, …) and `model` are reachable directly.
/// Requests are buffered: call [`Client::flush`] (the GLib source flushes itself).
pub struct Client {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl std::ops::Deref for Client {
    type Target = State;
    fn deref(&self) -> &State { &self.state }
}
impl std::ops::DerefMut for Client {
    fn deref_mut(&mut self) -> &mut State { &mut self.state }
}

impl Client {
    /// Connect to the compositor of the environment ([`connect`]).
    pub fn connect(protocols: Protocols) -> Result<Self, ConnectError> {
        Ok(Self::new(connect()?, protocols))
    }

    /// Bind on an existing connection (e.g. GTK's, see `Client::for_gdk_display`).
    /// Objects get their own event queue, so the owner's dispatching is unaffected.
    pub fn new(conn: Connection, protocols: Protocols) -> Self {
        let queue = conn.new_event_queue();
        let mut state = State::new(&conn, queue.handle(), protocols);
        state.model.workspaces_published = true;
        let _ = conn.flush();
        Self { conn, queue, state }
    }

    pub fn connection(&self) -> &Connection { &self.conn }
    pub fn fd(&self) -> RawFd { self.conn.backend().poll_fd().as_raw_fd() }
    pub fn flush(&self) -> Result<(), WaylandError> {
        match self.conn.flush() {
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            other => other,
        }
    }

    /// Blocking roundtrip: every event the compositor sent before is handled.
    pub fn roundtrip(&mut self) -> Result<usize, DispatchError> { self.queue.roundtrip(&mut self.state) }
    /// Handle events already read from the socket (no I/O).
    pub fn dispatch_pending(&mut self) -> Result<usize, DispatchError> { self.queue.dispatch_pending(&mut self.state) }

    /// Read whatever the socket holds without blocking, then dispatch.
    pub fn read_and_dispatch(&mut self) -> Result<usize, DispatchError> {
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
        }
        self.dispatch_pending()
    }

    /// Flush, wait up to `timeout` (None = forever) for events, dispatch them.
    /// Returns whether anything was dispatched.
    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<bool, DispatchError> {
        if self.dispatch_pending()? > 0 {
            return Ok(true);
        }
        self.flush()?;
        let Some(guard) = self.queue.prepare_read() else { return Ok(self.dispatch_pending()? > 0) };
        let mut fds = [libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        if ready > 0 && fds[0].revents & libc::POLLIN != 0 {
            match guard.read() {
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
        } else if ready > 0 && fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(WaylandError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset)).into());
        } else {
            drop(guard);
        }
        Ok(self.dispatch_pending()? > 0)
    }

    /// Dispatch until `done` holds or `timeout` passes. Returns `done()`.
    pub fn wait_until(&mut self, timeout: Duration, mut done: impl FnMut(&mut Client) -> bool) -> Result<bool, DispatchError> {
        let deadline = Instant::now() + timeout;
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(false);
            }
            self.wait(Some(left.min(Duration::from_millis(50))))?;
        }
        Ok(true)
    }

    /// Model events then finished captures, in order.
    pub fn take_events(&mut self) -> Vec<Event> {
        let mut events: Vec<_> = self.state.model.take_events().into_iter().map(Event::Changed).collect();
        events.extend(self.state.take_captured().into_iter().map(|(id, r)| Event::Captured(id, r)));
        events
    }

    #[cfg(feature = "glib")]
    pub(crate) fn has_events(&self) -> bool { self.state.model.has_events() || !self.state.captured.is_empty() }

    #[cfg(feature = "glib")]
    /// Dispatch everything queued (inner libwayland queue included) and
    /// register `cx`'s waker for events other readers route here.
    pub(crate) fn poll_pending(&mut self, cx: &mut std::task::Context<'_>) -> Result<(), DispatchError> {
        match self.queue.poll_dispatch_pending(cx, &mut self.state) {
            std::task::Poll::Ready(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }
    #[cfg(feature = "glib")]
    pub(crate) fn read_socket(&mut self) -> Result<(), WaylandError> {
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
                Ok(_) => {}
            }
        }
        Ok(())
    }
}
