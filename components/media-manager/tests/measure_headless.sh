#!/bin/sh
# Time-to-ready (bus name owned) and RSS of the Rust vs Python media popup in a
# private nested headless labwc with a private D-Bus that holds one simulated
# MPRIS player (lock-screen's FakePlayer). Never touches the live session or
# the user's real players.
set -eu
AGENT=${AGENT:-media}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=${WS:-$(cd "$(dirname "$0")/../../.." && pwd)}
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p media-manager --bins)
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/media-manager
cat > "$R/player.py" <<'PYEOF'
import sys
sys.path[:0] = ['/home/tcrider/btmanager/lock-screen', '/home/tcrider/btmanager/shared']
from gi.repository import GLib
from test_lock_content import FakePlayer
player = FakePlayer('spotify', desktop_entry='spotify')
open(sys.argv[1], 'w').close()
GLib.MainLoop().run()
PYEOF
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data"
export GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland
WAYDOCK_TEST_ISOLATED=1 python3 "$R/player.py" "$R/player.ready" & PLAYER=\$!
while [ ! -e "$R/player.ready" ]; do sleep 0.01; done
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.waydock.Media 2>/dev/null | grep -q true; }
measure() { # name start-cmd...
  name=\$1; shift
  t0=\$(ms); "\$@" --popup --popup-edge bottom --popup-offset 76 >/dev/null 2>&1 & pid=\$!
  until owned; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name ready_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name rss_kb=\$(rss \$pid)"
  grim "$R/\$name.png" 2>/dev/null || true
  kill -TERM \$pid; wait \$pid 2>/dev/null || true
  while owned; do sleep 0.05; done
}
measure rust "$REL/media-manager"
measure python /usr/bin/python3 "$PY/media_manager.py"
kill -TERM \$PLAYER
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/media-manager" | awk '{print $9, $5}'
grep -E "ready_ms|rss_kb" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
