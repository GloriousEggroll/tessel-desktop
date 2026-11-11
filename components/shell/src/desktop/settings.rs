//! Wallpaper Settings window (tessel-shell:1149-1482): per monitor an image/video
//! chooser button, a placement dropdown and a colour button; Apply merges
//! into wallpapers.json (indent=2), Close/Escape closes.

use super::config::{self, MODES};
use super::view::{notify, DesktopView};
use gtk::gdk;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use tessel_core::tr;

pub const WALLPAPER_STYLE_CSS: &str = r#"
@define-color wallpaper_surface mix(@theme_base_color, @theme_bg_color, 0.18);
@define-color wallpaper_muted alpha(@theme_fg_color, 0.62);
@define-color wallpaper_line alpha(@theme_fg_color, 0.10);
window.tessel-shell-wallpaper-settings { background-color: @theme_bg_color; color: @theme_fg_color; }
.tessel-shell-wallpaper-settings headerbar {
    background-image: none; background-color: @theme_bg_color;
    border: none; box-shadow: none; min-height: 28px; padding: 8px 14px;
}
.tessel-shell-wallpaper-settings .wallpaper-title { font-size: 18pt; font-weight: 700; }
.tessel-shell-wallpaper-settings .wallpaper-subtitle { color: @wallpaper_muted; font-size: 10pt; }
.tessel-shell-wallpaper-settings .wallpaper-tile {
    min-width: 50px; min-height: 50px; border-radius: 12px;
    color: @theme_selected_bg_color; background-color: alpha(@theme_selected_bg_color, 0.10);
}
.tessel-shell-wallpaper-settings .wallpaper-card {
    background-color: @wallpaper_surface; border: 1px solid @wallpaper_line;
    border-radius: 8px; padding: 16px;
}
.tessel-shell-wallpaper-settings .wallpaper-section-title { font-size: 11pt; font-weight: 600; }
.tessel-shell-wallpaper-settings .wallpaper-detail { color: @wallpaper_muted; font-size: 9pt; }
.tessel-shell-wallpaper-settings .wallpaper-display-icon { color: @theme_selected_bg_color; }
.tessel-shell-wallpaper-settings .wallpaper-body button {
    background-image: none; background-color: @wallpaper_surface;
    color: @theme_fg_color; border: 1px solid @wallpaper_line;
    border-radius: 8px; box-shadow: none; text-shadow: none;
    min-height: 28px; padding: 4px 12px;
}
.tessel-shell-wallpaper-settings button:hover {
    background-color: mix(@wallpaper_surface, @theme_fg_color, 0.06);
}
.tessel-shell-wallpaper-settings button.suggested-action {
    background-color: @theme_selected_bg_color;
    color: @theme_selected_fg_color; border-color: transparent;
}
.tessel-shell-wallpaper-settings button.suggested-action:hover {
    background-color: shade(@theme_selected_bg_color, 1.10);
}
.tessel-shell-wallpaper-settings button:focus-visible {
    outline: 2px solid alpha(@theme_selected_bg_color, 0.65); outline-offset: -2px;
}
.tessel-shell-wallpaper-settings button:disabled { opacity: 0.45; }
.tessel-shell-wallpaper-settings dropdown popover contents {
    background-color: @theme_bg_color; color: @theme_fg_color;
    border: 1px solid @wallpaper_line; border-radius: 12px; padding: 6px;
}
.tessel-shell-wallpaper-settings dropdown listview { background: transparent; }
.tessel-shell-wallpaper-settings dropdown row { border-radius: 8px; padding: 5px 8px; }
.tessel-shell-wallpaper-settings dropdown row:selected {
    background-color: alpha(@theme_selected_bg_color, 0.18); color: @theme_fg_color;
}
.tessel-shell-wallpaper-settings separator { background-color: @wallpaper_line; min-height: 1px; }
/* The desktop's older, display-wide provider hides scrollbars at USER priority. */
.tessel-shell-wallpaper-settings scrolledwindow scrollbar {
    min-width: 8px; min-height: 8px; opacity: 1; background: transparent;
}
.tessel-shell-wallpaper-settings scrolledwindow scrollbar slider {
    min-width: 6px; min-height: 32px; border-radius: 6px;
    background-color: alpha(@theme_fg_color, 0.25);
}
"#;

/// One monitor's controls (`self.wallpaper_settings[conn]`).
pub struct MonitorControls {
    pub key: String,
    pub monitor: gdk::Monitor,
    pub file_path: RefCell<Option<String>>,
    pub selected_path: RefCell<Option<String>>,
    pub mode: gtk::DropDown,
    pub color: gtk::ColorDialogButton,
    pub chooser: gtk::Button,
}

pub struct WallpaperWindow {
    pub window: gtk::Window,
    pub controls: Vec<Rc<MonitorControls>>,
    pub apply: gtk::Button,
}

