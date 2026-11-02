# Clipboard (tessel-clipboard) vs. KDE Klipper

No Python twin: this replaces Klipper's main job on the labwc desktop.

| Klipper | tessel-clipboard |
|---|---|
| Copy survives the application closing | Yes: every new selection is read at once in all formats (ext-data-control-v1, else wlr-data-control v2); when its owner exits and the compositor clears the selection, tessel-clipboard offers the copy itself. While the owner lives, pastes still go to it. |
| "Prevent empty clipboard" | Same effect (an explicit clear by an application is also refilled; copying something new replaces it). |
| Ignore password-manager copies (`x-kde-passwordManagerHint: secret`) | Yes (Global Settings > Clipboard > Do not keep passwords, default on). |
| Synchronise / keep the selection (middle-click) | Keep it after its application closes: optional, default off. Clipboard and selection are not merged. |
| Size limit | Largest kept copy, default 64 MB (all formats together; formats that do not fit are dropped, text first in offer order usually survives). |
| History popup, actions, QR codes, saving history to disk | Not implemented. Nothing is written to disk; kept data lives only in tessel-clipboard's memory. |

Files: `src/capture.rs` (pure: formats, budget, secrets), `src/daemon.rs` (protocol and poll loop),
`settings/` (clipboard.json model and the GTK page Global Settings embeds), `tessel-clipboard.service`.

Tests: `cargo test -p tessel-clipboard -p tessel-clipboard-settings` (unit) and `tests/run_headless.sh`
(nested labwc: wl-copy owns a selection, is killed, wl-paste still gets the text / a 3 MB image;
primary selection, size limit and the off switch). Global Settings: `applets_headless`
`clipboard_page_edits_the_clipboard_settings_file`.
