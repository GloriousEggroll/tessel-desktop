//! Port of files_style.py: the Files stylesheet, byte-identical to the Python bytes.
//! `CSS` is installed at APPLICATION priority; `NAVIGATION_CSS` sits above the shared
//! palette theme (Python installs it at APPLICATION+40 in files_ui.py).

/// `files_style.CSS`.
pub const CSS: &str = r#"
.files-window .files-toolbar { padding: 8px 0; }
.files-window .files-navigation-buttons { padding: 0 12px; }
.files-window .files-toolbar button { min-width: 24px; min-height: 24px; padding: 5px; }
.files-window .files-sidebar { padding: 12px 8px; min-width: 176px; border-right: 1px solid alpha(currentColor, .12); }
.files-window .files-disk-usage label { font-size: 9pt; }
.files-window .files-disk-usage trough { min-height: 5px; border: none; border-radius: 3px; box-shadow: none; background: alpha(currentColor, .12); }
.files-window .files-disk-usage progress { min-width: 0; min-height: 5px; border: none; border-radius: 3px; box-shadow: none; background: @theme_selected_bg_color; }
.files-window .files-sidebar .section-heading { font-size: 10pt; font-weight: 650; opacity: .7; padding: 10px 10px 4px; }
.files-window .files-location { padding: 8px; border-bottom: 1px solid alpha(currentColor, .1); }
.files-window .files-location button { padding: 5px 7px; }
.files-window .files-pane { border: 1px solid transparent; }
.files-window .files-split .files-pane.active-pane { border-color: alpha(currentColor, .2); }
.files-window .files-tile { padding: 10px 8px; border-radius: 10px; }
/* Adwaita gives "gridview > child > box" a 12px margin that ate a quarter of each tile's label width. */
.files-window gridview > child > .files-tile { margin: 0; }
.files-window gridview { padding: 10px; }
.files-window gridview child { border-radius: 10px; margin: 2px; }
.files-window columnview cell { padding: 3px 10px; }
.files-window columnview row { min-height: 0; }
.files-window columnview .files-tile, .files-window listview .files-tile { padding: 0; }
.files-window .files-status { padding: 8px 14px; font-size: 10pt; }
.files-window .files-error { padding: 8px 14px; }
.files-window .files-info { border-left: 1px solid alpha(currentColor, .12); }
.files-window .files-info-header { padding: 8px 14px; border-bottom: 1px solid alpha(currentColor, .10); }
.files-window .files-info-content { padding: 16px; }
.files-window .files-image-preview { background-color: alpha(currentColor, .035); border-radius: 10px; }
.files-window .files-info button { background: transparent; border: none; box-shadow: none; }
.files-window .files-info button:hover { background-color: alpha(currentColor, .10); }
.files-window .files-info .title { font-size: 13pt; font-weight: 650; }
.files-window .muted { opacity: .65; }
.files-window .files-progress { padding: 8px 12px; }
.files-window button { border-radius: 8px; }
.files-window .files-empty { padding: 30px; }
.files-window .files-column { border-right: 1px solid alpha(currentColor, .10); }
.files-window .files-column listview { background: transparent; }
.files-window .files-column listview row { padding: 4px 10px; border-radius: 8px; }
.files-window .files-column-list > row:selected { background-color: alpha(currentColor, .12); }
.files-text-preview { font-family: monospace; }
.files-window .files-text-preview { font-size: 9.5pt; padding: 8px; }
.files-window .files-columns .files-tile { padding: 3px 8px; border-radius: 8px; }
.files-peek > contents { padding: 10px 12px; }
.files-peek .heading { font-weight: 650; margin-bottom: 4px; }
.files-peek .muted, .files-goto .muted { opacity: .65; }
.files-goto { border-radius: 14px; }
.files-goto .files-goto-content { padding: 14px; }
.files-goto .files-goto-results { background: transparent; }
.files-goto .files-goto-results row { padding: 6px 10px; border-radius: 8px; }
.files-goto .heading { font-weight: 600; }
.files-quicklook .files-text-preview { font-size: 10.5pt; padding: 12px; }
"#;

