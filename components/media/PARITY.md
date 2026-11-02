# tessel-media — parity with contracts/tessel-media.md

Crate `components/media`: binary `tessel-media` (= `media_manager.py` / `waymedia`)
and a library holding the MPRIS client (`mpris`: lock_media.LockMedia + media_session
.MediaSession), player icons (`icons`: lock_media_icons), album art (`art`: lock_media_art)
and Tessel Dock's `dock_status::MediaStatus` (tessel-dock/media_status.py) for reuse by the tessel-dock
and tessel-lock ports. Tests: `cargo test -p tessel-media` (unit + `tests/private_bus.rs`
on a `gio::TestDBus` with simulated players), `tests/run_headless.sh` (all popup
AppletTests in a private nested headless labwc), `tests/measure_headless.sh`. Only
simulated players (`tests/support/fake_player.rs`, refuses to run without
`MEDIA_PRIVATE_BUS=1` + a private bus) are ever controlled.

Legend: ✅ implemented + tested · ☑ implemented, not directly tested · ⚠ deviation · ❌ gap

## §1 Entry points / CLI
| Item | | Test |
|---|---|---|
| GtkApplication `org.tessel.Media`, `HANDLES_COMMAND_LINE`; `--popup`, `--popup-edge`, `--popup-offset` (negative → exit 2 "must be nonnegative"), `--settings` rejected; argv validated locally and in the primary | ✅ | private_bus `cli_rejects_settings_and_negative_offsets`, `compat::argparse::tests` |
| show(): create + show, else `set_position` + `activate`; exits when the popup is destroyed; `hold()` only while "Open player" waits | ✅ | ui `open_player_*`, `moving_to_another_edge_*` |

## §2 MPRIS (consumed)
| Item | | Test |
|---|---|---|
| ListNames discovery, NameOwnerChanged remove/rediscover, proxies DO_NOT_AUTO_START + GET_INVALIDATED_PROPERTIES 1500 ms | ✅ | private_bus `live_title_selection_*`, `skip_tracks_*` |
| Root `GetAll` (DesktopEntry/Identity/CanRaise), re-read on root PropertiesChanged | ✅ | ui `open_player_raises_media_owner_and_obeys_capability`, private_bus `player_desktop_identity_*` |
| Play/Pause/Previous/Next/Raise to the **unique owner**, NO_AUTO_START, capability + busy + readonly gating, error texts, `pause_playing` once (lock screen) | ✅ | private_bus `pause_existing_playing_only_once_*`, `replacement_owner_never_receives_stale_command` |
| Position `Get` on demand, Seeked re-anchor, interpolation × Rate, re-anchor on PlaybackStatus/Rate/Metadata | ✅ | ui `seek_bar_tracks_position_and_seeks_with_track_id` |
| SetPosition(trackid, x) / Seek(delta) fallback, Shuffle / LoopStatus `Set`, LOOP cycle | ✅ | ui `seek_bar_*`, `shuffle_repeat_and_player_switcher` |
| Model rules: text() 300 chars, title fallback, 10 artists, 8192-char URLs, sort, pinned player, available() order, track_kind | ✅ | `mpris::tests::*`, private_bus `album_metadata_*`, ui switcher |

## §4 Icons and art
Icon resolution order (DesktopEntry → unique Identity match → bus name minus
`.instance/.pid` suffix → matches → `multimedia-player`), cache key: ✅ `icons::tests`, ui
(Spotify icon resolved in measure screenshots). Art: file:// (local, absolute, regular, O_NONBLOCK,
≤5 MiB) or http(s) without userinfo (UA `Kasumi-Media/1.0`, 4 s, redirects http(s) only),
≤16 Mpx, ≤256 px, EXIF orientation, RGBA PNG, one in flight, LRU 12, TTL 3600/60 s, late
results cached but not shown: ✅ `art::tests`, ui `album_cover_updates_*`.

## §6 UI
Popup title/CSS classes/CSS text verbatim, header "Open player" (tooltip swap), switcher only
with >1 player, player/album stack (56/96 px), labels (no markup), seek bar (hidden when no
length, drag defers, 250 ms debounce, clamp), 500 ms tick + real Get every 6th tick only while
mapped, shuffle/repeat toggles and icons, all strings via `tr!`: ✅ ui_headless (7 checks).
measure_headless: Rust and Python popups with the same simulated player are **pixel-identical**.

## §8 Dock side
`MediaStatus` (text ``, tooltip lines, title/artist/player/playing/can_*/icon_revision/
classes, actions only on request, nothing emitted after close): ✅ private_bus
`dock_controller_only_changes_playback_on_explicit_action`. Note: the contract table says
`text ''`; the Python source and its test use `''` — the port follows the source.

## Deviations ⚠ / gaps ❌
1. ⚠ gio D-Bus (not zbus); art decoding with gdk-pixbuf instead of PIL (thumbnail rounding and
   resampling filter may differ by a pixel; formats limited to the installed pixbuf loaders).
2. ⚠ HTTP art via `ureq` + system OpenSSL; proxies from the environment like urllib.
3. ❌ tessel-dock's inline `media_controls.py` widgets (contract test 8) belong to the tessel-dock port;
   `dock_status` + `icons` are provided for it. The shared MPRIS client should move to a
   shared crate (`suite-mpris`) — it lives here because crates/ is off limits to this agent.
4. ❌ The Python installer's tessel-dock `config.json` edit / backups are installer work (not ported).
