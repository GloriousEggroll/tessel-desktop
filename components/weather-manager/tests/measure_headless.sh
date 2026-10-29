#!/bin/sh
# Time-to-ready (bus name owned) and RSS of the Rust vs Python weather popup,
# tray and status stream in a private nested headless labwc with a private
# D-Bus. A fresh shared cache entry is written first, so neither version
# touches the network. Never touches the live session or the real ~/.config.
set -eu
AGENT=${AGENT:-weather}
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=${WS:-$(cd "$(dirname "$0")/../../.." && pwd)}
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p weather-manager --bins)
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/weather-manager
XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" python3 -I - "$PY" <<'PYEOF'
import sys, time, json
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo
sys.path.insert(0, sys.argv[1])
from weather_backend import ConfigStore, WeatherClient, WeatherConfig
from dataclasses import asdict
store = ConfigStore(); config = WeatherConfig("Denver", 39.7392, -104.9847, "America/Denver", "fahrenheit"); store.save(config)
client = WeatherClient(store); now = time.time(); today = datetime.fromtimestamp(now, ZoneInfo("America/Denver")).date()
days = [(today + timedelta(days=n)).isoformat() for n in range(-7, 16)]
response = {"current": {"time": today.isoformat() + "T12:00", "temperature_2m": 72, "weather_code": 2, "is_day": 1,
            "apparent_temperature": 71, "relative_humidity_2m": 40, "wind_speed_10m": 8},
            "daily": {"time": days, "temperature_2m_min": [50] * 23, "temperature_2m_max": [70] * 23,
                      "weather_code": [3] * 23, "precipitation_probability_max": [20] * 23}}
client.cache_dir.mkdir(parents=True, exist_ok=True)
client.cache_path(config).write_text(json.dumps({"version": 1, "config": asdict(config), "response": response,
                                                 "fetched_at": now, "attempted_at": now, "error": None}))
PYEOF
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data"
export GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.waydock.Weather 2>/dev/null | grep -q true; }
measure() { # name mode start-cmd...
  name=\$1; mode=\$2; shift 2
  t0=\$(ms); "\$@" \$mode >/dev/null 2>&1 & pid=\$!
  until owned; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name \$mode ready_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name \$mode rss_kb=\$(rss \$pid)"
  grim "$R/\$name\$mode.png" 2>/dev/null || true
  kill -TERM \$pid; wait \$pid 2>/dev/null || true
  while owned; do sleep 0.05; done
}
measure rust --popup "$REL/weather-manager"
measure python --popup /usr/bin/python3 "$PY/weather_manager.py"
measure rust --tray "$REL/weather-manager"
measure python --tray /usr/bin/python3 "$PY/weather_manager.py"
stream() { name=\$1; shift; t0=\$(ms); "\$@" --watch > "$R/\$name-stream.out" 2>/dev/null & pid=\$!
  until grep -q Denver "$R/\$name-stream.out" 2>/dev/null; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 10000 ] && break; done
  echo "\$name-status first_line_ms=\$((\$(ms)-t0))"; sleep 2; echo "\$name-status rss_kb=\$(rss \$pid)"; kill -TERM \$pid; wait \$pid 2>/dev/null || true; }
stream rust "$REL/weather-status"
stream python /usr/bin/python3 -u "$PY/weather_status.py"
cmp "$R/rust-stream.out" "$R/python-stream.out" && echo "status streams identical"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
ls -l "$REL/weather-manager" "$REL/weather-status" | awk '{print $9, $5}'
grep -E "ready_ms|rss_kb|line_ms|identical" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
