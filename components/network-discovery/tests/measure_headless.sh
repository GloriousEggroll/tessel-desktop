#!/bin/sh
# Rust vs Python network-discovery: release size, `--status` time, popup
# time-to-registered and RSS, in a private nested headless labwc whose private
# D-Bus also serves as the "system" bus with examples/fake_systemd. Avahi is
# never queried or changed (avahi-browse cannot reach a daemon on that bus).
set -eu
AGENT=network-discovery
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p network-discovery --bins --examples)
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/network-discovery/discovery_manager.py
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland
"$REL/examples/fake_systemd" "$R/fake.ready" & FAKE=\$!
while [ ! -e "$R/fake.ready" ]; do sleep 0.01; done
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.waydock.NetworkDiscovery 2>/dev/null | grep -q true; }
status() { name=\$1; shift; t0=\$(ms); for i in 1 2 3 4 5; do "\$@" --status > "$R/\$name.status"; done; echo "\$name status_avg_ms=\$(((\$(ms)-t0)/5)) \$(cat "$R/\$name.status")"; }
popup() { name=\$1; shift; t0=\$(ms); "\$@" --popup >/dev/null 2>&1 & pid=\$!
  until owned; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 10000 ] && break; done
  echo "\$name popup_registered_ms=\$((\$(ms)-t0))"; sleep 2.5; grim "$R/\$name-popup.png" 2>/dev/null || true
  echo "\$name popup_rss_kb=\$(rss \$pid)"; kill -TERM \$pid; wait \$pid 2>/dev/null || true; sleep 0.5; }
status rust "$REL/network-discovery"
status python /usr/bin/python3 -B "$PY"
popup rust "$REL/network-discovery"
popup python /usr/bin/python3 -B "$PY"
kill -TERM \$FAKE; wait \$FAKE 2>/dev/null || true
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/network-discovery" | awk '{print $9, $5}'
grep -E "_ms=|rss_kb" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
