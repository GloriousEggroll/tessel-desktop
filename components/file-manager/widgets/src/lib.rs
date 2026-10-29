//! `files-widgets`: the standalone GTK4 widgets of Files (wayfiles), ported from
//! `~/btmanager/file-manager/{files_style,files_information,file_print,file_quicklook,
//! file_dialogs,file_jobs,files_devices}.py`. Contract: `contracts/file-manager.md` §5.
//!
//! Widgets are cheap-to-clone `Rc` handles (Python subclasses → struct + `widget()`);
//! Python attributes are public fields reachable through `Deref` or accessor methods.
//! Strings are the Python literals: only file_jobs.py (and `item_count`) translate (`tr!`).
//!
//! # API summary
//!
//! ## [`compat`] — items
//! `trait PreviewItem { file info name display_name uri is_dir content_type icon size modified
//! type_description as_any }`, `type Item = Rc<dyn PreviewItem>`; implemented for
//! `files_model::model::FileItem` (`compat::item(&file_item) -> Item`; get it back with
//! `item.as_any().downcast_ref::<FileItem>()`) and for `SimpleItem::{new(file, info), query(&file),
//! for_path(p) -> Item}`. Helpers `thousands`, `capacity` (re-export of files_model::devices::capacity).
//!
//! ## [`style`] — `CSS`, `NAVIGATION_CSS` (`&str`, byte-identical to files_style.py).
//!
//! ## [`information`] (files_information.py)
//! `TEXT_LIMIT`, `PDF_LIMIT`, kinds `IMAGE PDF MEDIA TEXT`, `preview_kind(Option<&Item>)`,
//! `kind_for_type`, `photo_details(Option<&Path>)`, `decode_preview / decode_text / decode_pdf
//! (&File, &Cancellable) -> Decoded = (Option<Payload::{Texture, Text}>, Details, String)`,
//! `read_text(&File, Option<&Cancellable>, limit) -> Result<(String, bool), ReadError>`,
//! `render_pdf_page`, `decoder(kind)`, **`set_decoder(kind, Option<Decoder>)`** (= Python's
//! `patch.object(files_information, 'decode_preview', …)`, process-wide).
//! `PreviewLoader::{new, request(&file, FnOnce(Decoded), kind), cancel, close, running, pending}`.
//! `FilePreview::new(large)`: `widget() -> &gtk::Stack` (pages image/placeholder/text/media),
//! fields `picture icon hint buffer (sourceview5::Buffer) text_view video placeholder`,
//! `show(Option<Item>, Option<&str> hint)`, `clear`, `close`, `item`, `kind`, `ready`,
//! `printable`, `media`, `source()` (true), `loader`, `set_on_loaded(Rc<dyn Fn(&Details, &str)>)`.
//! `InformationPane::new(hide: Fn(), print_file: Option<Rc<dyn Fn(Item)>>)`: `widget() -> &gtk::Box`,
//! fields `print_button collapse preview picture icon hint title details text`,
//! `show_selection(&[Item], &folder)`, `set_details`, `rows()`, `row_value(title)`, `loader()`,
//! `clear`, `close`.
//!
//! ## [`print`] (file_print.py)
//! `PRINTABLE`, `printable(Option<&Item>)`, `TextPages`, `print_file(parent: Option<&Window>,
//! &Item, on_error: Rc<dyn Fn(String)>, export_to: Option<&Path>) -> Option<PrintOperationResult>`.
//!
//! ## [`quicklook`] (file_quicklook.py)
//! `trait QuickLookPane { closed(), move_selection(delta) -> Option<Item>, activate(&Item) }`;
//! `QuickLook::new(&parent, Rc<dyn QuickLookPane>, print_file: Rc<dyn Fn(Item)>)`: fields
//! `window heading print_button open_button preview`, `present`, `close`, `show_item`,
//! `open_item`, `key_pressed(Key, ModifierType) -> bool` (Python `_key_pressed`). Connect
//! `window().connect_close_request` to drop your `quicklook` reference.
//!
//! ## [`dialogs`] (file_dialogs.py)
//! `dialog_window(parent, title, width, height) -> (Window, Box)` (= `_window`),
//! `has_companion_theme(&window)`, `show_error(parent, msg)`, `confirm(parent, title, msg,
//! FnOnce(bool))`, `prompt_text(parent, title, msg, initial, FnOnce(Option<String>))`,
//! `choose_file(parent, title, FnOnce(Option<File>), ChooserMode::{Open,Save,Folder},
//! initial_folder, initial_name) -> FileChooserDialog {window, file_chooser, accept_button, status}`,
//! `choose_application(parent, &file, FnOnce(Option<AppInfo>)) -> OpenWithDialog {window,
//! application_chooser, application_search, application_sort, default_check, default_label,
//! accept_button, status, applications_loaded(), rows()}`, `choose_application_with(.., OpenWithOptions
//! {discovery, set_default})`, `row_app_id(&row)`, `row_app_info(&row)`, `open_with_catalog`,
//! `gio_sources`. `PropertiesWindow::new(parent, &[File], on_changed: Option<Rc<dyn Fn()>>,
//! metadata_store: Option<MetadataStore>)`: `window present close busy cancelled metadata_loaded
//! size_label mode permission_checks owner group acl acl_text tags_entry rating_choice comment_text
//! app_choice hash_button hash_result apply_mode apply_owner apply_acl apply_annotations
//! set_default calculate_hashes`, pages `general annotations permissions open_with hash_page status`.
//!
//! ## [`jobs`] (file_jobs.py)
//! `trait JobHandle { cancel }` (impl for `files_ops::operations::Job`), `set_window_alive(&w, bool)`
//! / `window_alive(&w)` (Python `window.alive` + `jobs_button`: the lead marks FilesWindows),
//! `JobEntry {ident, title, detail, fraction, cut_generation, cancelled; set_job, job, window(),
//! cancel}`, `Change::{Added, Progress, Removed}`, `JobManager::new(&gtk::Application)`:
//! `add(title, origin, job) -> Rc<JobEntry>` (holds the app), `update(&e, detail, fraction)`,
//! `finish(&e, summary, failed)` (releases; notification when no Files window is left),
//! `subscribe(Rc<dyn Fn(Change, &Rc<JobEntry>)>) -> id`, `unsubscribe(id)`, `entries`,
//! `is_empty`, `windows`, `owned_by`, `cancel_all(Option<&[..]>)`, `set_notifier(..)` (test hook).
//! `JobsButton::new(&manager)`: `widget() -> &MenuButton`, `indicator`, `list`,
//! `row(ident) -> (row, title, progress, cancel)`, `idents()`, `pulsing`, `close`.
//! `ask_keep_running(parent, FnOnce(Option<KeepAnswer::{Keep, Cancel}>))`.
//!
//! ## [`devices`] (files_devices.DiskUsage)
//! `DiskUsage::new(&location)`: `widget()`, fields `bar detail location`, `refresh` (never
//! queues), `pending() -> Option<Cancellable>` (Python `usage.cancel`), `finished(&token, value)`,
//! `closed`, `close`.
//!
//! ## [`poppler`] — dlopen("libpoppler-glib.so.8") binding: `Document::{from_gfile, from_uri,
//! n_pages, page, title, author}`, `Page::{size, render, render_for_printing}`, `available()`.

pub mod compat;
pub mod devices;
pub mod dialogs;
pub mod information;
pub mod jobs;
pub mod poppler;
pub mod print;
mod properties;
pub mod quicklook;
pub mod style;

pub use compat::{Item, PreviewItem, SimpleItem};
