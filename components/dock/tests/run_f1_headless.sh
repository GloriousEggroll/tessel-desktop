#!/bin/sh
# Build and run tests/f1_ui_headless.rs inside a private nested headless labwc
# (tools/headless.md): own XDG_RUNTIME_DIR, own D-Bus with NO service
# directories (nothing auto-activates), private XDG dirs, 2 outputs, no input
# devices (wlrctl/wtype create virtual ones inside the private compositor).
# Never touches the live session. Optional $1 filters cases by name.
set -eu
export TMPDIR=/run/user/1000/cbt/rust-tessel-dock/${AGENT:-f1}
mkdir -p "$TMPDIR"
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
BIN=$(cd "$ROOT" && cargo test -p tessel-dock --test f1_ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="f1_ui_headless" and m.get("executable"): print(m["executable"])')
[ -n "$BIN" ] || { echo "build failed"; exit 1; }
R=$TMPDIR/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg-config" "$R/cache" "$R/data" "$R/state" "$R/tmp" "$R/svc"; chmod 700 "$R/rt"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" \
    -e "s|<standard_session_servicedirs */>|<servicedir>$R/svc</servicedir>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
grep -q standard_session_servicedirs "$R/d.conf" && { echo "service dirs not removed"; exit 1; }
printf '<?xml version="1.0"?>\n<labwc_config><core><gap>0</gap></core></labwc_config>\n' > "$R/cfg/rc.xml"
: > "$R/cfg/autostart"
rm -f "$R/result" "$R/log"
{
  echo '#!/bin/sh'
  echo 'case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac'
  printf 'WAYDOCK_F1_HEADLESS=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory TMPDIR="%s" timeout %s' "$R/tmp" "${TIMEOUT:-240}"
  printf " '%s'" "$BIN" "${1:-}"
  printf ' > "%s/log" 2>&1; echo $? > "%s/result"\n' "$R" "$R"
  echo 'labwc --exit'
} > "$R/run.sh"
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 RUST_BACKTRACE=1 \
  XDG_CONFIG_HOME="$R/xdg-config" XDG_CACHE_HOME="$R/cache" XDG_DATA_HOME="$R/data" XDG_STATE_HOME="$R/state" \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log" 2>/dev/null || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
