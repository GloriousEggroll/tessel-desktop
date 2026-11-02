#!/bin/sh
# Text editor: time until the window is mapped (wlrctl sees app_id
# org.tessel.TextEditor) and idle RSS/PSS, opening the same small file, 2 runs
# each, in a private nested headless labwc with a private D-Bus and private XDG
# dirs (recovery drafts, preferences and palette stay inside $R). Never touches
# the live session; only processes started here are signalled, by PID.
set -eu
AGENT=${AGENT:-text-editor}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-text-editor)
REL="$ROOT/target/release"
printf '#!/usr/bin/env python3\n"""Sample."""\n\n\ndef hello(name):\n    return f"Hello, {name}!"\n\n\nprint(hello("world"))\n' > "$R/sample.py"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data"
export XDG_STATE_HOME="$R/xdg/state" XDG_CONFIG_DIRS="$R/none" GSETTINGS_BACKEND=memory
export NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland GTK_USE_PORTAL=0
ms() { date +%s%3N; }
mem() { echo "rss_kb=\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) pss_kb=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() {
  name=\$1; shift
  t0=\$(ms); "\$@" "$R/sample.py" >"$R/\$name.out" 2>&1 & pid=\$!
  until wlrctl toplevel find app_id:org.tessel.TextEditor >/dev/null 2>&1; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name window_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name idle \$(mem \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null || true
  sleep 0.5
}
for i in 1 2; do
  measure rust "$REL/tessel-text-editor"
done
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless \
  WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/tessel-text-editor" | awk '{print "release binary", $9, $5, "bytes"}'
grep -E "window_ms|idle" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
