#!/bin/sh
# Run tests/headless.rs (ui, then daemon) inside a private nested headless
# labwc (tools/headless.md). Never touches the live session: own
# XDG_RUNTIME_DIR, own dbus-run-session bus, private HOME/XDG_STATE_HOME, no
# input devices. The daemon owns org.freedesktop.Notifications only on that
# private bus.
set -eu
AGENT=${AGENT:-tessel-notifications}
BASE=/run/user/1000/cbt/rust-$AGENT
R=$BASE/n
rm -rf "$R"
mkdir -p "$R/rt" "$R/cfg" "$R/home" "$R/state" "$R/helpers"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
artifacts() {
  (cd "$ROOT" && TMPDIR=$BASE cargo test -p tessel-notifications --test headless --no-run --message-format=json 2>/dev/null) \
  | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m.get("executable"): print(m["target"]["name"], m["executable"])'
}
OUT=$(artifacts)
BIN=$(echo "$OUT" | awk '$1=="headless"{print $2}')
NM_BIN=$(echo "$OUT" | awk '$1=="tessel-notifications"{print $2}')
[ -n "$BIN" ] && [ -n "$NM_BIN" ] || { echo "build failed"; exit 1; }
# The compositor helper (read-only use) and a fake X11 probe.
if [ -z "${TESSEL_DOCK_TOPLEVEL:-}" ]; then
  (cd "$ROOT" && TMPDIR=$BASE cargo build -q -p tessel-wayland --bin tessel-dock-toplevel)
  TESSEL_DOCK_TOPLEVEL=$ROOT/target/debug/tessel-dock-toplevel
fi
ln -sf "$TESSEL_DOCK_TOPLEVEL" "$R/helpers/tessel-dock-toplevel"
cat > "$R/helpers/tessel-notifications" <<'SH'
#!/bin/sh
[ "$*" = --x11-probe ] || exit 2
echo probe >> "$NM_FAKE_PROBE.log"
[ -f "$NM_FAKE_PROBE.delay" ] && sleep "$(cat "$NM_FAKE_PROBE.delay")"
cat "$NM_FAKE_PROBE.snapshot" 2>/dev/null || echo null
SH
chmod +x "$R/helpers/tessel-notifications"
rm -f "$R/result-ui" "$R/result-daemon"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export NM_HEADLESS=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo LANGUAGE=en
export HOME="$R/home" XDG_STATE_HOME="$R/state" XDG_CACHE_HOME="$R/home/.cache" XDG_CONFIG_HOME="$R/home/.config"
export NOTIFICATION_MANAGER_HELPER_DIR="$R/helpers" NM_FAKE_PROBE="$R/probe.json" NM_BIN="$NM_BIN" DISPLAY=:99
timeout 120 "$BIN" ui > "$R/log-ui" 2>&1; echo \$? > "$R/result-ui"
timeout 120 "$BIN" daemon > "$R/log-daemon" 2>&1; echo \$? > "$R/result-daemon"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log-ui" "$R/log-daemon" 2>/dev/null | grep -v "^$" || true
U=$(cat "$R/result-ui" 2>/dev/null || echo 1); D=$(cat "$R/result-daemon" 2>/dev/null || echo 1)
echo "ui=$U daemon=$D"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
[ "$U" = 0 ] && [ "$D" = 0 ]
