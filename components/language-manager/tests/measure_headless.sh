#!/bin/sh
# Measure the Rust language-manager vs the Python one in a private nested
# headless labwc whose private D-Bus is also the "system" bus, populated only
# by examples/fake_locale_services (fake localed/AccountsService, setters
# logged, never real). Only --status and opening the wizard are exercised:
# nothing is applied. XDG_CONFIG_HOME/HOME files are not read from the live
# session's config (private XDG_CONFIG_HOME). Never touches the live session.
set -eu
AGENT=language-manager
R=/run/user/1000/cbt/rust-$AGENT/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p language-manager --bins --examples)
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/language-manager
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache"
export GSETTINGS_BACKEND=memory NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GDK_BACKEND=wayland PYTHONDONTWRITEBYTECODE=1
"$REL/examples/fake_locale_services" "$R/fakes.ready" 2> "$R/fakes.log" & FAKES=\$!
while [ ! -e "$R/fakes.ready" ]; do sleep 0.01; done
ms() { date +%s%3N; }
rss() { echo "\$(awk '/VmRSS/ {print \$2}' /proc/\$1/status) PSS=\$(awk '/^Pss:/ {print \$2}' /proc/\$1/smaps_rollup)"; }
measure() { # name command...
  name=\$1; shift
  t0=\$(ms); "\$@" --status > "$R/\$name-status.json" 2>/dev/null; echo "\$name status_cli_ms=\$((\$(ms)-t0))"
  t0=\$(ms); "\$@" >/dev/null 2>&1 & pid=\$!
  until wlrctl toplevel find app_id:org.waydock.LanguageManager >/dev/null 2>&1; do sleep 0.01; [ \$((\$(ms)-t0)) -gt 15000 ] && break; done
  echo "\$name window_mapped_ms=\$((\$(ms)-t0))"
  sleep 3; echo "\$name idle_rss_kb=\$(rss \$pid)"
  grim "$R/\$name-choose.png" 2>/dev/null || true
  "\$@" --quit >/dev/null 2>&1 || kill -TERM \$pid; wait \$pid 2>/dev/null; echo "\$name exit=\$?"
  sleep 0.5
}
measure rust "$REL/language-manager"
measure python /usr/bin/python3 "$PY/language_manager.py"
kill -TERM \$FAKES; wait \$FAKES 2>/dev/null
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
ls -l "$REL/language-manager" | awk '{print $9, $5}'
grep -E "_ms=|rss|exit=" "$R/labwc.log" || { tail -30 "$R/labwc.log"; exit 1; }
cmp "$R/rust-status.json" "$R/python-status.json" && echo "status output identical"
grep -c "fake: Set" "$R/fakes.log" | sed 's/^/setter calls (must be 0): /' || true
