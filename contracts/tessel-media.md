# tessel-media — external contract

Source: `~/tessel-bluetooth/tessel-media/` (968 LOC: `media_manager.py` 398, `media_session.py` 192,
`install.py` 100, `test_media.py` 278) plus shared backends it imports from
`tessel-lock/lock_media.py` (294), `tessel-lock/lock_media_icons.py` (95),
`tessel-lock/lock_media_art.py` (102) and the dock-side `tessel-dock/media_status.py` (54),
`tessel-dock/media_controls.py` (126). The tessel-lock files are shared with the lock screen and
tessel-dock, so the Rust port should put the MPRIS client in a shared crate (`tessel-core` or a
`suite-mpris` crate) used by tessel-media, tessel-dock and tessel-lock.

## 1. Entry points and processes

| Item | Value | Ref |
|---|---|---|
| Installed script | `~/.config/waybar/scripts/tessel-media/media_manager.py` (+ copies of `media_session.py`, `lock_media*.py`, `companion_theme.py`, shared `tray_popup.py`, `gtk4_utils.py`, `suite_i18n.py`, `popup_placement.py`, ...) | install.py:25-35 |
| Launcher | `~/.local/bin/tessel-media` = `#!/bin/sh\nexec /usr/bin/python3 <target>/media_manager.py "$@"` (mode 0755) | install.py:40-41 |
| Also installs | `lock_media.py`, `lock_media_icons.py` → `~/.local/share/tessel-lock/`; `DOCK_FILES` → `~/.local/share/tessel-dock/`; `tessel-settings/settings_search.py` → `~/.local/share/tessel-settings/` | install.py:18-39 |
| GApplication id | `org.tessel.Media`, flags `HANDLES_COMMAND_LINE` (single instance; 2nd invocation forwards argv to the primary over D-Bus `org.gtk.Application`) | media_manager.py:348 |
| systemd unit | none. Launched on demand by tessel-dock via `systemd-run --user --quiet --collect --service-type=exec --slice=app.slice --expand-environment=no --setenv=WAYLAND_DISPLAY,DISPLAY,XAUTHORITY,XDG_RUNTIME_DIR,DBUS_SESSION_BUS_ADDRESS,XDG_CURRENT_DESKTOP,XDG_SESSION_TYPE,LABWC_PID -- /usr/bin/python3 ~/.config/waybar/scripts/tessel-media/media_manager.py --popup --popup-edge E --popup-offset N` | tessel-dock/panel_actions.py:10,28-47,76-83 |
| .desktop | none | |
| Keybinds | none in rc.xml | |

### CLI (media_manager.py:47-55)
- `--popup` (store_true; accepted but the popup is always shown on command line).
- `--popup-edge {top,bottom,left,right}` default `bottom`.
- `--popup-offset INT` default `76`; negative → argparse error (exit 2, `--popup-offset must be nonnegative`).
- `--settings` is NOT accepted: tessel-dock never passes it for `media` (panel_actions.py:43).
- Argv is validated before the GApplication runs (media_manager.py:397) and again in
  `do_command_line` (360-361; SystemExit code returned as command-line exit status).
- `do_command_line` → `show(edge, offset)`; if a popup exists, repositions + activates
  it instead of creating another (367-373). `do_activate` → `show()` with defaults.
- Process lifetime: the app exits when the popup is destroyed (no hold) except during
  "Open player" where `app.hold()` keeps it alive until the Raise reply (326-335).

## 2. IPC

### Owned
- Only the GApplication bus name `org.tessel.Media` (object path `/org/tessel/Media`,
  standard `org.gtk.Application` + `org.gtk.Actions`). No custom interface.

