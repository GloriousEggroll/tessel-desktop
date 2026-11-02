//! Minimal hand-written bindings for libpolkit-gobject-1 / libpolkit-agent-1
//! (polkit 127 ABI). Only symbols the agent needs; no headers are used, and the
//! libraries are linked by their versioned SONAME so building needs no
//! `polkit-devel` (only the runtime `polkit-libs`).

#![allow(non_camel_case_types)]

use gio::ffi::{GAsyncReadyCallback, GAsyncResult, GCancellable};
use glib::ffi::{gboolean, gchar, gint, gpointer, GError, GList, GType};
use glib::gobject_ffi::GObjectClass;
use std::os::raw::c_void;

pub type PolkitAgentListener = c_void;
pub type PolkitIdentity = c_void;
pub type PolkitSubject = c_void;
pub type PolkitDetails = c_void;
pub type PolkitAgentSession = c_void;

pub const POLKIT_AGENT_REGISTER_FLAGS_NONE: u32 = 0;

/// `struct _PolkitAgentListenerClass` (polkitagentlistener.h).
#[repr(C)]
pub struct PolkitAgentListenerClass {
    pub parent_class: GObjectClass,
    pub initiate_authentication: Option<
        unsafe extern "C" fn(*mut PolkitAgentListener, *const gchar, *const gchar, *const gchar, *mut PolkitDetails,
                             *const gchar, *mut GList, *mut GCancellable, GAsyncReadyCallback, gpointer),
    >,
    pub initiate_authentication_finish:
        Option<unsafe extern "C" fn(*mut PolkitAgentListener, *mut GAsyncResult, *mut *mut GError) -> gboolean>,
    pub reserved: [gpointer; 8],
}

#[link(name = "libpolkit-gobject-1.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    pub fn polkit_identity_to_string(identity: *mut PolkitIdentity) -> *mut gchar;
    pub fn polkit_identity_from_string(text: *const gchar, error: *mut *mut GError) -> *mut PolkitIdentity;
    pub fn polkit_unix_user_new(uid: gint) -> *mut PolkitIdentity;
    pub fn polkit_unix_session_new_for_process_sync(pid: gint, cancellable: *mut GCancellable,
                                                     error: *mut *mut GError) -> *mut PolkitSubject;
    pub fn polkit_details_new() -> *mut PolkitDetails;
}

#[link(name = "libpolkit-agent-1.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    pub fn polkit_agent_listener_get_type() -> GType;
    pub fn polkit_agent_listener_register(listener: *mut PolkitAgentListener, flags: u32, subject: *mut PolkitSubject,
                                          object_path: *const gchar, cancellable: *mut GCancellable,
                                          error: *mut *mut GError) -> gpointer;
    pub fn polkit_agent_listener_unregister(handle: gpointer);
    pub fn polkit_agent_listener_initiate_authentication(
        listener: *mut PolkitAgentListener, action_id: *const gchar, message: *const gchar, icon_name: *const gchar,
        details: *mut PolkitDetails, cookie: *const gchar, identities: *mut GList, cancellable: *mut GCancellable,
        callback: GAsyncReadyCallback, user_data: gpointer);
    pub fn polkit_agent_listener_initiate_authentication_finish(listener: *mut PolkitAgentListener,
                                                                result: *mut GAsyncResult,
                                                                error: *mut *mut GError) -> gboolean;
    pub fn polkit_agent_session_new(identity: *mut PolkitIdentity, cookie: *const gchar) -> *mut PolkitAgentSession;
    pub fn polkit_agent_session_initiate(session: *mut PolkitAgentSession);
    pub fn polkit_agent_session_response(session: *mut PolkitAgentSession, response: *const gchar);
    pub fn polkit_agent_session_cancel(session: *mut PolkitAgentSession);
}