// Kept separate so these small chrome overrides can sit above the shared theme
// without replacing the application's wallpaper-derived foreground/accent colors.
/// `files_style.NAVIGATION_CSS`.
pub const NAVIGATION_CSS: &str = r#"
/* Above the palette's accent progress fill, so nearly full disks stay red. */
.files-window .files-disk-usage progressbar.low-space progress { background-color: @error_color; border-color: @error_color; }
.files-window columnview > header,
.files-window columnview > header button { border-radius: 0; }

.files-window .files-toolbar button,
.files-window .files-toolbar button:checked,
.files-window button.files-sidebar-action {
    background: transparent;
    border: 1px solid transparent;
    box-shadow: none;
}
.files-window .files-toolbar button:hover,
.files-window button.files-sidebar-action:hover { background-color: alpha(currentColor, .10); }
.files-window .files-toolbar button:focus-visible,
.files-window button.files-sidebar-action:focus-visible { outline: 2px solid alpha(currentColor, .4); outline-offset: -2px; }
.files-window button.files-sidebar-action { min-width: 20px; min-height: 20px; padding: 5px; }
.files-window .files-address {
    background-color: alpha(currentColor, .035);
    border: 1px solid alpha(currentColor, .13);
    border-radius: 8px;
    padding: 0 5px;
}
.files-window .files-address:focus-within { border-color: alpha(currentColor, .38); }
.files-window .files-address entry {
    background: transparent;
    border: none;
    box-shadow: none;
    min-height: 30px;
    padding: 1px 6px;
}
.files-window .files-address button.files-crumb {
    background: transparent;
    border: none;
    box-shadow: none;
    min-width: 0;
    min-height: 28px;
    padding: 2px 6px;
    border-radius: 5px;
    font-weight: normal;
}
.files-window .files-address button.files-crumb:hover,
.files-window .files-address button.files-crumb:focus-visible {
    background-color: alpha(currentColor, .10);
}
.files-window .files-crumb-arrow { opacity: .5; margin: 0 2px; }
.files-completion list { background: transparent; }
.files-completion row { padding: 8px 10px; border-radius: 6px; }
.files-completion row:hover { background-color: alpha(currentColor, .10); }
.files-completion .dim-label { padding: 4px 10px; font-size: 10pt; }
.files-window .files-tabs > header {
    background: transparent;
    border: none;
    border-bottom: 1px solid alpha(currentColor, .12);
    box-shadow: none;
    padding: 4px 6px;
}
.files-window .files-tabs > header tabs { border: none; }
.files-window .files-tabs > header tab {
    background: transparent;
    border: none;
    border-radius: 7px;
    box-shadow: none;
    padding: 4px 9px;
    margin: 0 2px;
    min-height: 26px;
}
.files-window .files-tabs > header tab:hover { background-color: alpha(currentColor, .05); }
.files-window .files-tabs > header tab:checked {
    background-color: alpha(currentColor, .10);
    box-shadow: none;
}
.files-window .files-tab-label label { font-weight: normal; }
.files-window button.files-tab-close {
    background: transparent;
    border: none;
    box-shadow: none;
    min-height: 20px;
    min-width: 20px;
    padding: 0;
    border-radius: 5px;
    opacity: .6;
}
.files-window button.files-tab-close:hover,
.files-window button.files-tab-close:focus-visible {
    background-color: alpha(currentColor, .12);
    opacity: 1;
}
"#;

#[cfg(test)]
mod tests {
    /// Byte-identical to the Python bytes literals (read-only check when the source is present).
    #[test]
    fn matches_python_source() {
        let path = std::path::Path::new(&std::env::var("HOME").unwrap_or_default()).join("btmanager/file-manager/files_style.py");
        let Ok(source) = std::fs::read_to_string(path) else { return };
        let literal = |name: &str| {
            let start = source.find(&format!("{name} = b'''")).unwrap() + name.len() + 7;
            let end = source[start..].find("'''").unwrap() + start;
            source[start..end].to_string()
        };
        assert_eq!(literal("CSS"), super::CSS);
        assert_eq!(literal("NAVIGATION_CSS"), super::NAVIGATION_CSS);
    }
}
