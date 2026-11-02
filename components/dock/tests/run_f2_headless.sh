#!/bin/sh
# Build and run tests/f2_ui_headless.rs (panel widgets: indicators, tray,
# expander, media, capture controls) inside a private nested headless labwc
# (tools/headless.md): own XDG_RUNTIME_DIR, own D-Bus without service
# activation, own XDG dirs, 2 outputs, no input devices (pointer input only via
# wlrctl's virtual pointer on the PRIVATE compositor). Never touches the live
# session. Optional $1 = test-name filter (substring).
set -eu
export TMPDIR=/run/user/1000/cbt/rust-tessel-dock/${AGENT:-f2}
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
BIN=$(cd "$ROOT" && cargo test -p tessel-dock --test f2_ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="f2_ui_headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
R=$TMPDIR/n
rm -rf "$R"
mkdir -p "$R/rt" "$R/cfg" "$R/xdg-config" "$R/cache" "$R/data" "$R/tmp" "$R/services"; chmod 700 "$R/rt"
# Private bus; no D-Bus service activation (nothing real can be auto-started).
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" \
    -e "s|<standard_session_servicedirs */>|<servicedir>$R/services</servicedir>|" \
    /usr/share/dbus-1/session.conf > "$R/d.conf"
cat > "$R/cfg/rc.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <core><gap>0</gap></core>
</labwc_config>
XML
: > "$R/cfg/autostart"
rm -f "$R/result" "$R/log"
{
  echo '#!/bin/sh'
  echo 'case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac'
  printf 'WAYDOCK_F2_HEADLESS=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory'
  printf ' TMPDIR="%s/tmp" timeout %s' "$R" "${TIMEOUT:-600}"
  printf " '%s' %s" "$BIN" "${1:-}"
  printf ' > "%s/log" 2>&1; echo $? > "%s/result"\n' "$R" "$R"
  echo 'labwc --exit'
} > "$R/run.sh"
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 RUST_BACKTRACE=1 \
  XDG_CONFIG_HOME="$R/xdg-config" XDG_CACHE_HOME="$R/cache" XDG_DATA_HOME="$R/data" \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log" 2>/dev/null || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
