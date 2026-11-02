#!/bin/sh
# Measure time-to-ready, idle RSS and screenshots of the popup/settings for the
# Rust tessel-network vs the Python one, in a private nested headless labwc
# with a private D-Bus that is also the "system" bus, populated only by
# examples/fake_network_services (fake NetworkManager / Secret Service /
# notifications). Never touches the live session or the real NetworkManager.
set -eu
AGENT=${AGENT:-network-manager}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-network --bins --examples)
REL="$ROOT/target/release"
PY=/home/tcrider/tessel-bluetooth/tessel-network
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" XDG_CONFIG_HOME="$R/xdg/config"
export XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland
FAKE_NOTIFICATIONS=1 FAKE_NM_LOG="$R/fake-calls.log" "$REL/examples/fake_network_services" "$R/fakes.ready" & FAKES=\$!
while [ ! -e "$R/fakes.ready" ]; do sleep 0.01; done
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() { # name start-cmd check-cmd
  name=\$1; start=\$2; check=\$3
  t0=\$(ms); \$start --background >"$R/\$name.out" 2>"$R/\$name.err" & pid=\$!
  until \$check --check >"$R/\$name-check.out" 2>&1; do sleep 0.02; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name ready_ms=\$((\$(ms)-t0)) check=\$(cat $R/\$name-check.out)"
  sleep 3; echo "\$name idle_rss_kb=\$(rss \$pid)"
  \$check --popup --popup-edge bottom --popup-offset 76 >/dev/null 2>&1; sleep 1.5
  grim "$R/\$name-popup.png" 2>/dev/null || true
  \$check --settings >/dev/null 2>&1; sleep 1.5
  grim "$R/\$name-settings.png" 2>/dev/null || true
  echo "\$name rss_after_windows_kb=\$(rss \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null; echo "\$name exit=\$?"
  sleep 0.5
}
measure rust "$REL/tessel-network" "$REL/tessel-network"
measure python "/usr/bin/python3 $PY/network_manager.py" "/usr/bin/python3 $PY/network_manager.py"
stream() { name=\$1; shift; t0=\$(ms); "\$@" > "$R/\$name-stream.out" 2>/dev/null & pid=\$!
  until grep -q connected "$R/\$name-stream.out" 2>/dev/null; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 10000 ] && break; done
  echo "\$name-status first_connected_line_ms=\$((\$(ms)-t0))"; sleep 2; echo "\$name-status idle_rss_kb=\$(rss \$pid)"; kill -TERM \$pid; wait \$pid 2>/dev/null; }
stream rust "$REL/tessel-network-status"
stream python /usr/bin/python3 "$PY/network_status.py"
kill -TERM \$FAKES; wait \$FAKES 2>/dev/null
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/tessel-network" "$REL/tessel-network-status" | awk '{print $9, $5}'
grep -E "ready_ms|rss|exit=|line_ms" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
grep -E "Enable|Activate|Deactivate|Update2|Delete|SetManaged|RequestScan|CreateItem| Set " "$R/fake-calls.log" && echo "UNEXPECTED MUTATION" || echo "no mutations"
