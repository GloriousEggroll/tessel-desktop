//! Basic screenshot annotation: pen, arrow, rectangle, text and crop
//! (capture_editor.py). Edits are a list of operations drawn over the untouched
//! capture, so Undo is exact. Saving renders them with cairo and replaces the
//! file atomically (or writes a new file with Save As).

use gtk::cairo;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use tessel_core::tr;
use tessel_gtk::utils::{accessible, headerbar, image as icon_image, set_margins};

pub const TOOLS: [(&str, &str, &str); 5] = [("pen", "Pen", "document-edit-symbolic"), ("arrow", "Arrow", "go-next-symbolic"),
    ("rectangle", "Rectangle", "checkbox-symbolic"), ("text", "Text", "insert-text-symbolic"), ("crop", "Crop", "image-crop-symbolic")];

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub kind: String,
    pub color: (f64, f64, f64, f64),
    pub width: i64,
    pub points: Vec<(f64, f64)>,
    pub text: String,
}

impl Operation {
    pub fn new(kind: &str, color: (f64, f64, f64, f64), width: i64, points: Vec<(f64, f64)>) -> Self {
        Self { kind: kind.into(), color, width, points, text: String::new() }
    }
}

fn cairo_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// `PIL.Image.open(path).convert('RGBA')` → cairo ARGB32 surface (premultiplied).
pub fn load_surface(path: &Path) -> Result<cairo::ImageSurface, String> {
    let data = std::fs::read(path).map_err(|e| tessel_core::pyerr::os_error_path(&e, path))?;
    let picture = image::load_from_memory(&data).map_err(|e| e.to_string())?.to_rgba8();
    let (width, height) = picture.dimensions();
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width as i32, height as i32).map_err(cairo_error)?;
    let stride = surface.stride() as usize;
    {
        let mut target = surface.data().map_err(cairo_error)?;
        for (y, row) in picture.rows().enumerate() {
            for (x, pixel) in row.enumerate() {
                let [r, g, b, a] = pixel.0;
                let premultiply = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
                let offset = y * stride + x * 4;
                let value = u32::from_be_bytes([a, premultiply(r), premultiply(g), premultiply(b)]);
                target[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
            }
        }
    }
    Ok(surface)
}

pub fn draw_operation(context: &cairo::Context, operation: &Operation) {
    let kind = operation.kind.as_str();
    if kind == "crop" {
        return;
    }
    let (r, g, b, a) = operation.color;
    context.set_source_rgba(r, g, b, a);
    let width = operation.width as f64;
    context.set_line_width(width);
    context.set_line_cap(cairo::LineCap::Round);
    context.set_line_join(cairo::LineJoin::Round);
    let points = &operation.points;
    if points.is_empty() {
        return;
    }
    let (first, last) = (points[0], points[points.len() - 1]);
    match kind {
        "pen" if points.len() > 1 => {
            context.move_to(first.0, first.1);
            for point in &points[1..] {
                context.line_to(point.0, point.1);
            }
            let _ = context.stroke();
        }
        "rectangle" => {
            context.rectangle(first.0.min(last.0), first.1.min(last.1), (last.0 - first.0).abs(), (last.1 - first.1).abs());
            let _ = context.stroke();
        }
        "arrow" => {
            let ((ax, ay), (bx, by)) = (first, last);
            context.move_to(ax, ay);
            context.line_to(bx, by);
            let _ = context.stroke();
            let angle = (by - ay).atan2(bx - ax);
            let head = (width * 4.0).max(10.0);
            context.move_to(bx, by);
            for side in [-1.0, 1.0] {
                context.line_to(bx - head * (angle + side * 0.45).cos(), by - head * (angle + side * 0.45).sin());
            }
            context.close_path();
            let _ = context.fill();
        }
        "text" if !operation.text.is_empty() => {
            context.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
            context.set_font_size((width * 5.0).max(10.0));
            context.move_to(first.0, first.1);
            let _ = context.show_text(&operation.text);
        }
        _ => {}
    }
}

/// The last crop operation's rectangle (whole image without one).
pub fn crop_rectangle(operations: &[Operation], size: (i32, i32)) -> (i64, i64, i64, i64) {
    let Some(crop) = operations.iter().rev().find(|op| op.kind == "crop") else { return (0, 0, size.0 as i64, size.1 as i64) };
    let ((ax, ay), (bx, by)) = (crop.points[0], crop.points[crop.points.len() - 1]);
    let round = crate::area::py_round;
    let x = round(ax.min(bx)).max(0);
    let y = round(ay.min(by)).max(0);
    let width = (size.0 as i64).min(round(ax.max(bx))) - x;
    let height = (size.1 as i64).min(round(ay.max(by))) - y;
    (x, y, width.max(1), height.max(1))
}

