#!/bin/sh
# Run tests/wallpaper_chooser_ui.rs inside a private nested headless labwc
# (tools/headless.md). Never touches the live session: own XDG_RUNTIME_DIR,
# own D-Bus, temporary HOME/XDG dirs, fake ffprobe-free PATH entries only.
set -eu
R=/run/user/1000/cbt/rust-wsfops/n
mkdir -p "$R/rt" "$R/cfg" "$R/home" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/tmp"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
BIN=$(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-ws-fops cargo test -p wayshell \
      --test wallpaper_chooser_ui --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="wallpaper_chooser_ui" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
WAYSHELL_FILEOPS_ISOLATED=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory \
HOME="$R/home" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" TMPDIR="$R/tmp" \
timeout 180 "$BIN" --nocapture --test-threads=1 > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R/tmp" XDG_DATA_HOME="$R/xdg/data" XDG_CONFIG_HOME="$R/xdg/config" \
  XDG_CACHE_HOME="$R/xdg/cache" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
