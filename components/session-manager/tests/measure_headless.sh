#!/bin/sh
# Time-to-popup, idle RSS and screenshots of the Rust session-manager vs the
# Python one in a private nested headless labwc with a private D-Bus that also
# serves as the "system" bus (no logind/DisplayManager on it, so every action
# is unavailable). Nothing is clicked: no action can run. systemctl/labwc
# lookups are not reached. Never touches the live session.
set -eu
AGENT=${AGENT:-session-manager}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p session-manager)
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/session-manager
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" XDG_CONFIG_HOME="$R/xdg/config"
export XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland

ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
# First buffer attached to the "desktop-applet" layer surface = popup on screen.
mapped() { awk '/get_layer_surface\(.*"desktop-applet"\)/ { if (match(\$0, /wl_surface#[0-9]+/)) s = substr(\$0, RSTART, RLENGTH) }
                s != "" && index(\$0, "-> " s ".attach(wl_buffer") { found = 1 } END { exit !found }' "\$1"; }
measure() { # name argv...
  name=\$1; shift
  for run in 1 2 3; do
    t0=\$(ms); WAYLAND_DEBUG=client "\$@" --popup > /dev/null 2> "$R/\$name.wl" & pid=\$!
    until mapped "$R/\$name.wl"; do sleep 0.005; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
    echo "\$name popup_mapped_ms=\$((\$(ms)-t0)) (run \$run, WAYLAND_DEBUG on)"
    kill -TERM \$pid; wait \$pid 2>/dev/null || true; sleep 0.3
  done
  "\$@" --popup > /dev/null 2>&1 & pid=\$!
  sleep 3; echo "\$name popup_idle_rss_kb=\$(rss \$pid)"
  grim "$R/\$name-popup.png" 2>/dev/null || true
  "\$@" --menu > /dev/null 2>&1; sleep 1.5
  grim "$R/\$name-menu.png" 2>/dev/null || true
  echo "\$name menu_rss_kb=\$(rss \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null || true; echo "\$name exit=\$?"
  sleep 0.5
}
measure rust env -u LABWC_PID "$REL/session-manager"
measure python env -u LABWC_PID /usr/bin/python3 "$PY/session_manager.py"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/session-manager" | awk '{print $9, $5}'
grep -E "_ms=|rss|exit=" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