/// PNG bytes of the capture with every operation applied and cropped.
pub fn render(surface: &cairo::ImageSurface, operations: &[Operation]) -> Result<Vec<u8>, String> {
    let (x, y, width, height) = crop_rectangle(operations, (surface.width(), surface.height()));
    let output = cairo::ImageSurface::create(cairo::Format::ARgb32, width as i32, height as i32).map_err(cairo_error)?;
    {
        let context = cairo::Context::new(&output).map_err(cairo_error)?;
        context.translate(-x as f64, -y as f64);
        context.set_source_surface(surface, 0.0, 0.0).map_err(cairo_error)?;
        context.paint().map_err(cairo_error)?;
        for operation in operations {
            draw_operation(&context, operation);
        }
    }
    let mut stream = Vec::new();
    output.write_to_png(&mut stream).map_err(cairo_error)?;
    Ok(stream)
}

/// Re-encode as JPEG (quality 95, 4:4:4, sRGB ICC) for `.jpg`/`.jpeg` targets.
pub fn encode(png: Vec<u8>, suffix: &str) -> Result<Vec<u8>, String> {
    if !matches!(suffix.to_lowercase().as_str(), ".jpg" | ".jpeg") {
        return Ok(png);
    }
    crate::imaging::jpeg_with_icc(&crate::imaging::decode_rgb(&png)?)
}

fn suffix(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()
}

/// Atomic replace with mode 0600 (`.<name>-XXXXXXXX` temporary).
pub fn replace_file(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = crate::media::mkstemp(&parent, &format!(".{name}-"), "").map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
    let result = std::fs::write(&temporary, data)
        .and_then(|_| std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)))
        .and_then(|_| std::fs::rename(&temporary, path));
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| tessel_core::pyerr::os_error_text(&e))
}

pub struct EditorInner {
    pub window: gtk::ApplicationWindow,
    pub path: PathBuf,
    pub surface: cairo::ImageSurface,
    pub operations: RefCell<Vec<Operation>>,
    current: RefCell<Option<Operation>>,
    origin: Cell<(f64, f64)>,
    pub tool: RefCell<String>,
    scale: Cell<f64>,
    offset: Cell<(f64, f64)>,
    pub tool_buttons: Vec<(String, gtk::ToggleButton)>,
    pub color: gtk::ColorDialogButton,
    pub width: gtk::SpinButton,
    pub text: gtk::Entry,
    pub canvas: gtk::DrawingArea,
    pub status: gtk::Label,
    theme: Option<tessel_gtk::theme::WindowTheme>,
}

#[derive(Clone)]
pub struct EditorWindow(pub Rc<EditorInner>);

thread_local! {
    static EDITORS: RefCell<Vec<EditorWindow>> = const { RefCell::new(Vec::new()) };
}

/// Open editor windows (Python: EditorWindow instances among `app.get_windows()`).
pub fn editors() -> Vec<EditorWindow> {
    // gtk_window_destroy releases the application at once; the widget itself
    // is only disposed later, so prune by application membership.
    EDITORS.with(|list| {
        list.borrow_mut().retain(|e| e.0.window.application().is_some());
        list.borrow().clone()
    })
}

