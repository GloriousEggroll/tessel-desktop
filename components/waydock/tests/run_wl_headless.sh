#!/bin/sh
# Build and run tests/wl_headless.rs inside a private nested headless labwc
# (tools/headless.md): own XDG_RUNTIME_DIR, own D-Bus, 2 outputs, 4 named
# desktops, no input devices. Never touches the live session.
set -eu
export TMPDIR=/run/user/1000/cbt/rust-waydock/${AGENT:-c}
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
BIN=$(cd "$ROOT" && cargo test -p waydock --test wl_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="wl_headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
R=$TMPDIR/n
mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
cat > "$R/cfg/rc.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <desktops><popupTime>0</popupTime><names><name>1</name><name>2</name><name>3</name><name>10</name></names></desktops>
</labwc_config>
XML
: > "$R/cfg/autostart"
rm -f "$R/result" "$R/log"
{
  echo '#!/bin/sh'
  echo 'case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac'
  printf 'WAYDOCK_WL_HEADLESS=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory timeout %s' "${TIMEOUT:-180}"
  printf " '%s'" "$BIN"
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
