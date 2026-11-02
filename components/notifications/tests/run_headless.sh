#!/bin/sh
# Run tests/headless.rs (ui, then daemon) inside a private nested headless
# labwc (tools/headless.md). Never touches the live session: own
# XDG_RUNTIME_DIR, own dbus-run-session bus, private HOME/XDG_STATE_HOME, no
# input devices. The daemon owns org.freedesktop.Notifications only on that
# private bus.
set -eu
AGENT=${AGENT:-notification-manager}
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
# The compositor helper from Tessel Dock (read-only use) and a fake X11 probe.
HELPER=${WAYDOCK_TOPLEVEL:-$HOME/tessel-bluetooth/tessel-dock/tessel-dock-toplevel}
ln -sf "$HELPER" "$R/helpers/tessel-dock-toplevel"
cat > "$R/helpers/fullscreen_guard.py" <<'PY'
import json, os, time
config = os.environ.get("NM_FAKE_PROBE", "")
with open(config + ".log", "a") as log:
    log.write("probe\n")
try:
    with open(config) as stream:
        data = json.load(stream)
except (OSError, ValueError):
    data = {}
time.sleep(float(data.get("delay", 0)))
print(json.dumps(data.get("snapshot")))
PY
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
