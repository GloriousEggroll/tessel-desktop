#!/bin/sh
# tessel-hdr-calibration: time until the window is mapped (wlrctl sees
# app_id org.desktop.HDRCalibration) and idle RSS/PSS after 3 s, 2 runs each, in a
# private nested headless labwc with a private D-Bus and private XDG dirs.
# It reads only the private compositor's own status file ($R/rt/labwc-color-*.ini,
# which lists no HDR output) and is never clicked, so it writes no rc.xml,
# profiles or reports and signals nothing. Display/colour tools on PATH are
# logging fakes; hdr-pattern is a fake too.
set -eu
AGENT=${AGENT:-hdr-calibration}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/bin" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
for tool in way-displays wlr-randr kscreen-doctor xrandr hdr-pattern; do
  printf '#!/bin/sh\necho "%s $*" >> %s/calls.log\nexit 1\n' "$tool" "$R" > "$R/bin/$tool"; chmod +x "$R/bin/$tool"
done
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-hdr-calibration)
REL="$ROOT/target/release"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" PATH="$R/bin:\$PATH" TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config"
export XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" GSETTINGS_BACKEND=memory
export NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland GTK_USE_PORTAL=0
export HDR_CALIBRATION_PATTERN="$R/bin/hdr-pattern"
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() {
  name=\$1; shift
  t0=\$(ms); "\$@" >"$R/\$name.out" 2>&1 & pid=\$!
  until wlrctl toplevel find app_id:org.desktop.HDRCalibration >/dev/null 2>&1; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name window_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name idle_rss_kb=\$(rss \$pid)"
  grim "$R/\$name.png" 2>/dev/null || true
  kill -TERM \$pid; wait \$pid 2>/dev/null || true
  sleep 0.5
}
for i in 1 2; do
  measure rust "$REL/tessel-hdr-calibration"
done
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless \
  WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/tessel-hdr-calibration" | awk '{print $9, $5}'
grep -E "window_ms|rss" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
echo "private XDG files written:"; find "$R/xdg" -type f | sed "s|$R/||"
echo "rc.xml unchanged: $(cat "$R/cfg/rc.xml")"
echo "fake tool calls:"; cat "$R/calls.log" 2>/dev/null || echo "(none)"
