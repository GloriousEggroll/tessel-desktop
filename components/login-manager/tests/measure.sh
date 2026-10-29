#!/bin/sh
# PORTING.md 6: time-to-ready and RSS/PSS of `login-ui --preview` (release) vs
# Python `login_ui.py --preview`, both inside the same kind of private nested
# headless labwc (tools/headless.md) with a fake HOME and private buses. Python
# runs only here (python3 -I -B, no bytecode written to ~/btmanager). Also checks
# the release binary's CLI failure modes. Results: $R/measure.txt
set -eu
S=/run/user/1000/cbt/rust-login
R=$S/m
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/home" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
(cd "$ROOT" && env -u TMPDIR cargo build --release -p login-manager >/dev/null 2>&1)
RUST="$ROOT/target/release/login-ui"
PY=/home/tcrider/btmanager/login-manager/login_ui.py
printf '{"blur_strength": 500}\n' > "$R/bad.json"
# Measurement-only shim (both implementations alike): PR_SET_DUMPABLE=0 makes
# /proc/<pid>/smaps_rollup root-owned, so turn that one prctl into a no-op here.
cat > "$R/dumpable.c" <<'C'
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <sys/prctl.h>
int prctl(int option, ...) {
    va_list ap; va_start(ap, option);
    unsigned long a = va_arg(ap, unsigned long), b = va_arg(ap, unsigned long), c = va_arg(ap, unsigned long), d = va_arg(ap, unsigned long);
    va_end(ap);
    if (option == PR_SET_DUMPABLE) return 0;
    int (*real)(int, ...) = (int (*)(int, ...))dlsym(RTLD_NEXT, "prctl");
    return real(option, a, b, c, d);
}
C
cc -shared -fPIC -O2 -o "$R/dumpable.so" "$R/dumpable.c" -ldl
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export GSK_RENDERER=cairo LANG=C.UTF-8 HOME="$R/home" DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" GDK_BACKEND=wayland NO_AT_BRIDGE=1 \
  GTK_A11Y=none GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" \
  XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" \
  XDG_CONFIG_DIRS="$R/none" PYTHONDONTWRITEBYTECODE=1 PYTHONNOUSERSITE=1
out="$R/measure.txt"; : > "\$out"
"$RUST" --bogus > /dev/null 2> "$R/e1"; echo "rust --bogus exit \$? \$(tail -1 $R/e1)" >> "\$out"
"$RUST" > /dev/null 2> "$R/e2"; echo "rust no GREETD_SOCK exit \$? \$(cat $R/e2)" >> "\$out"
"$RUST" --config "$R/bad.json" --preview > /dev/null 2> "$R/e3"; echo "rust bad config exit \$? \$(cat $R/e3)" >> "\$out"
measure() {
  name=\$1; shift
  for round in 1 2 3; do
    start=\$(date +%s%N)
    LD_PRELOAD="$R/dumpable.so" "\$@" > "$R/\$name.log" 2>&1 &
    pid=\$!
    while ! wlrctl toplevel find title:"Sign In" >/dev/null 2>&1; do
      sleep 0.01
      kill -0 \$pid 2>/dev/null || { echo "\$name died" >> "\$out"; break; }
    done
    ready=\$(( (\$(date +%s%N) - start) / 1000000 ))
    sleep 3
    rss=\$(awk '/^Rss:/{print \$2}' /proc/\$pid/smaps_rollup); pss=\$(awk '/^Pss:/{print \$2}' /proc/\$pid/smaps_rollup)
    [ \$round = 1 ] && grim "$R/\$name.png"
    echo "\$name round \$round ready_ms \$ready rss_kb \$rss pss_kb \$pss" >> "\$out"
    kill \$pid; wait \$pid 2>/dev/null || true
    sleep 0.5
  done
}
measure rust "$RUST" --preview
measure python /usr/bin/python3 -I -B "$PY" --preview
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" \
  TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/measure.txt"
ls -l "$ROOT/target/release/login-ui" "$ROOT/target/release/login-settings-helper" "$ROOT/target/release/session-transition" | awk '{print $5, $9}'
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit 0
