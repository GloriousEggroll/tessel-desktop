#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc (tools/headless.md):
# own XDG_RUNTIME_DIR, own service-free D-Bus (no activatable services), no input
# devices, two headless outputs. Never touches the live session.
set -eu
R=/run/user/1000/cbt/rust-${AGENT:-wscap}/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e '/<standard_session_servicedirs/d' -e '/<servicedir>/d' \
  /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
export TMPDIR=/run/user/1000/cbt/rust-${AGENT:-ws-cap}
mkdir -p "$TMPDIR"
(cd "$ROOT" && cargo build -q -p tessel-wayland --bin tessel-dock-toplevel)
BIN=$(cd "$ROOT" && cargo test -p tessel-capture-picker --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
HELPER=$ROOT/target/debug/tessel-dock-toplevel
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
CAPTURE_UI=1 CAPTURE_UI_CASE="${CAPTURE_UI_CASE:-}" WAYSHELL_CAPTURE_TOPLEVEL_HELPER="$HELPER" DESKTOP_SUITE_LOCALE_DIR=/nonexistent \
  GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo XDG_CACHE_HOME="$R/cache" XDG_CONFIG_HOME="$R/config" \
  timeout 300 "$BIN" > "$R/log" 2>&1
echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
