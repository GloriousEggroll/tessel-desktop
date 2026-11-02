#!/bin/sh
# Build a fake HOME from READ-ONLY COPIES of the real tessel-dock/tessel-shell
# configuration (never symlinks, never writes to the originals).
# Output names are rewritten in the copies only: DP-2 -> HEADLESS-1,
# HDMI-A-1 -> HEADLESS-2 (the private headless labwc's outputs), and the
# wallpaper paths point at copies inside the fake HOME.
# Usage: make_home.sh <dest>
set -eu
DEST=$1
SRC=$HOME
rm -rf "$DEST"
mkdir -p "$DEST/.config" "$DEST/.local/share/tessel-dock" "$DEST/.local/share/themes" "$DEST/.local/state" \
         "$DEST/.cache" "$DEST/Desktop" "$DEST/wallpapers"
cp -r --no-preserve=ownership "$SRC/.config/tessel-dock" "$SRC/.config/tessel-shell" "$SRC/.config/gtk-4.0" "$DEST/.config/"
mkdir -p "$DEST/.config/labwc"
for f in rc.xml menu.xml themerc themerc-override environment autostart; do
  [ -f "$SRC/.config/labwc/$f" ] && cp "$SRC/.config/labwc/$f" "$DEST/.config/labwc/"
done
[ -d "$SRC/.config/labwc/environment.d" ] && cp -r "$SRC/.config/labwc/environment.d" "$DEST/.config/labwc/"
[ -f "$SRC/.config/user-dirs.dirs" ] && cp "$SRC/.config/user-dirs.dirs" "$DEST/.config/"
cp -r "$SRC/.local/share/tessel-dock/papirus-icons" "$DEST/.local/share/tessel-dock/"
# User icon dir: real dirs copied, the Papirus mirror symlinks re-created to
# point at the fake HOME's mirror copy.
mkdir -p "$DEST/.local/share/icons"
for f in "$SRC/.local/share/icons"/*; do
  [ -e "$f" ] || continue
  n=$(basename "$f")
  if [ -L "$f" ]; then
    case "$(readlink "$f")" in
      "$SRC/.local/share/tessel-dock/papirus-icons/"*) ln -s "../tessel-dock/papirus-icons/$(basename "$(readlink "$f")")" "$DEST/.local/share/icons/$n";;
      *) cp -rL "$f" "$DEST/.local/share/icons/$n";;
    esac
  else cp -r "$f" "$DEST/.local/share/icons/"; fi
done
# User flatpak exports (desktop files + icons), dereferenced copies.
[ -d "$SRC/.local/share/flatpak/exports/share" ] && { mkdir -p "$DEST/.local/share/flatpak/exports"; cp -rL "$SRC/.local/share/flatpak/exports/share" "$DEST/.local/share/flatpak/exports/"; }
[ -d "$SRC/.local/share/themes/GTK" ] && cp -r "$SRC/.local/share/themes/GTK" "$DEST/.local/share/themes/"
[ -d "$SRC/.local/share/applications" ] && cp -r "$SRC/.local/share/applications" "$DEST/.local/share/"
[ -d "$SRC/.cache/tessel-dock" ] && cp -r "$SRC/.cache/tessel-dock" "$DEST/.cache/"
# Desktop: top-level files, folders as empty folders (their content is not shown).
for f in "$SRC/Desktop"/* "$SRC/Desktop"/.directory; do
  [ -e "$f" ] || continue
  if [ -d "$f" ]; then mkdir -p "$DEST/Desktop/$(basename "$f")"; else cp "$f" "$DEST/Desktop/"; fi
done
python3 -I - "$DEST" <<'PY'
import json, shutil, sys
from pathlib import Path
dest = Path(sys.argv[1])
names = {"DP-2": "HEADLESS-1", "HDMI-A-1": "HEADLESS-2"}
def rename(d):
    return {names.get(k, k): v for k, v in d.items()} if isinstance(d, dict) else d
wp = dest / ".config/tessel-shell/wallpapers.json"
data = json.loads(wp.read_text())
out = {}
for key, value in data.items():
    if isinstance(value, dict) and value.get("file_path"):
        src = Path(value["file_path"])
        copy = dest / "wallpapers" / src.name
        if src.is_file() and not copy.exists():
            shutil.copyfile(src, copy)
        value = dict(value, file_path=str(copy))
    if key in names.values():
        continue  # a stale entry for a name we map onto
    out[names.get(key, key)] = value
wp.write_text(json.dumps(out, indent=2))
cfg = dest / ".config/tessel-dock/config.json"
data = json.loads(cfg.read_text())
for key in ("monitor_overrides", "hot_corners"):
    if key in data:
        data[key] = rename(data[key])
cfg.write_text(json.dumps(data, indent=2))
PY
