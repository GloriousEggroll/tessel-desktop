//! Protocol-independent state tracking for outputs, foreign toplevels,
//! workspaces and the ext-foreign-toplevel list.
//!
//! The Wayland glue (`client`) translates protocol events into calls on
//! [`Model`]; unit tests drive the same methods with mocked events. Ordering
//! and emission rules reproduce the C helpers exactly (tessel-dock/toplevel_helper.c,
//! tessel-dock/workspace_helper.c), because the compat binaries print from here:
//! windows and outputs are kept newest-first, membership lists newest-first,
//! workspaces and groups oldest-first.

/// A `wl_registry` global name (the `id` of helper `output` events).
pub type GlobalName = u32;
/// Window id, unique for one [`Model`] (= one connection), never reused.
pub type WindowId = u64;
/// Workspace / workspace-group id (shared counter, as in workspace_helper.c).
pub type WorkspaceId = u64;
/// Key of an `ext_foreign_toplevel_handle_v1` (capture source).
pub type HandleKey = u64;

/// Longest string kept from the compositor, in bytes (C `strndup(…, 16384)`).
pub const STRING_LIMIT: usize = 16384;

/// `zwlr_foreign_toplevel_handle_v1.state` values.
pub mod toplevel_state {
    pub const MAXIMIZED: u32 = 0;
    pub const MINIMIZED: u32 = 1;
    pub const ACTIVATED: u32 = 2;
    pub const FULLSCREEN: u32 = 3;
}

/// `ext_workspace_handle_v1` state / capability bits.
pub mod workspace_bits {
    pub const ACTIVE: u32 = 1;
    pub const URGENT: u32 = 2;
    pub const HIDDEN: u32 = 4;
    pub const CAN_ACTIVATE: u32 = 1;
    pub const CAN_DEACTIVATE: u32 = 2;
    pub const CAN_REMOVE: u32 = 4;
    pub const CAN_ASSIGN: u32 = 8;
}

/// `wl_output.transform` values that swap width and height.
fn transform_swaps(transform: i32) -> bool {
    matches!(transform, 1 | 3 | 5 | 7)
}

/// Copy at most [`STRING_LIMIT`] bytes; a split UTF-8 sequence becomes U+FFFD,
/// which is what Python's `decode("utf-8", "replace")` made of the C bytes.
pub fn limit(value: &str) -> String {
    if value.len() <= STRING_LIMIT {
        return value.to_owned();
    }
    String::from_utf8_lossy(&value.as_bytes()[..STRING_LIMIT]).into_owned()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub global: GlobalName,
    pub name: Option<String>,
    pub description: Option<String>,
    pub x: i32,
    pub y: i32,
    /// Logical size from xdg-output (valid when `logical`).
    pub width: i32,
    pub height: i32,
    pub pixel_width: i32,
    pub pixel_height: i32,
    pub scale: i32,
    pub transform: i32,
    /// True once xdg-output reported a logical position or size.
    pub logical: bool,
}

/// Logical output geometry as the helper's `output` event reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Output {
    /// Logical geometry: xdg-output's size, else pixels / scale with the
    /// 90°/270° transforms swapping width and height (toplevel_helper.c).
    pub fn geometry(&self) -> Geometry {
        let (mut width, mut height) = (self.width, self.height);
        if !self.logical {
            let scale = self.scale.max(1);
            width = self.pixel_width / scale;
            height = self.pixel_height / scale;
            if transform_swaps(self.transform) {
                std::mem::swap(&mut width, &mut height);
            }
        }
        Geometry { x: self.x, y: self.y, width, height }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub id: WindowId,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// Parent window id, 0 for none.
    pub parent: WindowId,
    /// Output globals the window is on, newest first.
    pub outputs: Vec<GlobalName>,
    /// True after the first `done`; unpublished windows are never reported.
    pub published: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceGroup {
    pub id: WorkspaceId,
    /// Output globals, newest first.
    pub outputs: Vec<GlobalName>,
    pub capabilities: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Compositor-provided stable id (`ext_workspace_handle_v1.id`), if any.
    pub stable_id: Option<String>,
    pub name: String,
    pub state: u32,
    pub capabilities: u32,
    pub coordinates: Vec<u32>,
    /// Group id, 0 for none.
    pub group: WorkspaceId,
}

impl Workspace {
    pub fn active(&self) -> bool { self.state & workspace_bits::ACTIVE != 0 }
    pub fn urgent(&self) -> bool { self.state & workspace_bits::URGENT != 0 }
    pub fn hidden(&self) -> bool { self.state & workspace_bits::HIDDEN != 0 }
    pub fn can_activate(&self) -> bool { self.capabilities & workspace_bits::CAN_ACTIVATE != 0 }
}

/// One `ext_foreign_toplevel_handle_v1` (identity for image capture).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListedToplevel {
    pub key: HandleKey,
    pub identifier: Option<String>,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub closed: bool,
    pub done: bool,
}

