#!/bin/sh
# Rust vs Python Files (tessel-files): time until the window is mapped (wlrctl sees
# app_id org.tessel.Files), idle RSS/PSS after 3 s on an empty folder, and
# RSS/PSS after browsing (a second invocation opens a 2,000-file folder with
# 120 PNG images in icon view, 4 s later), two runs each, in a private nested
# headless labwc with a private D-Bus (no service activation) and private
# HOME/XDG dirs. Python runs only inside this private environment
# (PYTHONDONTWRITEBYTECODE=1, fake HOME). Never touches the live session.
set -eu
AGENT=${AGENT:-file-manager}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/bin" "$R/tmp" "$R/home/empty" "$R/home/browse" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"
chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
for tool in systemd-run xdg-terminal-exec git; do
  printf '#!/bin/sh\necho "%s $*" >> "%s/calls.log"\nexit 1\n' "$tool" "$R" > "$R/bin/$tool"
done
chmod +x "$R/bin/"*
i=0; while [ $i -lt 2000 ]; do : > "$R/home/browse/file-$i.txt"; i=$((i+1)); done
find /usr/share/icons/hicolor -name '*.png' -size -64k 2>/dev/null | head -120 | while read -r p; do
  cp "$p" "$R/home/browse/img-$(echo "$p" | md5sum | cut -c1-8).png"; done
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-files)
REL="$ROOT/target/release"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export HOME="$R/home" TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data"
export XDG_STATE_HOME="$R/xdg/state" GSETTINGS_BACKEND=memory GIO_USE_VFS=local NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo
export GDK_BACKEND=wayland GTK_USE_PORTAL=0 GDK_DEBUG=no-portals PYTHONDONTWRITEBYTECODE=1 PATH="$R/bin:\$PATH"
ms() { date +%s%3N; }
mem() { echo "rss_kb=\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) pss_kb=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() {
  name=\$1; shift
  rm -rf "$R/xdg/config/tessel"
  t0=\$(ms); "\$@" --new-window "$R/home/empty" >"$R/\$name.out" 2>&1 & pid=\$!
  until wlrctl toplevel find app_id:org.tessel.Files >/dev/null 2>&1; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name window_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name idle \$(mem \$pid)"
  "\$@" "$R/home/browse" >>"$R/\$name.out" 2>&1
  sleep 4; echo "\$name browsing \$(mem \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null || true
  sleep 0.5
}
for i in 1 2; do
  measure rust "$REL/tessel-files"
  measure python /usr/bin/python3 /home/tcrider/tessel-bluetooth/tessel-files/file_manager.py
done
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" \
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
ls -l "$REL/tessel-files" | awk '{print $9, $5}'
grep -E "window_ms|rss" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
echo "fake calls:"; sort "$R/calls.log" 2>/dev/null | uniq -c || true
