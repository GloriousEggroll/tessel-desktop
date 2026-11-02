//! Single-instance conventions shared by the GApplication-based components.
//!
//! Every component is a unique `GApplication` with `HANDLES_COMMAND_LINE`: a
//! second launch forwards its argv to the primary over D-Bus and exits. Status
//! queries and stale tray clicks pass `launcher = true` (G_APPLICATION_IS_LAUNCHER)
//! so they never become a new primary/daemon; only the explicit `--background`
//! start may own the name.

use gio::ApplicationFlags;

/// `HANDLES_COMMAND_LINE`, plus `IS_LAUNCHER` when this process must not become primary.
pub fn application_flags(launcher: bool) -> ApplicationFlags {
    let mut flags = ApplicationFlags::HANDLES_COMMAND_LINE;
    if launcher {
        flags |= ApplicationFlags::IS_LAUNCHER;
    }
    flags
}

/// The object path GApplication exports for an id (`org.foo.Bar-x` → `/org/foo/Bar_x`).
pub fn object_path(application_id: &str) -> String {
    format!("/{}", application_id.replace('.', "/").replace('-', "_"))
}

/// Whether some process currently owns `name` on the session bus (bounded, no auto-start).
pub fn name_has_owner(connection: &gio::DBusConnection, name: &str) -> bool {
    connection
        .call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner",
                   Some(&(name,).into()), Some(glib::VariantTy::new("(b)").unwrap()),
                   gio::DBusCallFlags::NO_AUTO_START, 500, None::<&gio::Cancellable>)
        .ok()
        .and_then(|reply| reply.get::<(bool,)>())
        .is_some_and(|(owned,)| owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_object_path() {
        assert_eq!(application_flags(false), ApplicationFlags::HANDLES_COMMAND_LINE);
        assert!(application_flags(true).contains(ApplicationFlags::IS_LAUNCHER));
        assert_eq!(object_path("org.tessel.Keep-Awake"), "/org/tessel/Keep_Awake");
    }
}
