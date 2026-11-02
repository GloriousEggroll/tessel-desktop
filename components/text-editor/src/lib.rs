//! Text Editor: a standalone GTK4 / GtkSourceView 5
//! editor with one document per window. See contracts/tessel-text-editor.md.
pub mod app;
pub mod cli;
pub mod codec;
pub mod compat;
pub mod document;
pub mod preferences;
pub mod recovery;
pub mod style;
pub mod window;
pub mod worker;

pub use app::{EditorApplication, APP_ID};
pub use window::EditorWindow;
