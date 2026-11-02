#!/bin/sh
# Run tests/ui_headless.rs (tessel-files-widgets) inside a private nested headless labwc
# (tools/headless.md). Never touches the live session: own XDG_RUNTIME_DIR and
# Wayland socket, own D-Bus without standard service dirs (nothing can be
# bus-activated; the run fails if anything was), no portals, local VFS only,
# private HOME and XDG dirs (mimeapps.list, desktop entries, metadata, trash all
# stay in $R), no input devices, PipeWire/Pulse pointed at nonexistent paths.
# Extra arguments filter check names.
set -eu
AGENT=${AGENT:-file-manager-widgets}
S=/run/user/1000/cbt/rust-$AGENT
R=$S/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/home" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data/applications" \
  "$R/xdg/state" "$R/apps/applications"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
# A private desktop entry directory first in XDG_DATA_DIRS (Open With discovery).
printf '[Desktop Entry]\nType=Application\nName=Private Text Viewer\nExec=/usr/bin/true %%F\nMimeType=text/plain;\n' \
  > "$R/apps/applications/fw-private-viewer.desktop"
ROOT=$(cd "$(dirname "$0")/../../../.." && pwd)
BIN=$(cd "$ROOT" && TMPDIR=$S cargo test -p tessel-files-widgets --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "could not build ui_headless"; exit 1; }
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
HOME="$R/home" FILES_WIDGETS_ISOLATED_TEST=1 FILES_WIDGETS_PRIVATE_ROOT="$R" \
PIPEWIRE_REMOTE="$R/no-pipewire" PULSE_SERVER="unix:$R/no-pulse" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none \
GSK_RENDERER=cairo GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" \
XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" \
XDG_CONFIG_DIRS="$R/none" XDG_DATA_DIRS="$R/apps:/usr/share" \
timeout 600 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" \
  TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