/// What changed. Emitted in protocol order; the compat binaries print from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An output with a name finished an update (`wl_output.done` or
    /// `zxdg_output_v1.done`). Followed by a `Window` for every published window.
    Output(GlobalName),
    OutputRemoved(GlobalName),
    /// A published window changed (or was re-reported).
    Window(WindowId),
    WindowClosed(WindowId),
    /// `ext_workspace_manager_v1.done`: the workspace state is consistent.
    Workspaces,
    /// An ext-foreign-toplevel handle finished an update.
    Listed(HandleKey),
    ListedClosed(HandleKey),
    /// Protocol trouble; `fatal` ones end the helper session.
    Error { message: String, fatal: bool },
}

#[derive(Debug, Default)]
pub struct Model {
    outputs: Vec<Output>,
    windows: Vec<Window>,
    groups: Vec<WorkspaceGroup>,
    workspaces: Vec<Workspace>,
    listed: Vec<ListedToplevel>,
    next_window: WindowId,
    next_workspace: WorkspaceId,
    next_listed: HandleKey,
    /// Workspace snapshots are reported only once published (the helper
    /// publishes after its startup roundtrips; in-process clients at once).
    pub workspaces_published: bool,
    /// When set, each event is also rendered to a helper JSON line at the
    /// moment it happens (the C helpers print the state as of the event).
    pub render: Option<Render>,
    events: Vec<Event>,
    lines: Vec<String>,
}

/// Which helper's line protocol [`Model::take_lines`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render {
    /// `tessel-dock-toplevel`: output, output-removed, window, closed, error.
    Toplevel,
    /// `tessel-dock-workspaces`: state, error.
    Workspaces,
}

impl Model {
    pub fn new() -> Self { Self::default() }

    /// Drain the events produced since the last call.
    pub fn take_events(&mut self) -> Vec<Event> { std::mem::take(&mut self.events) }
    pub fn has_events(&self) -> bool { !self.events.is_empty() }
    /// Drain the rendered helper lines (see [`Model::render`]).
    pub fn take_lines(&mut self) -> Vec<String> { std::mem::take(&mut self.lines) }
    fn push(&mut self, event: Event) {
        if let Some(render) = self.render {
            let line = match (render, &event) {
                (_, Event::Error { message, .. }) => Some(error_json(message)),
                (Render::Toplevel, Event::Output(g)) => self.output_json(*g),
                (Render::Toplevel, Event::OutputRemoved(g)) => Some(format!("{{\"event\":\"output-removed\",\"id\":{g}}}")),
                (Render::Toplevel, Event::Window(id)) => self.window_json(*id),
                (Render::Toplevel, Event::WindowClosed(id)) => Some(format!("{{\"event\":\"closed\",\"id\":\"{id}\"}}")),
                (Render::Workspaces, Event::Workspaces) => Some(self.workspaces_json()),
                _ => None,
            };
            self.lines.extend(line);
        }
        self.events.push(event)
    }
    pub fn error(&mut self, message: &str, fatal: bool) {
        self.push(Event::Error { message: message.to_owned(), fatal })
    }