pub fn update_media_button(button: &gtk::Button, path: Option<&str>) {
    let base = path.map(|p| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    button.set_label(&base.unwrap_or_else(|| tr!("Choose image or video…")));
    button.set_tooltip_text(Some(&path.map(str::to_owned).unwrap_or_else(|| tr!("Choose an image, animation, or video"))));
    if let Some(label) = button.child().and_downcast::<gtk::Label>() {
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_max_width_chars(28);
    }
}

fn label(text: &str, css: Option<&str>) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.set_xalign(0.0);
    if let Some(css) = css {
        widget.add_css_class(css);
    }
    widget
}

/// `show_wallpaper_manager`: present the existing window or build one.
pub fn show_wallpaper_manager(view: &Rc<DesktopView>) {
    let Some(desktops) = view.desktops() else { return };
    if let Some(existing) = desktops.wallpaper_window.borrow().as_ref() {
        existing.window.present();
        return;
    }
    let window = gtk::Window::new();
    window.set_title(Some(&tr!("Wallpaper Settings")));
    window.add_css_class("tessel-shell-wallpaper-settings");
    window.set_default_size(620, 610);
    let display = gdk::Display::default().unwrap_or_else(|| WidgetExt::display(&view.window));
    if desktops.wallpaper_style.borrow().is_none() {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(WALLPAPER_STYLE_CSS);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
        desktops.wallpaper_style.replace(Some(provider));
    }
    window.set_titlebar(Some(&tessel_gtk::utils::headerbar(&tr!("Wallpaper Settings"))));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.add_css_class("wallpaper-body");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 20);
    content.set_margin_top(18);
    content.set_margin_bottom(20);
    content.set_margin_start(22);
    content.set_margin_end(22);
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let tile = gtk::Image::from_icon_name("preferences-desktop-wallpaper-symbolic");
    tile.set_pixel_size(26);
    tile.add_css_class("wallpaper-tile");
    tile.set_valign(gtk::Align::Center);
    hero.append(&tile);
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    heading.append(&label(&tr!("Wallpaper"), Some("wallpaper-title")));
    let subtitle = label(&tr!("Choose a background for each display."), Some("wallpaper-subtitle"));
    subtitle.set_wrap(true);
    heading.append(&subtitle);
    hero.append(&heading);
    content.append(&hero);

    let saved = config::read_cfg();
    let mut controls = Vec::new();
    let monitors = display.monitors();
    for index in 0..monitors.n_items() {
        let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() else { continue };
        let conn = monitor.connector().map(|c| c.to_string()).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("Monitor-{}", index + 1));
        let description = monitor.description().map(|d| d.to_string()).filter(|d| !d.is_empty()).unwrap_or_else(|| conn.clone());
        let previous = saved.get(&conn).cloned().unwrap_or(serde_json::Value::Object(Default::default()));
        let section = gtk::Box::new(gtk::Orientation::Vertical, 16);
        section.add_css_class("wallpaper-card");
        let monitor_heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let icon = gtk::Image::from_icon_name("video-display-symbolic");
        icon.set_pixel_size(22);
        icon.add_css_class("wallpaper-display-icon");
        monitor_heading.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        text.set_hexpand(true);
        let name = label(&description, Some("wallpaper-section-title"));
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(42);
        name.set_tooltip_text(Some(&description));
        text.append(&name);
        text.append(&label(&conn, Some("wallpaper-detail")));
        monitor_heading.append(&text);
        section.append(&monitor_heading);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(10);
        grid.set_column_spacing(20);
        let mut file_path = previous.get("file_path").and_then(|v| v.as_str()).map(str::to_owned);
        if !saved.contains_key(&conn) {
            // Nothing chosen for this screen yet: it shows the default wallpaper.
            let size = monitor.geometry();
            file_path = tessel_core::wallpaper::default_wallpaper(Some((size.width(), size.height()))).map(|p| p.to_string_lossy().into_owned());
        }
        let chooser = gtk::Button::new();
        chooser.set_hexpand(true);
        update_media_button(&chooser, file_path.as_deref());
        let translated: Vec<String> = MODES.iter().map(|m| tr!(*m)).collect();
        let mode = gtk::DropDown::from_strings(&translated.iter().map(String::as_str).collect::<Vec<_>>());
        mode.set_hexpand(true);
        let saved_mode = previous.get("mode").and_then(|v| v.as_str());
        mode.set_selected(saved_mode.and_then(|m| MODES.iter().position(|x| *x == m)).unwrap_or(0) as u32);
        let dialog = gtk::ColorDialog::new();
        dialog.set_title(&tr!("Background Color"));
        let color = gtk::ColorDialogButton::new(Some(dialog));
        color.set_halign(gtk::Align::Start);
        color.set_tooltip_text(Some(&tr!("Color behind the image or video")));
        let color_text = previous.get("color").and_then(|v| v.as_str()).unwrap_or("black");
        color.set_rgba(&gdk::RGBA::parse(color_text).unwrap_or_else(|_| gdk::RGBA::parse("black").unwrap()));
        for (row, (title, control)) in [(tr!("Image or video"), chooser.clone().upcast::<gtk::Widget>()), (tr!("Placement"), mode.clone().upcast()), (tr!("Background color"), color.clone().upcast())].into_iter().enumerate() {
            let row_label = label(&title, None);
            row_label.set_mnemonic_widget(Some(&control));
            grid.attach(&row_label, 0, row as i32, 1, 1);
            grid.attach(&control, 1, row as i32, 1, 1);
        }
        section.append(&grid);
        content.append(&section);
        let entry = Rc::new(MonitorControls { key: conn.clone(), monitor, file_path: RefCell::new(file_path), selected_path: RefCell::default(), mode, color, chooser: chooser.clone() });
        let (weak_view, weak_entry) = (Rc::downgrade(view), Rc::downgrade(&entry));
        chooser.connect_clicked(move |_| {
            if let (Some(view), Some(entry)) = (weak_view.upgrade(), weak_entry.upgrade()) {
                choose_image_for_monitor(&view, &entry);
            }
        });
        controls.push(entry);
    }

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&content));
    body.append(&scroll);
    body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    for (m, setter) in [(14, 0), (14, 1), (22, 2), (22, 3)] {
        match setter {
            0 => footer.set_margin_top(m),
            1 => footer.set_margin_bottom(m),
            2 => footer.set_margin_start(m),
            _ => footer.set_margin_end(m),
        }
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    footer.append(&spacer);
    let close = gtk::Button::with_label(&tr!("Close"));
    let apply = gtk::Button::with_label(&tr!("Apply"));
    apply.add_css_class("suggested-action");
    apply.set_sensitive(!controls.is_empty());
    footer.append(&close);
    footer.append(&apply);
    body.append(&footer);
    let weak = Rc::downgrade(view);
    apply.connect_clicked(move |_| if let Some(v) = weak.upgrade() { apply_wallpaper_settings(&v) });
    let w = window.clone();
    close.connect_clicked(move |_| w.close());
    let weak_desktops = Rc::downgrade(&desktops);
    window.connect_close_request(move |window| {
        if let Some(d) = weak_desktops.upgrade() {
            let same = d.wallpaper_window.borrow().as_ref().is_some_and(|w| &w.window == window);
            if same {
                d.wallpaper_window.replace(None);
            }
        }
        glib::Propagation::Proceed
    });
    let keys = gtk::EventControllerKey::new();
    let w = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.set_child(Some(&body));
    desktops.wallpaper_window.replace(Some(Rc::new(WallpaperWindow { window: window.clone(), controls, apply })));
    window.present();
}

