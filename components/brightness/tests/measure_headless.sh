#!/bin/sh
# Measure time-to-ready, idle RSS and the popup of the Rust tessel-brightness
# vs the Python one in a private nested headless labwc with a private D-Bus
# (also the "system" bus; no logind there, so auto-dim guards stay blocked).
# wlr-randr/ddcutil/swayidle are fakes first on PATH (ddcutil only reads/writes
# a state file); Rust uses a fake sysfs, Python only *reads* the real
# /sys/class/drm (which cannot match the fake output). No --settings launch,
# no hardware writes, never touches the live session.
set -eu
AGENT=${AGENT:-brightness-manager}
S=/run/user/1000/cbt/rust-$AGENT
R=$S/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/bin" "$R/log" "$R/vcp/12" "$R/sys/drm/card1-DP-TEST/i2c-12" "$R/sys/backlight" \
  "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=$S cargo build -q --release -p tessel-brightness --bins)
REL="$ROOT/target/release"
PY=/home/tcrider/tessel-bluetooth/tessel-brightness
echo connected > "$R/sys/drm/card1-DP-TEST/status"
python3 -c 'import sys; open(sys.argv[1],"wb").write(b"\x00\xff\xff\xff\xff\xff\xff\x00"+bytes([5])*120)' "$R/sys/drm/card1-DP-TEST/edid"
echo "75 100" > "$R/vcp/12/10"; echo "50 100" > "$R/vcp/12/12"
echo '[{"name": "DP-TEST", "description": "Synthetic display", "enabled": true, "position": {"x": 0, "y": 0}, "scale": 1.0, "modes": [{"width": 1920, "height": 1080, "current": true}]}]' > "$R/outputs.json"
printf '#!/bin/sh\nexec cat %s/outputs.json\n' "$R" > "$R/bin/wlr-randr"
cat > "$R/bin/ddcutil" <<DDC
#!/bin/sh
echo "\$*" >> $R/log/ddcutil
bus=""; op=""
while [ \$# -gt 0 ]; do case "\$1" in --bus) bus=\$2; shift;; getvcp|setvcp) op=\$1; shift; break;; esac; shift; done
dir=$R/vcp/\$bus
if [ "\$op" = getvcp ]; then
  [ -d "\$dir" ] || { echo "DDC address 0x37 did not respond" >&2; exit 1; }
  for code in "\$@"; do [ "\$code" = --terse ] && continue
    if [ -f "\$dir/\$code" ]; then read raw max < "\$dir/\$code"; echo "VCP \$code C \$raw \$max"; else echo "VCP \$code ERR"; fi
  done
elif [ "\$op" = setvcp ]; then read raw max < "\$dir/\$1"; echo "\$2 \$max" > "\$dir/\$1"; fi
DDC
printf '#!/bin/sh\nexec sleep 300\n' > "$R/bin/swayidle"
chmod +x "$R/bin/"*
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" PATH="$R/bin:\$PATH" XDG_CONFIG_HOME="$R/xdg/config" XDG_STATE_HOME="$R/xdg/state"
export XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo
export GDK_BACKEND=wayland BRIGHTNESS_MANAGER_SYSFS="$R/sys" PYTHONDONTWRITEBYTECODE=1
unset XDG_SESSION_ID
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() { # name command
  name=\$1; cmd=\$2
  t0=\$(ms); \$cmd --background >/dev/null 2>&1 & pid=\$!
  until \$cmd --check >/dev/null 2>&1; do sleep 0.02; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name ready_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name idle_rss_kb=\$(rss \$pid)"
  \$cmd --popup --popup-edge bottom --popup-offset 76 >/dev/null 2>&1; sleep 1.5
  grim "$R/\$name-popup.png" 2>/dev/null || true
  echo "\$name rss_after_popup_kb=\$(rss \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null; echo "\$name exit=\$?"
  sleep 0.5
}
measure rust "$REL/tessel-brightness"
measure python "/usr/bin/python3 $PY/brightness_manager.py"
stream() { name=\$1; svc=\$2; shift 2; \$svc --background >/dev/null 2>&1 & spid=\$!
  until \$svc --check >/dev/null 2>&1; do sleep 0.02; done
  t0=\$(ms); "\$@" > "$R/\$name-stream.out" 2>/dev/null & pid=\$!
  until grep -q manual "$R/\$name-stream.out" 2>/dev/null; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 10000 ] && break; done
  echo "\$name-status first_state_line_ms=\$((\$(ms)-t0))"; sleep 2; echo "\$name-status idle_rss_kb=\$(rss \$pid)"
  kill -TERM \$pid; wait \$pid 2>/dev/null; kill -TERM \$spid; wait \$spid 2>/dev/null; sleep 0.5; }
stream rust "$REL/tessel-brightness" "$REL/tessel-brightness-status"
stream python "/usr/bin/python3 $PY/brightness_manager.py" /usr/bin/python3 "$PY/brightness_status.py"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
ls -l "$REL/tessel-brightness" "$REL/tessel-brightness-status" | awk '{print $9, $5}'
grep -E "ready_ms|rss|exit=|line_ms" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
grep -c setvcp "$R/log/ddcutil" 2>/dev/null | sed 's/^/setvcp calls (must be 0): /' || echo "setvcp calls (must be 0): 0"