    // ---- queries ----------------------------------------------------------
    /// Outputs, newest first.
    pub fn outputs(&self) -> &[Output] { &self.outputs }
    pub fn output(&self, global: GlobalName) -> Option<&Output> { self.outputs.iter().find(|o| o.global == global) }
    pub fn output_by_name(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name.as_deref() == Some(name))
    }
    /// All windows (published or not), newest first.
    pub fn all_windows(&self) -> &[Window] { &self.windows }
    /// Published windows, newest first.
    pub fn windows(&self) -> impl Iterator<Item = &Window> { self.windows.iter().filter(|w| w.published) }
    pub fn window(&self, id: WindowId) -> Option<&Window> { self.windows.iter().find(|w| w.id == id) }
    /// Names of the window's (named) outputs, in helper order (newest first).
    pub fn window_outputs(&self, window: &Window) -> Vec<&str> { self.names(&window.outputs) }
    pub fn groups(&self) -> &[WorkspaceGroup] { &self.groups }
    pub fn group(&self, id: WorkspaceId) -> Option<&WorkspaceGroup> { self.groups.iter().find(|g| g.id == id) }
    pub fn group_outputs(&self, group: &WorkspaceGroup) -> Vec<&str> { self.names(&group.outputs) }
    /// Workspaces in creation order.
    pub fn workspaces(&self) -> &[Workspace] { &self.workspaces }
    pub fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> { self.workspaces.iter().find(|w| w.id == id) }
    /// Output names of the workspace's group (empty without a group).
    pub fn workspace_outputs(&self, workspace: &Workspace) -> Vec<&str> {
        self.group(workspace.group).map(|g| self.group_outputs(g)).unwrap_or_default()
    }
    /// ext-foreign-toplevel handles in creation order.
    pub fn listed(&self) -> &[ListedToplevel] { &self.listed }
    pub fn listed_toplevel(&self, key: HandleKey) -> Option<&ListedToplevel> { self.listed.iter().find(|t| t.key == key) }

    fn names<'a>(&'a self, globals: &[GlobalName]) -> Vec<&'a str> {
        globals.iter().filter_map(|g| self.output(*g).and_then(|o| o.name.as_deref())).collect()
    }
    fn output_mut(&mut self, global: GlobalName) -> Option<&mut Output> { self.outputs.iter_mut().find(|o| o.global == global) }
    fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> { self.windows.iter_mut().find(|w| w.id == id) }
    fn group_mut(&mut self, id: WorkspaceId) -> Option<&mut WorkspaceGroup> { self.groups.iter_mut().find(|g| g.id == id) }
    fn workspace_mut(&mut self, id: WorkspaceId) -> Option<&mut Workspace> { self.workspaces.iter_mut().find(|w| w.id == id) }
    fn listed_mut(&mut self, key: HandleKey) -> Option<&mut ListedToplevel> { self.listed.iter_mut().find(|t| t.key == key) }

    fn emit_window(&mut self, id: WindowId) {
        if self.window(id).is_some_and(|w| w.published) {
            self.push(Event::Window(id));
        }
    }
    fn emit_output(&mut self, global: GlobalName) {
        if self.output(global).is_none_or(|o| o.name.is_none()) {
            return;
        }
        self.push(Event::Output(global));
        let ids: Vec<_> = self.windows.iter().filter(|w| w.published).map(|w| w.id).collect();
        for id in ids {
            self.push(Event::Window(id));
        }
    }

    // ---- wl_output / zxdg_output_v1 ----------------------------------------
    pub fn output_added(&mut self, global: GlobalName) {
        if self.output(global).is_none() {
            self.outputs.insert(0, Output { global, scale: 1, ..Default::default() });
        }
    }
    pub fn output_geometry(&mut self, global: GlobalName, x: i32, y: i32, transform: i32) {
        if let Some(o) = self.output_mut(global) {
            if !o.logical {
                o.x = x;
                o.y = y;
            }
            o.transform = transform;
        }
    }
    pub fn output_mode(&mut self, global: GlobalName, current: bool, width: i32, height: i32) {
        if let Some(o) = self.output_mut(global).filter(|_| current) {
            o.pixel_width = width;
            o.pixel_height = height;
        }
    }
    pub fn output_scale(&mut self, global: GlobalName, scale: i32) {
        if let Some(o) = self.output_mut(global) {
            o.scale = if scale > 0 { scale } else { 1 };
        }
    }
    pub fn output_name(&mut self, global: GlobalName, name: &str) {
        if let Some(o) = self.output_mut(global) {
            o.name = Some(limit(name));
        }
    }
    pub fn output_description(&mut self, global: GlobalName, description: &str) {
        if let Some(o) = self.output_mut(global) {
            o.description = Some(limit(description));
        }
    }
    pub fn output_done(&mut self, global: GlobalName) { self.emit_output(global) }
    pub fn logical_position(&mut self, global: GlobalName, x: i32, y: i32) {
        if let Some(o) = self.output_mut(global) {
            o.x = x;
            o.y = y;
            o.logical = true;
        }
    }
    pub fn logical_size(&mut self, global: GlobalName, width: i32, height: i32) {
        if let Some(o) = self.output_mut(global) {
            o.width = width;
            o.height = height;
            o.logical = true;
        }
    }
    pub fn logical_done(&mut self, global: GlobalName) { self.emit_output(global) }

    /// The output global went away: `OutputRemoved`, then every published
    /// window is re-reported without it; workspace groups drop it silently.
    pub fn output_removed(&mut self, global: GlobalName) -> bool {
        let Some(index) = self.outputs.iter().position(|o| o.global == global) else { return false };
        self.push(Event::OutputRemoved(global));
        let ids: Vec<_> = self.windows.iter().map(|w| w.id).collect();
        for id in ids {
            if let Some(w) = self.window_mut(id) {
                w.outputs.retain(|g| *g != global);
            }
            self.emit_window(id);
        }
        for group in &mut self.groups {
            group.outputs.retain(|g| *g != global);
        }
        self.outputs.remove(index);
        true
    }

    // ---- zwlr_foreign_toplevel_handle_v1 -----------------------------------
    pub fn toplevel_new(&mut self) -> WindowId {
        self.next_window += 1;
        let id = self.next_window;
        self.windows.insert(0, Window { id, ..Default::default() });
        id
    }
    pub fn toplevel_title(&mut self, id: WindowId, title: &str) {
        if let Some(w) = self.window_mut(id) {
            w.title = limit(title);
        }
    }
    pub fn toplevel_app_id(&mut self, id: WindowId, app_id: &str) {
        if let Some(w) = self.window_mut(id) {
            w.app_id = limit(app_id);
        }
    }
    /// Unknown outputs (not yet bound or already removed) are ignored.
    pub fn toplevel_output_enter(&mut self, id: WindowId, global: GlobalName) {
        if self.output(global).is_none() {
            return;
        }
        if let Some(w) = self.window_mut(id).filter(|w| !w.outputs.contains(&global)) {
            w.outputs.insert(0, global);
        }
    }
    pub fn toplevel_output_leave(&mut self, id: WindowId, global: GlobalName) {
        if let Some(w) = self.window_mut(id) {
            w.outputs.retain(|g| *g != global);
        }
    }
    pub fn toplevel_state(&mut self, id: WindowId, states: &[u32]) {
        if let Some(w) = self.window_mut(id) {
            w.activated = states.contains(&toplevel_state::ACTIVATED);
            w.fullscreen = states.contains(&toplevel_state::FULLSCREEN);
            w.minimized = states.contains(&toplevel_state::MINIMIZED);
            w.maximized = states.contains(&toplevel_state::MAXIMIZED);
        }
    }
    /// `parent` is the parent's id if it is a known window (else none).
    pub fn toplevel_parent(&mut self, id: WindowId, parent: Option<WindowId>) {
        let parent = parent.filter(|p| self.window(*p).is_some()).unwrap_or(0);
        if let Some(w) = self.window_mut(id) {
            w.parent = parent;
        }
    }
    pub fn toplevel_done(&mut self, id: WindowId) {
        if let Some(w) = self.window_mut(id) {
            w.published = true;
            self.push(Event::Window(id));
        }
    }
    /// `WindowClosed`, then children lose their parent and are re-reported.
    pub fn toplevel_closed(&mut self, id: WindowId) {
        if self.window(id).is_none() {
            return;
        }
        self.push(Event::WindowClosed(id));
        let children: Vec<_> = self.windows.iter().filter(|w| w.parent == id).map(|w| w.id).collect();
        for child in children {
            if let Some(w) = self.window_mut(child) {
                w.parent = 0;
            }
            self.emit_window(child);
        }
        self.windows.retain(|w| w.id != id);
    }

    // ---- ext_workspace_v1 ---------------------------------------------------
    pub fn group_new(&mut self) -> WorkspaceId {
        self.next_workspace += 1;
        let id = self.next_workspace;
        self.groups.push(WorkspaceGroup { id, ..Default::default() });
        id
    }
    pub fn group_capabilities(&mut self, id: WorkspaceId, capabilities: u32) {
        if let Some(g) = self.group_mut(id) {
            g.capabilities = capabilities;
        }
    }
    pub fn group_output_enter(&mut self, id: WorkspaceId, global: GlobalName) {
        if self.output(global).is_none() {
            return;
        }
        if let Some(g) = self.group_mut(id).filter(|g| !g.outputs.contains(&global)) {
            g.outputs.insert(0, global);
        }
    }
    pub fn group_output_leave(&mut self, id: WorkspaceId, global: GlobalName) {
        if let Some(g) = self.group_mut(id) {
            g.outputs.retain(|o| *o != global);
        }
    }
    pub fn group_workspace_enter(&mut self, group: WorkspaceId, workspace: WorkspaceId) {
        if let Some(w) = self.workspace_mut(workspace) {
            w.group = group;
        }
    }
    pub fn group_workspace_leave(&mut self, group: WorkspaceId, workspace: WorkspaceId) {
        if let Some(w) = self.workspace_mut(workspace).filter(|w| w.group == group) {
            w.group = 0;
        }
    }
    pub fn group_removed(&mut self, id: WorkspaceId) {
        for w in self.workspaces.iter_mut().filter(|w| w.group == id) {
            w.group = 0;
        }
        self.groups.retain(|g| g.id != id);
    }
    pub fn workspace_new(&mut self) -> WorkspaceId {
        self.next_workspace += 1;
        let id = self.next_workspace;
        self.workspaces.push(Workspace { id, ..Default::default() });
        id
    }
    pub fn workspace_stable_id(&mut self, id: WorkspaceId, stable: &str) {
        if let Some(w) = self.workspace_mut(id) {
            w.stable_id = Some(limit(stable));
        }
    }
    pub fn workspace_name(&mut self, id: WorkspaceId, name: &str) {
        if let Some(w) = self.workspace_mut(id) {
            w.name = limit(name);
        }
    }
    /// Raw `array` of u32 coordinates. Malformed (size not a multiple of 4 or
    /// over 1024 bytes) is a fatal error, as in the helper.
    pub fn workspace_coordinates(&mut self, id: WorkspaceId, bytes: &[u8]) {
        if bytes.len() % 4 != 0 || bytes.len() > 1024 {
            self.error("Invalid workspace coordinates.", true);
            return;
        }
        if let Some(w) = self.workspace_mut(id) {
            w.coordinates = bytes.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect();
        }
    }
    pub fn workspace_state(&mut self, id: WorkspaceId, state: u32) {
        if let Some(w) = self.workspace_mut(id) {
            w.state = state;
        }
    }
    pub fn workspace_capabilities(&mut self, id: WorkspaceId, capabilities: u32) {
        if let Some(w) = self.workspace_mut(id) {
            w.capabilities = capabilities;
        }
    }
    pub fn workspace_removed(&mut self, id: WorkspaceId) { self.workspaces.retain(|w| w.id != id) }
    pub fn workspaces_done(&mut self) {
        if self.workspaces_published {
            self.push(Event::Workspaces);
        }
    }
    /// Mark the workspace state published and report it once.
    pub fn publish_workspaces(&mut self) {
        self.workspaces_published = true;
        self.push(Event::Workspaces);
    }

    // ---- ext_foreign_toplevel_list_v1 ------------------------------------------
    pub fn listed_new(&mut self) -> HandleKey {
        self.next_listed += 1;
        let key = self.next_listed;
        self.listed.push(ListedToplevel { key, ..Default::default() });
        key
    }
    pub fn listed_title(&mut self, key: HandleKey, title: &str) {
        if let Some(t) = self.listed_mut(key) {
            t.title = Some(title.to_owned());
        }
    }
    pub fn listed_app_id(&mut self, key: HandleKey, app_id: &str) {
        if let Some(t) = self.listed_mut(key) {
            t.app_id = Some(app_id.to_owned());
        }
    }
    pub fn listed_identifier(&mut self, key: HandleKey, identifier: &str) {
        if let Some(t) = self.listed_mut(key) {
            t.identifier = Some(identifier.to_owned());
        }
    }
    pub fn listed_done(&mut self, key: HandleKey) {
        if let Some(t) = self.listed_mut(key) {
            t.done = true;
            self.push(Event::Listed(key));
        }
    }
    /// Closed handles stay listed (flagged) until [`Model::listed_forget`].
    pub fn listed_closed(&mut self, key: HandleKey) {
        if let Some(t) = self.listed_mut(key) {
            t.closed = true;
            self.push(Event::ListedClosed(key));
        }
    }
    pub fn listed_forget(&mut self, key: HandleKey) { self.listed.retain(|t| t.key != key) }
}