### Consumed: MPRIS (session bus) — lock_media.py + media_session.py
| Call | Detail | Ref |
|---|---|---|
| `org.freedesktop.DBus.ListNames` | at start; every name with prefix `org.mpris.MediaPlayer2.` discovered | lock_media.py:107-118 |
| signal `NameOwnerChanged` (sender/iface `org.freedesktop.DBus`, path `/org/freedesktop/DBus`) | on any owner change: remove player, rediscover if new owner non-empty | 103,120-126 |
| Proxy `org.mpris.MediaPlayer2.Player` at `/org/mpris/MediaPlayer2` | flags DO_NOT_AUTO_START + GET_INVALIDATED_PROPERTIES, timeout 1500 ms; uses cached props `PlaybackStatus, Metadata, CanControl, CanPlay, CanPause, CanGoPrevious, CanGoNext, CanSeek, Shuffle, LoopStatus, Rate, Position` | 140-166 |
| `Properties.GetAll("org.mpris.MediaPlayer2")` | reads `DesktopEntry`, `Identity`, `CanRaise`; re-read on `PropertiesChanged` for path `/org/mpris/MediaPlayer2` arg0 `org.mpris.MediaPlayer2` | 168-186 |
| `Player.Play/Pause/Previous/Next` (no args) | sent to the *unique owner* (not well-known name), NO_AUTO_START, 1500 ms; only if `CanControl` and the per-method capability (`CanPlay/CanPause/CanGoPrevious/CanGoNext`) are true and player not busy | 252-282 |
| `org.mpris.MediaPlayer2.Raise` | only if `CanRaise` true | 245-250 |
| `Properties.Get(Player,"Position")` → `(v)` x | on demand only (popup open), 1500 ms | media_session.py:113-128 |
| signal `Player.Seeked(x)` path `/org/mpris/MediaPlayer2`, any sender | re-anchor position for the player whose owner == sender | media_session.py:26-27,130-136 |
| `Player.SetPosition(o trackid, x pos)` | when `mpris:trackid` is a valid object path and != `/org/mpris/MediaPlayer2/TrackList/NoTrack`; else `Player.Seek(x delta)` with delta = target − interpolated position | media_session.py:156-171 |
| `Properties.Set(Player,"Shuffle", v<b>)` | if CanControl and Shuffle is a bool | 173-178 |
| `Properties.Set(Player,"LoopStatus", v<s>)` | cycles `None → Playlist → Track → None` | 13,180-186 |

Never: autostart players, pause on startup (`pause_playing` only used by lock screen),
open URLs, read browser databases.

### Dock side (inside tessel-dock process, not this binary)
- `tessel-dock/status_backend.py:246-249` creates `MediaStatus(lambda v: self._set('media', v))`;
  dock buttons call `status_backend.action('media-previous'|'media-toggle'|'media-next')`
  (status_backend.py:391-392 → media_status.py:45-49).
- Status dict emitted (media_status.py:34-43): `text ''`, `tooltip` = lines
  `Media`, identity, title, artist, `Playing|Paused`, error (or `Nothing playing`),
  `visible True`, `title`, `artist`, `player`, `playing`, `can_toggle`, `can_previous`,
  `can_next`, `icon_revision` (int bumped when resolved GIcon changes), `classes`
  (`('playing',)|('paused',)|()`).

## 3. Model / selection rules (lock_media.py:200-228, media_session.py)
- `text()`: collapse whitespace, cap 300 chars. Title fallback `Media playback`.
  Artists: first 10 strings of `xesam:artist` joined `, `. `xesam:url`, `mpris:artUrl`
  dropped if > 8192 chars.
- Only players in `Playing`/`Paused` count. Sort: playing first, then previously selected,
  then bus name. `controllable` = CanControl ∧ (CanPause if Playing else CanPlay) ∧ ¬readonly.
- `kind` (`track_kind`, 18-47): `xesam:contentType`, `xesam:mimeType`, `mpris:mediaType`,
  `kde:mediaType` (str or first 16 of list; casefold, strip `/`, take after `#`):
  video/ or audio/ prefixes; else URL suffix lists; else hosts (youtube/vimeo/twitch/netflix →
  video, spotify/soundcloud/apple music/tidal or `spotify:` scheme → audio).
