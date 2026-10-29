#!/bin/sh
# Rust vs Python Pictures: time until the window is mapped (wlrctl sees app_id
# org.waydock.Pictures) and idle RSS/PSS with the same fixture photo open, in a
# private nested headless labwc with a private D-Bus, private XDG dirs and a
# private Pictures folder. Never touches the live session.
set -eu
AGENT=${AGENT:-picture-viewer}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state" "$R/pictures"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
printf 'XDG_PICTURES_DIR="%s/pictures"\nXDG_DOWNLOAD_DIR="%s/pictures"\n' "$R" "$R" > "$R/xdg/config/user-dirs.dirs"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p picture-viewer)
REL="$ROOT/target/release"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS PYTHONDONTWRITEBYTECODE=1 python3 -I -c '
import sys
from PIL import Image, ImageDraw
for name, color in (("01-sunset.png", "#ba6d8d"), ("02-coast.jpg", "#5180a7")):
    picture = Image.new("RGB", (1600, 1000), color)
    draw = ImageDraw.Draw(picture)
    draw.ellipse((980, 180, 1280, 480), fill="#f5d29c")
    draw.polygon([(0, 1000), (600, 400), (1140, 1000)], fill="#31455a")
    picture.save(sys.argv[1] + "/" + name)
' "$R/pictures"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data"
export XDG_STATE_HOME="$R/xdg/state" GSETTINGS_BACKEND=memory PYTHONDONTWRITEBYTECODE=1
export NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland GTK_USE_PORTAL=0
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() {
  name=\$1; shift
  t0=\$(ms); "\$@" "$R/pictures/01-sunset.png" >"$R/\$name.out" 2>&1 & pid=\$!
  until wlrctl toplevel find app_id:org.waydock.Pictures >/dev/null 2>&1; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name window_ms=\$((\$(ms)-t0))"
  sleep 4; echo "\$name idle_rss_kb=\$(rss \$pid)"
  grim "$R/\$name.png" 2>/dev/null || true
  kill -TERM \$pid; wait \$pid 2>/dev/null || true
  sleep 0.5
}
for i in 1 2; do
  measure rust "$REL/waypictures"
  measure python /usr/bin/python3 /home/tcrider/btmanager/picture-viewer/picture_viewer.py
done
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless \
  WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
ls -l "$REL/waypictures" | awk '{print $9, $5}'
grep -E "window_ms|rss" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