// ---- JSON text exactly as the C helpers print it -------------------------------

/// `"…"` with only `"` and `\` escaped and bytes < 0x20 as `\u00XX`;
/// everything else (DEL, non-ASCII UTF-8) is copied verbatim.
pub fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn boolean(value: bool) -> &'static str { if value { "true" } else { "false" } }

fn string_list(out: &mut String, names: &[&str]) {
    out.push('[');
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_string(out, name);
    }
    out.push(']');
}

impl Model {
    /// `tessel-dock-toplevel` line for a window event.
    pub fn window_json(&self, id: WindowId) -> Option<String> {
        let w = self.window(id).filter(|w| w.published)?;
        let mut out = format!("{{\"event\":\"window\",\"id\":\"{}\",\"app_id\":", w.id);
        json_string(&mut out, &w.app_id);
        out.push_str(",\"title\":");
        json_string(&mut out, &w.title);
        out.push_str(&format!(
            ",\"activated\":{},\"fullscreen\":{},\"minimized\":{},\"maximized\":{},\"parent\":\"",
            boolean(w.activated), boolean(w.fullscreen), boolean(w.minimized), boolean(w.maximized)));
        if w.parent != 0 {
            out.push_str(&w.parent.to_string());
        }
        out.push_str("\",\"outputs\":");
        string_list(&mut out, &self.window_outputs(w));
        out.push('}');
        Some(out)
    }