impl EditorWindow {
    pub fn new(application: &gtk::Application, path: &Path) -> Result<Self, String> {
        let surface = load_surface(path)?;
        let window = gtk::ApplicationWindow::builder().application(application).title(tr!("Edit Screenshot")).build();
        window.set_default_size(980, 700);
        let header = headerbar(&tr!("Edit Screenshot"));
        window.set_titlebar(Some(&header));
        let theme = Some(tessel_gtk::theme::attach(&window, None));
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        set_margins(&root, 10);
        window.set_child(Some(&root));
        let bar = gtk::Box::builder().spacing(6).build();
        root.append(&bar);
        let mut tool_buttons = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for (key, title, icon) in TOOLS {
            let button = gtk::ToggleButton::builder().child(&icon_image(icon, 18)).tooltip_text(tr!(title)).build();
            accessible(&button, &tr!(title));
            if let Some(group) = &group {
                button.set_group(Some(group));
            }
            if group.is_none() {
                group = Some(button.clone());
            }
            bar.append(&button);
            tool_buttons.push((key.to_string(), button));
        }
        let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
        color.set_rgba(&gdk::RGBA::parse("#e5484d").unwrap());
        color.set_tooltip_text(Some(&tr!("Color")));
        accessible(&color, &tr!("Color"));
        bar.append(&color);
        let width = gtk::SpinButton::with_range(1.0, 40.0, 1.0);
        width.set_value(4.0);
        width.set_tooltip_text(Some(&tr!("Line / text size")));
        accessible(&width, &tr!("Line / text size"));
        bar.append(&width);
        let text = gtk::Entry::builder().placeholder_text(tr!("Text to place")).hexpand(true).build();
        accessible(&text, &tr!("Text to place"));
        bar.append(&text);
        let mut buttons = Vec::new();
        for (icon, title) in [("edit-undo-symbolic", "Undo (Ctrl+Z)"), ("edit-copy-symbolic", "Copy to clipboard (Ctrl+C)"),
                              ("document-save-as-symbolic", "Save As… (Ctrl+Shift+S)")] {
            let button = gtk::Button::builder().child(&icon_image(icon, 18)).tooltip_text(tr!(title)).build();
            accessible(&button, &tr!(title));
            bar.append(&button);
            buttons.push(button);
        }
        let save = gtk::Button::builder().label(tr!("Save")).tooltip_text(tr!("Replace the screenshot file (Ctrl+S)")).build();
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        let canvas = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
        accessible(&canvas, &tr!("Screenshot being edited"));
        root.append(&canvas);
        let status = gtk::Label::builder().label(tr!("Drag to draw. Click to place text. Crop keeps the area you drag."))
            .xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        root.append(&status);
        let this = Self(Rc::new(EditorInner {
            window: window.clone(), path: path.to_path_buf(), surface, operations: RefCell::default(), current: RefCell::default(),
            origin: Cell::new((0.0, 0.0)), tool: RefCell::new("pen".into()), scale: Cell::new(1.0), offset: Cell::new((0.0, 0.0)),
            tool_buttons, color, width, text, canvas: canvas.clone(), status, theme,
        }));
        for (key, button) in &this.0.tool_buttons {
            let (weak, key) = (Rc::downgrade(&this.0), key.clone());
            button.connect_toggled(move |widget| {
                if let (true, Some(inner)) = (widget.is_active(), weak.upgrade()) {
                    EditorWindow(inner).set_tool(&key);
                }
            });
        }
        for (index, button) in buttons.iter().enumerate() {
            let weak = Rc::downgrade(&this.0);
            button.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let editor = EditorWindow(inner);
                match index {
                    0 => editor.undo(),
                    1 => editor.copy(),
                    _ => editor.save_as(),
                }
            });
        }
        let weak = Rc::downgrade(&this.0);
        save.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).save(None);
            }
        });
        let weak = Rc::downgrade(&this.0);
        canvas.set_draw_func(move |_, context, width, height| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).draw(context, width, height);
            }
        });
        let drag = gtk::GestureDrag::builder().button(1).build();
        let weak = Rc::downgrade(&this.0);
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).begin(x, y);
            }
        });
        let weak = Rc::downgrade(&this.0);
        drag.connect_drag_update(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).update(x, y);
            }
        });
        let weak = Rc::downgrade(&this.0);
        drag.connect_drag_end(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).end(x, y);
            }
        });
        canvas.add_controller(drag);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this.0);
        keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
            Some(inner) if EditorWindow(inner.clone()).key_pressed(key, modifiers) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
        this.0.tool_buttons[0].1.set_active(true);
        this.set_tool("pen");
        let weak = Rc::downgrade(&this.0);
        window.connect_close_request(move |_| {
            if let Some(inner) = weak.upgrade() {
                if let Some(theme) = &inner.theme {
                    theme.close();
                }
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(&this.0);
        window.connect_destroy(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            EDITORS.with(|list| list.borrow_mut().retain(|e| !Rc::ptr_eq(&e.0, &inner)));
        });
        EDITORS.with(|list| list.borrow_mut().push(this.clone()));
        Ok(this)
    }

    pub fn set_tool(&self, kind: &str) {
        *self.0.tool.borrow_mut() = kind.into();
        self.0.canvas.set_cursor_from_name(Some(if kind == "text" { "text" } else { "crosshair" }));
    }

    pub fn message(&self, text: &str) {
        self.0.status.set_text(text);
    }

    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.0.offset.get();
        let scale = self.0.scale.get();
        ((x - ox) / scale, (y - oy) / scale)
    }

    pub fn begin(&self, x: f64, y: f64) {
        let rgba = self.0.color.rgba();
        self.0.origin.set((x, y));
        let tool = self.0.tool.borrow().clone();
        let mut current = Operation::new(&tool, (rgba.red() as f64, rgba.green() as f64, rgba.blue() as f64, rgba.alpha() as f64),
                                         self.0.width.value_as_int() as i64, vec![self.point(x, y)]);
        current.text = self.0.text.text().to_string();
        if tool == "text" {
            if current.text.is_empty() {
                *self.0.current.borrow_mut() = None;
                self.message(&tr!("Type the text first, then click where it goes."));
                return;
            }
            self.0.operations.borrow_mut().push(current);
            *self.0.current.borrow_mut() = None;
        } else {
            *self.0.current.borrow_mut() = Some(current);
        }
        self.0.canvas.queue_draw();
    }

    pub fn update(&self, dx: f64, dy: f64) {
        let (ox, oy) = self.0.origin.get();
        let point = self.point(ox + dx, oy + dy);
        {
            let mut current = self.0.current.borrow_mut();
            let Some(current) = current.as_mut() else { return };
            if current.kind == "pen" {
                current.points.push(point);
            } else {
                current.points.truncate(1);
                current.points.push(point);
            }
        }
        self.0.canvas.queue_draw();
    }

    pub fn end(&self, dx: f64, dy: f64) {
        self.update(dx, dy);
        if let Some(current) = self.0.current.borrow_mut().take() {
            if current.points.len() > 1 {
                self.0.operations.borrow_mut().push(current);
            }
        }
        self.0.canvas.queue_draw();
    }

    fn draw(&self, context: &cairo::Context, width: i32, height: i32) {
        context.set_source_rgb(0.1, 0.1, 0.11);
        let _ = context.paint();
        let (iw, ih) = (self.0.surface.width(), self.0.surface.height());
        let scale = if iw > 0 && ih > 0 { (width as f64 / iw as f64).min(height as f64 / ih as f64).min(1.0) } else { 1.0 };
        self.0.scale.set(scale);
        let offset = ((width as f64 - iw as f64 * scale) / 2.0, (height as f64 - ih as f64 * scale) / 2.0);
        self.0.offset.set(offset);
        context.translate(offset.0, offset.1);
        context.scale(scale, scale);
        let _ = context.set_source_surface(&self.0.surface, 0.0, 0.0);
        let _ = context.paint();
        let mut operations = self.0.operations.borrow().clone();
        if let Some(current) = self.0.current.borrow().clone() {
            operations.push(current);
        }
        for operation in &operations {
            draw_operation(context, operation);
        }
        let (x, y, w, h) = crop_rectangle(&operations, (iw, ih));
        if (x, y, w, h) != (0, 0, iw as i64, ih as i64) {
            context.set_fill_rule(cairo::FillRule::EvenOdd);
            context.rectangle(0.0, 0.0, iw as f64, ih as f64);
            context.rectangle(x as f64, y as f64, w as f64, h as f64);
            context.set_source_rgba(0.0, 0.0, 0.0, 0.55);
            let _ = context.fill();
        }
    }

    pub fn undo(&self) {
        if self.0.operations.borrow_mut().pop().is_some() {
            self.0.canvas.queue_draw();
        }
    }

    pub fn set_operations(&self, operations: Vec<Operation>) {
        *self.0.operations.borrow_mut() = operations;
    }

    pub fn rendered(&self) -> Result<Vec<u8>, String> {
        render(&self.0.surface, &self.0.operations.borrow())
    }

    pub fn copy(&self) {
        let Some(application) = self.0.window.application() else { return };
        let data = match self.rendered() {
            Ok(data) => data,
            Err(error) => {
                self.message(&tr!("Could not copy: {error}", error = error));
                return;
            }
        };
        let weak = Rc::downgrade(&self.0);
        crate::clipboard::copy_image(&application, data, "image/png", Some(WidgetExt::display(&self.0.window)), Some(Box::new(move |error| {
            if let Some(inner) = weak.upgrade() {
                EditorWindow(inner).message(&match error {
                    Some(error) => tr!("Could not copy: {error}", error = error),
                    None => tr!("Copied to the clipboard."),
                });
            }
        })));
    }

    pub fn save(&self, path: Option<&Path>) -> bool {
        let target = path.map(Path::to_path_buf).unwrap_or_else(|| self.0.path.clone());
        let result = self.rendered().and_then(|png| encode(png, &suffix(&target))).and_then(|data| replace_file(&target, &data));
        match result {
            Ok(()) => {
                self.message(&tr!("Saved {path}", path = target.to_string_lossy().into_owned()));
                true
            }
            Err(error) => {
                self.message(&tr!("Could not save: {error}", error = error));
                false
            }
        }
    }

    pub fn save_as(&self) {
        let dialog = gtk::FileDialog::builder().title(tr!("Save Screenshot As")).build();
        if let Some(parent) = self.0.path.parent() {
            dialog.set_initial_folder(Some(&gio::File::for_path(parent)));
        }
        let stem = self.0.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        dialog.set_initial_name(Some(&format!("{stem}-edited{}", suffix(&self.0.path))));
        let weak = Rc::downgrade(&self.0);
        dialog.save(Some(&self.0.window), None::<&gio::Cancellable>, move |result| {
            let (Some(inner), Ok(file)) = (weak.upgrade(), result) else { return };
            if let Some(path) = file.path() {
                EditorWindow(inner).save(Some(&path));
            }
        });
    }

    pub fn key_pressed(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        if gtk::prelude::GtkWindowExt::focus(&self.0.window).is_some_and(|w| w.is::<gtk::Editable>() || w.ancestor(gtk::Text::static_type()).is_some()) {
            return false;
        }
        if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            let key = key.to_lower();
            let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            if key == gdk::Key::z {
                self.undo();
                return true;
            }
            if key == gdk::Key::c {
                self.copy();
                return true;
            }
            if key == gdk::Key::s {
                if shift { self.save_as() } else { self.save(None); }
                return true;
            }
        }
        if key == gdk::Key::Escape {
            self.0.window.close();
            return true;
        }
        false
    }
}

