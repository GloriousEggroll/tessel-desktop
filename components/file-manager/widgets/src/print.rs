//! Port of file_print.py: print PDFs, images and text files through GTK's print dialog.

use crate::compat::Item;
use crate::information::{preview_kind, read_text, ReadError, IMAGE, PDF, TEXT, TEXT_LIMIT};
use crate::poppler;
use gtk::prelude::*;
use gtk::{gdk_pixbuf, pango};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

pub const PRINTABLE: [&str; 3] = [PDF, IMAGE, TEXT];

#[link(name = "pangocairo-1.0")]
extern "C" {
    fn pango_cairo_show_layout_line(cr: *mut gtk::cairo::ffi::cairo_t, line: *mut pango::ffi::PangoLayoutLine);
}

fn show_layout_line(cr: &gtk::cairo::Context, line: &pango::LayoutLine) {
    use glib::translate::ToGlibPtr;
    unsafe { pango_cairo_show_layout_line(cr.to_raw_none(), line.to_glib_none().0) };
}

/// `printable(item)`.
pub fn printable(item: Option<&Item>) -> bool {
    preview_kind(item).is_some_and(|kind| PRINTABLE.contains(&kind))
}

/// Scale and centre a `width × height` drawing on the printable page.
fn fit(context: &gtk::PrintContext, width: f64, height: f64) -> gtk::cairo::Context {
    let scale = (context.width() / width).min(context.height() / height);
    let cr = context.cairo_context();
    cr.translate((context.width() - width * scale) / 2.0, (context.height() - height * scale) / 2.0);
    cr.scale(scale, scale);
    cr
}

/// Monospace text broken into page-sized runs of layout lines.
pub struct TextPages {
    pub text: String,
    pub layout: Option<pango::Layout>,
    pub pages: Vec<(i32, i32)>,
}

impl TextPages {
    pub fn new(text: String) -> Self {
        Self { text, layout: None, pages: Vec::new() }
    }

    pub fn begin(&mut self, operation: &gtk::PrintOperation, context: &gtk::PrintContext) {
        let layout = context.create_pango_layout();
        layout.set_font_description(Some(&pango::FontDescription::from_string("Monospace 9")));
        layout.set_width((context.width() * pango::SCALE as f64) as i32);
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_text(&self.text);
        let (mut pages, mut start, mut used) = (Vec::new(), 0, 0.0);
        let count = layout.line_count();
        for index in 0..count {
            let Some(line) = layout.line_readonly(index) else { continue };
            let (_ink, logical) = line.extents();
            let height = logical.height() as f64 / pango::SCALE as f64;
            if used + height > context.height() && index > start {
                pages.push((start, index));
                start = index;
                used = 0.0;
            }
            used += height;
        }
        pages.push((start, (start + 1).max(count)));
        operation.set_n_pages(pages.len() as i32);
        self.pages = pages;
        self.layout = Some(layout);
    }

    pub fn draw(&self, context: &gtk::PrintContext, number: i32) {
        let (Some(layout), Some(&(first, end))) = (self.layout.as_ref(), self.pages.get(number as usize)) else { return };
        let cr = context.cairo_context();
        cr.set_source_rgb(0.0, 0.0, 0.0);
        let mut y = 0.0;
        for index in first..end.min(layout.line_count()) {
            let Some(line) = layout.line_readonly(index) else { continue };
            let (_ink, logical) = line.extents();
            let baseline = -(logical.y() as f64) / pango::SCALE as f64;
            cr.move_to(0.0, y + baseline);
            show_layout_line(&cr, &line);
            y += logical.height() as f64 / pango::SCALE as f64;
        }
    }
}

/// Open the print dialog for `item` (or render it to the PDF `export_to`, synchronously);
/// errors go to `on_error`. Returns the operation result, `None` after an error.
pub fn print_file(parent: Option<&gtk::Window>, item: &Item, on_error: Rc<dyn Fn(String)>,
                  export_to: Option<&Path>) -> Option<gtk::PrintOperationResult> {
    let kind = preview_kind(Some(item));
    let operation = gtk::PrintOperation::new();
    operation.set_job_name(&item.display_name());
    operation.set_embed_page_setup(true);
    operation.set_allow_async(true);
    operation.set_use_full_page(false);
    let errors = on_error.clone();
    operation.connect_done(move |finished, result| {
        if result == gtk::PrintOperationResult::Error {
            errors(finished.error().map(|e| e.message().to_string())
                .unwrap_or_else(|| "The document could not be printed.".into()));
        }
    });
    let prepared: Result<(), String> = (|| {
        match kind {
            Some(PDF) => {
                let document = Rc::new(poppler::Document::from_gfile(&item.file(), None).map_err(|e| e.to_string())?);
                operation.set_n_pages(document.n_pages());
                operation.connect_draw_page(move |_, context, number| {
                    if let Some(page) = document.page(number) {
                        let (width, height) = page.size();
                        page.render_for_printing(&fit(context, width, height));
                    }
                });
            }
            Some(IMAGE) => {
                let stream = item.file().read(gio::Cancellable::NONE).map_err(|e| e.message().to_string())?;
                let result = gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE);
                let _ = stream.close(gio::Cancellable::NONE);
                let mut pixbuf = result.map_err(|e| e.message().to_string())?;
                if let Some(rotated) = pixbuf.apply_embedded_orientation() {
                    pixbuf = rotated;
                }
                operation.set_n_pages(1);
                operation.connect_draw_page(move |_, context, _| {
                    let cr = fit(context, pixbuf.width() as f64, pixbuf.height() as f64);
                    #[allow(deprecated)]
                    gtk::gdk::prelude::GdkCairoContextExt::set_source_pixbuf(&cr, &pixbuf, 0.0, 0.0);
                    let _ = cr.paint();
                });
            }
            Some(TEXT) => {
                let (text, _truncated) = read_text(&item.file(), None, TEXT_LIMIT).map_err(|e| match e {
                    ReadError::Glib(error) => error.message().to_string(),
                    ReadError::Value(text) => text,
                })?;
                let pages = Rc::new(RefCell::new(TextPages::new(text)));
                let begin = pages.clone();
                operation.connect_begin_print(move |operation, context| begin.borrow_mut().begin(operation, context));
                operation.connect_draw_page(move |_, context, number| pages.borrow().draw(context, number));
            }
            _ => return Err("This file type cannot be printed from Files.".into()),
        }
        Ok(())
    })();
    if let Err(message) = prepared {
        on_error(message);
        return None;
    }
    let result = if let Some(path) = export_to {
        operation.set_allow_async(false);
        operation.set_export_filename(path);
        operation.run(gtk::PrintOperationAction::Export, parent)
    } else {
        operation.run(gtk::PrintOperationAction::PrintDialog, parent)
    };
    match result {
        Ok(result) => Some(result),
        Err(error) => {
            on_error(error.message().to_string());
            None
        }
    }
}