    /// `tessel-dock-toplevel` line for an output event.
    pub fn output_json(&self, global: GlobalName) -> Option<String> {
        let o = self.output(global)?;
        let name = o.name.as_deref()?;
        let g = o.geometry();
        let mut out = format!("{{\"event\":\"output\",\"id\":{},\"name\":", o.global);
        json_string(&mut out, name);
        out.push_str(",\"description\":");
        json_string(&mut out, o.description.as_deref().unwrap_or(""));
        out.push_str(&format!(",\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"scale\":{}}}", g.x, g.y, g.width, g.height, o.scale));
        Some(out)
    }

    /// `tessel-dock-workspaces` `state` line.
    pub fn workspaces_json(&self) -> String {
        let mut out = String::from("{\"event\":\"state\",\"workspaces\":[");
        for (i, w) in self.workspaces.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"id\":\"{}\",\"name\":", w.id));
            json_string(&mut out, &w.name);
            out.push_str(&format!(",\"active\":{},\"urgent\":{},\"hidden\":{},\"can_activate\":{},\"group_id\":\"",
                boolean(w.active()), boolean(w.urgent()), boolean(w.hidden()), boolean(w.can_activate())));
            // A workspace whose group is gone has none (group_removed clears it).
            if w.group != 0 && self.group(w.group).is_some() {
                out.push_str(&w.group.to_string());
            }
            out.push_str("\",\"coordinates\":[");
            out.push_str(&w.coordinates.iter().map(u32::to_string).collect::<Vec<_>>().join(","));
            out.push_str("],\"outputs\":");
            string_list(&mut out, &self.workspace_outputs(w));
            out.push('}');
        }
        out.push_str("],\"groups\":[");
        for (i, g) in self.groups.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"id\":\"{}\",\"outputs\":", g.id));
            string_list(&mut out, &self.group_outputs(g));
            out.push('}');
        }
        out.push_str("]}");
        out
    }
}

/// `{"event":"error","message":…}`
pub fn error_json(message: &str) -> String {
    let mut out = String::from("{\"event\":\"error\",\"message\":");
    json_string(&mut out, message);
    out.push('}');
    out
}