/// Browse wallpapers without applying on selection (the shared chooser).
pub fn choose_image_for_monitor(view: &Rc<DesktopView>, entry: &Rc<MonitorControls>) {
    if super::chooser::present_existing() {
        return;
    }
    let Some(desktops) = view.desktops() else { return };
    let parent = desktops.wallpaper_window.borrow().as_ref().map(|w| w.window.clone());
    let (weak_entry, weak_desktops) = (Rc::downgrade(entry), Rc::downgrade(&desktops));
    let parent_for_check = parent.clone();
    let chosen = move |path: Option<String>| {
        let still = weak_desktops.upgrade().is_some_and(|d| d.wallpaper_window.borrow().as_ref().map(|w| w.window.clone()) == parent_for_check);
        let (Some(path), Some(entry)) = (path.filter(|p| !p.is_empty()), weak_entry.upgrade()) else { return };
        if !still {
            return;
        }
        update_media_button(&entry.chooser, Some(&path));
        entry.selected_path.replace(Some(path));
    };
    super::chooser::choose_wallpaper(parent.as_ref(), Box::new(chosen));
}

/// `apply_wallpaper_settings`: apply on each monitor's window, merge-write.
pub fn apply_wallpaper_settings(view: &Rc<DesktopView>) {
    let Some(desktops) = view.desktops() else { return };
    let Some(window) = desktops.wallpaper_window.borrow().clone() else { return };
    let old = config::read_cfg();
    let mut new = old.clone();
    for entry in &window.controls {
        let Some(target) = desktops.monitor_desktop(&entry.key) else {
            eprintln!("No wallpaper window registered for {}", entry.key);
            continue;
        };
        let old_entry = old.get(&entry.key);
        let mut path = entry.selected_path.borrow().clone().filter(|p| !p.is_empty())
            .or_else(|| entry.file_path.borrow().clone().filter(|p| !p.is_empty()))
            .or_else(|| old_entry.and_then(|o| o.get("file_path")).and_then(|v| v.as_str()).map(str::to_owned).filter(|p| !p.is_empty()));
        let selected = entry.mode.selected() as usize;
        let mode = MODES.get(selected).map(|m| m.to_string()).unwrap_or_else(|| old_entry.and_then(|o| o.get("mode")).and_then(|v| v.as_str()).unwrap_or("Fill").to_owned());
        if mode == "Color Only" {
            path = None;
        }
        let rgba = entry.color.rgba();
        target.set_monitor_wallpaper(&entry.monitor, path.as_deref(), Some(&mode), &rgba);
        new.insert(entry.key.clone(), config::wallpaper_entry(path.as_deref(), &mode, &rgba.to_str()));
        entry.file_path.replace(path.clone());
        entry.selected_path.replace(None);
        update_media_button(&entry.chooser, path.as_deref());
    }
    if let Err(error) = config::write_cfg(&new) {
        notify(&tr!("Could not set the wallpaper"), &error.to_string());
    }
}