- Pinned player (`choose`, media_session.py:70-74): stays selected while Playing/Paused;
  cleared when its name vanishes.
- `available()`: playing/paused players sorted by `(identity or name).casefold(), name`.
  Dropdown labels: identity or name minus `org.mpris.MediaPlayer2.`; visible only when >1.
- Position interpolation: anchor `(pos, monotonic)`; if Playing add elapsed×Rate (Rate≤0 or
  non-number → 1.0). Re-anchored on PlaybackStatus/Rate/Metadata change (Metadata resets to 0)
  then re-read (media_session.py:35-45).
- Errors: failed command → `Playback control is unavailable.`; failed Raise → `Could not open
  the media player.`; cleared on PlaybackStatus/Metadata change.
- Busy set: a player with an outstanding command has all controls insensitive.

## 4. Icons and art
- Icon resolution (lock_media_icons.py:54-77): (1) `DesktopEntry` + `.desktop` via
  DesktopAppInfo (reject names containing `/`); (2) unique exact casefold match of Identity
  against desktop id (minus `.desktop`), Name, StartupWMClass over visible apps with icons;
  (3) bus suffix with `\.(?:instance|pid)[_.-]?\d+(?:\..*)?$` removed, as desktop id;
  (4) unique match of DesktopEntry/candidate; (5) `multimedia-player`. Cached by
  `(name, desktop_entry, identity)` — title/art changes never re-scan apps.
- Dock (media_controls.py:9-21): generic names (`multimedia-player[-symbolic]`,
  `application-x-executable[-symbolic]`, `image-missing[-symbolic]`) or missing theme icon
  → fall back to the music-note glyph.
- Album art (lock_media_art.py): only when `kind != 'video'`; `file://` (local/localhost,
  absolute, regular file, O_NONBLOCK, ≤5 MiB) or http(s) without userinfo, UA
  `Kasumi-Media/1.0`, `Accept: image/*`, 4 s timeout, redirect must stay http(s); ≤16 Mpx;
  thumbnail 256×256, EXIF transpose, PNG RGBA. Worker thread, one in flight; LRU 12 entries;
  TTL 3600 s success / 60 s failure. Late results for an old URL populate cache but must not
  display. Album label shown only when `kind != 'video'` (`album_name`).

## 5. Files
- Reads: none of its own config. Installer edits `~/.config/tessel-dock/config.json`: removes
  `"media"` from `always_visible_apps` (install.py:43-49).
- Installer backups: `~/.local/state/tessel/backups/media-outline-<UTC %Y%m%dT%H%M%S%fZ>/`
  with `new-files.json` (install.py:58-62).
- Dock group layout: tessel-dock `group_layout.py:83-87` inserts `media` before `system_tray`
  in existing custom layouts. Global Settings exposes Media visibility/position under
  Dock/Panel (README); `settings_search.py:62` is tessel-lock media, separate.

## 6. UI surfaces
- Popup: `Gtk.ApplicationWindow` title `Media`, CSS classes `tessel-media`, `tray-popup`;
  inner `popup-surface`. Layer-shell via shared `PopupController(group="media")`
  (shared/tray_popup.py:15-130): namespace `desktop-applet`, layer OVERLAY, exclusive zone −1,
  keyboard ON_DEMAND, anchored to panel edge with margin = offset, plus side anchor margin 12
  (align `end` default); full-screen click-away backdrops namespace `desktop-applet-dismiss`,
  keyboard NONE, one per monitor. Escape / outside click / focus loss hide → destroy →
  app exits. `set_position` while visible re-shows on new edge (`show_pending`).
- CSS classes used (media_manager.py:19-37): `media-title`, `album-cover`, `media-control`
  (+`suggested-action` on toggle), `mode-toggle`, `seek-time`, `scale.seek`, `row-detail`.
