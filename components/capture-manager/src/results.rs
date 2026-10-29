//! Open saved captures independently of the capture window's state
//! (capture_results.py).

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::{Path, PathBuf};
use suite_core::tr;

pub fn file_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    glib::filename_to_uri(&absolute, None).map(|u| u.to_string()).unwrap_or_default()
}

pub fn saved_notification(title: &str, body: &str, path: &Path, recording: bool) -> gio::Notification {
    let uri = file_uri(path);
    let notification = gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_icon(&gio::ThemedIcon::new("camera-photo-symbolic"));
    let target = uri.to_variant();
    // Our notification service renders the default action as its Open button.
    notification.set_default_action_and_target_value("app.open-saved", Some(&target));
    notification.add_button_with_target_value(&tr!("Open Folder"), "app.reveal-saved", Some(&target));
    if !recording {
        notification.add_button_with_target_value(&tr!("Copy"), "app.copy-saved", Some(&target));
        notification.add_button_with_target_value(&tr!("Edit"), "app.edit-saved", Some(&target));
    }
    notification
}

fn failed(application: &gtk::Application, error: &str) {
    let notification = gio::Notification::new(&tr!("Could not open saved capture"));
    notification.set_body(Some(error));
    notification.set_icon(&gio::ThemedIcon::new("dialog-error-symbolic"));
    application.send_notification(Some("capture-open-error"), &notification);
}

/// Open the file (or reveal it) without showing Capture; holds the app meanwhile.
pub fn open_saved(application: &gtk::Application, uri: &str, reveal: bool) {
    let file = gio::File::for_uri(uri);
    if !file.is_native() {
        failed(application, &tr!("The saved capture must be a local file."));
        return;
    }
    let path = file.path().unwrap_or_default();
    if !path.is_file() {
        failed(application, &tr!("The saved capture no longer exists: {path}", path = path.to_string_lossy().into_owned()));
        return;
    }
    let guard = application.hold();
    if reveal {
        let handler = gio::AppInfo::default_for_type("inode/directory", false);
        if let Some(handler) = handler.filter(|h| h.id().as_deref() == Some("org.waydock.Files.desktop")) {
            // Files owns FileManager1 only while running and has no activation
            // service; its launcher starts/reuses the app and queues selection.
            use std::os::unix::process::CommandExt;
            let result = std::process::Command::new(handler.executable()).args(["--select", uri])
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .process_group(0).spawn();
            match result {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(error) => failed(application, &crate::process::spawn_error(&error, &handler.executable().to_string_lossy())),
            }
            drop(guard);
        } else {
            let launcher = gtk::FileLauncher::new(Some(&file));
            let application = application.clone();
            launcher.open_containing_folder(None::<&gtk::Window>, None::<&gio::Cancellable>, move |result| {
                if let Err(error) = result {
                    failed(&application, error.message());
                }
                drop(guard);
            });
        }
    } else {
        let context = gdk::Display::default().map(|d| d.app_launch_context());
        let application = application.clone();
        gio::AppInfo::launch_default_for_uri_async(uri, context.as_ref(), None::<&gio::Cancellable>, move |result| {
            if let Err(error) = result {
                failed(&application, error.message());
            }
            drop(guard);
        });
    }
}

fn local_file(application: &gtk::Application, uri: &str) -> Option<PathBuf> {
    let file = gio::File::for_uri(uri);
    let path = if file.is_native() { file.path() } else { None };
    match path {
        Some(path) if path.is_file() => Some(path),
        other => {
            let notification = gio::Notification::new(&tr!("Could not open saved capture"));
            let shown = other.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| uri.to_string());
            notification.set_body(Some(&tr!("The saved capture no longer exists: {path}", path = shown)));
            notification.set_icon(&gio::ThemedIcon::new("dialog-error-symbolic"));
            application.send_notification(Some("capture-open-error"), &notification);
            None
        }
    }
}

/// Copy a saved screenshot file (PNG or JPEG) to the clipboard as PNG.
pub fn copy_saved(application: &gtk::Application, uri: &str) {
    let Some(path) = local_file(application, uri) else { return };
    let encoded = std::fs::read(&path).map_err(|e| suite_core::pyerr::os_error_path(&e, &path)).and_then(|data| crate::imaging::to_png(&data));
    match encoded {
        Ok(png) => {
            crate::clipboard::copy_image(application, png, "image/png", None, None);
        }
        Err(error) => {
            let notification = gio::Notification::new(&tr!("Could not copy the screenshot"));
            notification.set_body(Some(&error));
            application.send_notification(Some("capture-open-error"), &notification);
        }
    }
}

pub fn edit_saved(application: &gtk::Application, uri: &str) {
    if let Some(path) = local_file(application, uri) {
        crate::editor::open_editor(application, &path);
    }
}

/// `open-saved`/`reveal-saved` go through `open` (Python: the module-level `open_saved`).
pub fn register_actions(application: &gtk::Application, open: crate::notifications::OpenSaved) {
    for (name, kind) in [("open-saved", 0), ("reveal-saved", 1), ("copy-saved", 2), ("edit-saved", 3)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let weak = application.downgrade();
        let open = open.clone();
        action.connect_activate(move |_, value| {
            let (Some(application), Some(uri)) = (weak.upgrade(), value.and_then(|v| v.str().map(String::from))) else { return };
            match kind {
                0 => open(&application, &uri, false),
                1 => open(&application, &uri, true),
                2 => copy_saved(&application, &uri),
                _ => edit_saved(&application, &uri),
            }
        });
        application.add_action(&action);
    }
}
