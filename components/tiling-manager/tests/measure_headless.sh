#!/bin/sh
# Rust vs Python Waytile: release size, `--status` time, the resident
# `--watch` stream's RSS (Waydock runs it) and the shortcut popup's
# time-to-registered and RSS, in a private nested headless labwc. The tiling
# socket is examples/fake_compositor in the private runtime dir; rc.xml is
# never written (no mutating flags are run).
set -eu
AGENT=tiling-manager
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/state"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tiling-manager --bins --examples)
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/tiling-manager
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_STATE_HOME="$R/xdg/state"
"$REL/examples/fake_compositor" "$R/fake.ready" & FAKE=\$!
while [ ! -e "$R/fake.ready" ]; do sleep 0.01; done
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.waydock.Tiling 2>/dev/null | grep -q true; }
status() { name=\$1; shift; t0=\$(ms); for i in 1 2 3 4 5; do "\$@" --status > "$R/\$name.status"; done; echo "\$name status_avg_ms=\$(((\$(ms)-t0)/5))"; }
watch() { name=\$1; shift; t0=\$(ms); "\$@" > "$R/\$name.watch" 2>/dev/null & pid=\$!
  until [ -s "$R/\$name.watch" ]; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 10000 ] && break; done
  echo "\$name watch_first_line_ms=\$((\$(ms)-t0))"; sleep 3; echo "\$name watch_rss_kb=\$(rss \$pid)"; kill -TERM \$pid; wait \$pid 2>/dev/null || true; }
popup() { name=\$1; shift; t0=\$(ms); "\$@" --settings >/dev/null 2>&1 & pid=\$!
  until owned; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 10000 ] && break; done
  echo "\$name popup_registered_ms=\$((\$(ms)-t0))"; sleep 3; grim "$R/\$name-popup.png" 2>/dev/null || true
  echo "\$name popup_rss_kb=\$(rss \$pid)"; kill -TERM \$pid; wait \$pid 2>/dev/null || true; sleep 0.5; }
status rust "$REL/waytile"
status python /usr/bin/python3 -B "$PY/tiling_manager.py"
cmp "$R/rust.status" "$R/python.status" && echo "status_output identical"
watch rust "$REL/tiling-status"
watch python /usr/bin/python3 -B "$PY/tiling_status.py"
cmp "$R/rust.watch" "$R/python.watch" && echo "watch_output identical"
popup rust "$REL/waytile"
popup python /usr/bin/python3 -B "$PY/tiling_manager.py"
kill -TERM \$FAKE; wait \$FAKE 2>/dev/null || true
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/waytile" "$REL/tiling-status" | awk '{print $9, $5}'
grep -E "_ms=|rss_kb|identical|differ" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
