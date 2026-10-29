#!/bin/sh
# Measure time-to-ready and RSS/PSS of the Rust luna-clock vs the Python
# luna_clock.py, both started the same way inside a private headless labwc with
# a fake HOME and private bus (Python ONLY there; its sources are copied into
# the private dir with a private config.json — ~/btmanager is only read).
# Ready = the primary answers `--status` with "visible": true (probed with the
# Rust client, which forwards over org.gtk.Application to either primary).
set -eu
AGENT=${AGENT:-dashboard}
S=/run/user/1000/cbt/rust-$AGENT
R=$S/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/bin" "$R/app" "$R/home" "$R/py" \
  "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
printf '#!/bin/sh\necho "pactl $*" >> %s/tmp/calls.log\nexit 1\n' "$R" > "$R/bin/pactl"
printf '#!/bin/sh\necho "wpctl $*" >> %s/tmp/calls.log\n' "$R" > "$R/bin/wpctl"
printf '#!/bin/sh\necho "swayidle $*" >> %s/tmp/calls.log\nexec sleep 3600\n' "$R" > "$R/bin/swayidle"
chmod +x "$R/bin/"*
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$ROOT"
export TMPDIR=$S
cargo build --release -p home-assistant-screen --bins >"$S/build-release.log" 2>&1
cp target/release/luna-clock target/release/voice-controls "$R/app/"
CONFIG='{"wallpaper_directory": "'"$R"'/home", "wallpapers": [], "wallpaper_seconds": 300, "idle_seconds": 600, "voice_log": "'"$R"'/tmp/agent.log"}'
echo "$CONFIG" > "$R/app/config.json"
B=$HOME/btmanager
for d in shared lock-screen weather-manager on-screen-keyboard home-assistant-screen; do
  mkdir -p "$R/py/$d"; cp "$B/$d"/*.py "$R/py/$d/"
done
echo "$CONFIG" > "$R/py/home-assistant-screen/config.json"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export PATH="$R/bin:\$PATH" HOME="$R/home" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSETTINGS_BACKEND=memory \
 GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" \
 XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" XDG_CONFIG_DIRS="$R/none" XDG_DATA_DIRS="$R/xdg/data:/usr/share" LANG=C.UTF-8
measure() {
  name=\$1; shift
  for run in 1 2 3; do
    t0=\$(date +%s%N)
    "\$@" 2>"$R/tmp/\$name.err" & pid=\$!
    until gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner local.luna.Clock 2>/dev/null | grep -q true; do
      [ \$(( (\$(date +%s%N) - t0) / 1000000 )) -gt 20000 ] && break
      sleep 0.005
    done
    while :; do
      out=\$("$R/app/luna-clock" --status 2>/dev/null || true)
      case "\$out" in *'"visible": true'*) break;; esac
      [ \$(( (\$(date +%s%N) - t0) / 1000000 )) -gt 20000 ] && { echo "\$name timeout"; break; }
      sleep 0.01
    done
    t1=\$(date +%s%N)
    sleep 5
    rss=\$(awk '/^Rss:/{print \$2}' /proc/\$pid/smaps_rollup); pss=\$(awk '/^Pss:/{print \$2}' /proc/\$pid/smaps_rollup)
    echo "\$name run=\$run ready_ms=\$(( (t1 - t0) / 1000000 )) rss_kb=\$rss pss_kb=\$pss" >> "$R/result.txt"
    "$R/app/luna-clock" --quit >/dev/null 2>&1 || true
    for i in 1 2 3 4 5 6 7 8 9 10; do kill -0 \$pid 2>/dev/null || break; sleep 0.3; done
    kill \$pid 2>/dev/null || true; wait \$pid 2>/dev/null || true
  done
}
measure rust "$R/app/luna-clock"
measure python /usr/bin/python3 "$R/py/home-assistant-screen/luna_clock.py"
export GSK_RENDERER=cairo
measure rust-cairo "$R/app/luna-clock"
measure python-cairo /usr/bin/python3 "$R/py/home-assistant-screen/luna_clock.py"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" \
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/result.txt" 2>/dev/null
ls -l "$R/app/luna-clock" "$R/app/voice-controls" | awk '{print $5, $9}'
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit 0
