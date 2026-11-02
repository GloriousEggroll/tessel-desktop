#!/bin/sh
# Rust vs Python capture picker: release size, time from spawn until the
# org.tessel.CapturePicker toplevel is mapped (wlrctl), and RSS/PSS after 2 s,
# in a private nested headless labwc on a service-free private bus. Each run is
# ended with SIGTERM (= cancel, no output).
set -eu
R=/run/user/1000/cbt/rust-wscap/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e '/<standard_session_servicedirs/d' -e '/<servicedir>/d' \
  /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-ws-cap cargo build -q --release -p tessel-capture-picker)
REL="$ROOT/target/release/tessel-capture-picker"
PY=/home/tcrider/tessel-bluetooth/tessel-shell/capture_picker.py
printf 'Monitor: HEADLESS-1 Headless output\nWindow: Terminal (ext-1)\nWindow: Editor (ext-2)\n' > "$R/in"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland XDG_CACHE_HOME="$R/cache" XDG_CONFIG_HOME="$R/config"
ms() { date +%s%3N; }
mapped() { wlrctl toplevel list 2>/dev/null | grep -q org.tessel.CapturePicker; }
one() { name=\$1; shift; t0=\$(ms); "\$@" < "$R/in" > "$R/\$name.out" 2>/dev/null & pid=\$!
  until mapped; do sleep 0.005; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name mapped_ms=\$((\$(ms)-t0))"; sleep 2
  echo "\$name rss_kb=\$(awk '/VmRSS/ {print \$2}' /proc/\$pid/status) pss_kb=\$(awk '/^Pss:/ {print \$2}' /proc/\$pid/smaps_rollup)"
  kill -TERM \$pid; wait \$pid; echo "\$name exit=\$? stdout_bytes=\$(wc -c < "$R/\$name.out")"
  while mapped; do sleep 0.02; done; sleep 0.3; }
for i in 1 2 3; do one rust "$REL"; one python /usr/bin/python3 -B "$PY"; done
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL" | awk '{print "release_size_bytes=" $5}'
grep -E "mapped_ms|rss_kb|exit=" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