- Widgets: header (`multimedia-player-symbolic`, `Media`, action `window-new-symbolic`
  "Open player"); dropdown "Choose media player"; Stack `player` (56 px icon) / `album`
  (96 px cover); labels player/title (2 lines, 28 chars)/artist/album; seek scale + elapsed /
  duration (`M:SS` or `H:MM:SS`, clock() 40-43); shuffle, previous, toggle, next, repeat
  (`media-playlist-repeat-song-symbolic` when Track); status label.
- Seek: hidden when `mpris:length` ≤ 0; sensitive only if CanControl∧CanSeek; drag defers
  send until release; scroll/keyboard debounced 250 ms; value clamped to length.
- Position polling only while mapped: 500 ms tick UI, real `Get Position` every 6th tick (3 s)
  (media_manager.py:298-322).
- Dock inline (media_controls.py): CSS `tessel-dock-media`, `tessel-dock-media-song` (22 chars),
  `tessel-dock-status tessel-dock-media-control`, `idle`; text `title · artist`; side edges rotate
  text 270° (left) / 90° (right) without thickening the panel.
- i18n strings: `Media, Open player, This player does not support opening its window., Choose
  media player, Playback position, Previous track, Play, Pause, Next track, Media player,
  Playback, Nothing playing, Playing, Paused, Start music or a video in your media app.,
  Shuffle on, Shuffle off, Repeat track, Repeat all, Repeat off`.

## 7. External commands
- None at runtime. Installer: `systemctl --user is-active --quiet tessel-dock.service`,
  `systemctl --user restart tessel-dock.service`, `~/.local/bin/tessel-dock --check` (poll JSON
  `ready`, 15 s) (install.py:63-78). Installer flags: `--apply`, `--with-tiling`,
  `--shared-media-only` (mutually exclusive with tiling).

## 8. Parity test cases (tessel-media/test_media.py, tessel-dock/test_inline_media_ui.py)
1. Open player: sensitive only if CanRaise; calls `Raise` on the owner; popup hides first
   and app closes after success; `raise_player()` returns False and sends nothing when
   CanRaise=false (test_media.py:69-90).
2. Live metadata: title with `<` `>` shown literally (no markup); toggle tooltip
   Pause/Play swaps after Pause/Play; CanGoNext=false disables next only; player exit →
   `Nothing playing` and all buttons insensitive (92-118).
3. Moving to another edge while open re-shows rather than closing; genuine dismissal sets
   `app.popup=None` (120-140).
4. Album art: BytesIcon cover on music; track change clears stale art; video uses player icon
   and hides album; no MPRIS calls triggered by art (142-175).
5. Seek: length 180 s → upper 180, `0:42`/`3:00`; seeking sends `SetPosition(trackid,
   90000000)`; interpolation advances; CanSeek false → insensitive; no calls after close;
   ticks stop on unmap (184-209).
6. Shuffle `Set Shuffle true`; repeat cycles `Playlist, Track, None`; switcher appears with
   2 players, sorted labels; choosing beta then toggle sends `Play` to beta, never `Pause`
   to alpha; players lacking Shuffle/LoopStatus/length hide those controls (211-239).
7. Dock controller: no MPRIS calls until explicit action; actions `Previous, Next, Pause` in
   order; classes playing/paused; text always ``; visible even when nothing playing;
   after close, refresh emits nothing (241-266).
8. Inline dock idle: empty media state hides text and all disabled controls, adds `idle`,
   popup button stays visible; `{'can_toggle':True}` keeps only toggle visible
   (test_inline_media_ui.py:73-94). Missing icons → music button (96); browser player
   (Helium) uses installed browser icon and updates on player change (113); fits all edges (145).

## 9. Porting notes / risks
- The MPRIS client is shared by three components: port once.
- Commands must address `get_name_owner()` unique name so a replacement player never gets
  a stale action.
- Media dock state (`title`) is `Nothing playing` when idle in media_status.py:35, but the
  inline idle test patches `{}`; replicate both exactly (idle = falsy `title`).
- No Wayland protocol beyond layer-shell (via gtk4-layer-shell).
