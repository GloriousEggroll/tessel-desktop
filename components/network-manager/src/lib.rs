//! Rust port of ~/btmanager/network-manager (netman); see contracts/network-manager.md.
//! NetworkManager is reached over raw D-Bus (gio); libnm is dlopen'ed only for
//! libnm-only connection logic (normalize/verify/need_secrets, editor schema,
//! VPN plugin metadata and import/export).

pub mod backend;
pub mod compat;
pub mod libnm;
pub mod model;
pub mod nm;
pub mod status;
pub mod preferences;
pub mod keyring;
pub mod vpn_auth;
pub mod agent;
pub mod profiles;
pub mod args;
pub mod app;
pub mod gui;
pub mod ui;

#[cfg(test)]
pub(crate) mod testing {
    /// A private thread-default main context for one test (cargo runs tests
    /// on many threads; the global default context belongs to one of them).
    pub struct TestContext(glib::MainContext);

    impl TestContext {
        pub fn new() -> Self {
            let context = glib::MainContext::new();
            unsafe { glib::ffi::g_main_context_push_thread_default(glib::translate::ToGlibPtr::to_glib_none(&context).0) };
            Self(context)
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            unsafe { glib::ffi::g_main_context_pop_thread_default(glib::translate::ToGlibPtr::to_glib_none(&self.0).0) };
        }
    }
}
