#!/bin/sh
# Measure time-to-ready and idle RSS of the Rust tessel-calendar vs the Python
# one in a private nested headless labwc. A private D-Bus serves as session AND
# "system" bus, where examples/fake_timedate provides a FAKE timedate1. Ready =
# the primary answers org.gtk.Actions.List on its object (startup done, the
# --popup command line handled, popup shown). Never touches the live session.
set -eu
AGENT=${AGENT:-calendar-manager}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/tmp"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-calendar --bins --examples)
REL="$ROOT/target/release"
PY=/home/tcrider/tessel-bluetooth/tessel-calendar
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache"
export XDG_DATA_HOME="$R/xdg/data" GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland
export PYTHONDONTWRITEBYTECODE=1 TMPDIR="$R/tmp" LANG=C.UTF-8
"$REL/examples/fake_timedate" "$R/fake.ready" & FAKE=\$!
while [ ! -e "$R/fake.ready" ]; do sleep 0.01; done
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
ready() { gdbus call --session --dest org.tessel.CalendarManager --object-path /org/tessel/CalendarManager \
          --method org.gtk.Actions.List >/dev/null 2>&1; }
measure() { # name launcher...
  name=\$1; shift
  t0=\$(ms); "\$@" --popup --popup-edge bottom --popup-offset 76 >/dev/null 2>&1 & pid=\$!
  until ready; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name ready_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name popup_idle_rss_kb=\$(rss \$pid)"
  grim "$R/\$name-popup.png" 2>/dev/null || true
  t1=\$(ms); "\$@" --settings >/dev/null 2>&1; echo "\$name settings_roundtrip_ms=\$((\$(ms)-t1))"
  sleep 2; grim "$R/\$name-settings.png" 2>/dev/null || true
  echo "\$name rss_after_settings_kb=\$(rss \$pid)"
  "\$@" --popup >/dev/null 2>&1; sleep 1; echo "\$name rss_after_toggle_kb=\$(rss \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null; echo "\$name exit=\$?"
  sleep 0.5
}
measure rust "$REL/tessel-calendar"
measure python /usr/bin/python3 "$PY/calendar_manager.py"
kill -TERM \$FAKE; wait \$FAKE 2>/dev/null
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/tessel-calendar" | awk '{print $9, $5}'
grep -E "ready_ms|rss|exit=|roundtrip" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