/// Open (or raise) the editor for a saved screenshot.
pub fn open_editor(application: &gtk::Application, path: &Path) -> Option<EditorWindow> {
    if let Some(existing) = editors().into_iter().find(|e| e.0.path == path) {
        existing.0.window.present();
        return Some(existing);
    }
    match EditorWindow::new(application, path) {
        Ok(window) => {
            window.0.window.present();
            Some(window)
        }
        Err(error) => {
            let notification = gio::Notification::new(&tr!("Could not open the screenshot editor"));
            notification.set_body(Some(&error));
            application.send_notification(Some("capture-editor-error"), &notification);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_crops_and_draws_operations() {
        let directory = std::env::temp_dir().join(format!("capture-editor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("Screenshot café #50% \"one\".png");
        let white = image::RgbImage::from_pixel(40, 30, image::Rgb([255, 255, 255]));
        std::fs::write(&file, crate::imaging::png_rgb_with_icc(&white).unwrap()).unwrap();
        let surface = load_surface(&file).unwrap();
        let mut text = Operation::new("text", (0.0, 0.0, 0.0, 1.0), 2, vec![(4.0, 28.0)]);
        text.text = "Hi".into();
        let operations = vec![
            Operation::new("rectangle", (1.0, 0.0, 0.0, 1.0), 2, vec![(2.0, 2.0), (20.0, 20.0)]),
            Operation::new("arrow", (0.0, 0.0, 1.0, 1.0), 2, vec![(5.0, 25.0), (35.0, 5.0)]),
            Operation::new("pen", (0.0, 1.0, 0.0, 1.0), 3, vec![(1.0, 1.0), (3.0, 9.0), (9.0, 3.0)]),
            text,
            Operation::new("crop", (0.0, 0.0, 0.0, 0.0), 0, vec![(1.0, 1.0), (31.0, 21.0)]),
        ];
        let png = render(&surface, &operations).unwrap();
        let picture = crate::imaging::decode_rgb(&png).unwrap();
        assert_eq!(picture.dimensions(), (30, 20));
        assert_eq!(picture.get_pixel(19, 10).0, [255, 0, 0]);
        assert_eq!(crop_rectangle(&operations[..1], (40, 30)), (0, 0, 40, 30));
        let jpeg = encode(png.clone(), ".JPG").unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert!(crate::imaging::icc_profile(&jpeg).is_some());
        assert_eq!(encode(png.clone(), ".png").unwrap(), png);
        replace_file(&file, &png).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(directory);
    }
}
