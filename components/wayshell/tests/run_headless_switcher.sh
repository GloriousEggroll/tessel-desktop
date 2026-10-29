#!/bin/sh
# Switcher/overview widget tests and the real-daemon integration test
# (tests/switcher_ui.rs) in a private nested headless labwc with two outputs
# (tools/headless.md): own XDG_RUNTIME_DIR, own D-Bus, private XDG dirs and HOME.
#   run_headless_switcher.sh [test …]
#   DAEMON=python  runs the integration test against ~/btmanager/wayshell/wayshell
#                  with wayshell_cli.py (measurement / cross-check).
set -eu
AGENT=${AGENT:-wayshell}
R=/run/user/1000/cbt/rust-$AGENT/h
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
FEATURES=${FEATURES-}
cd "$ROOT"
TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p wayshell $FEATURES --bins
BIN=$(TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test --release -p wayshell $FEATURES --test switcher_ui --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="switcher_ui" and m.get("executable"): print(m["executable"])')
REL="$ROOT/target/release"
PY=/home/tcrider/btmanager/wayshell
if [ "${DAEMON:-rust}" = python ]; then
  DAEMONCMD="/usr/bin/python3 $PY/wayshell"; KEYCLI="/usr/bin/python3 $PY/wayshell_cli.py"
else
  DAEMONCMD="$REL/wayshell"; KEYCLI="$REL/wayshell-cli"
fi
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0
export XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" WAYDOCK_TEST_ISOLATED=1
export WAYSHELL_DAEMON="$DAEMONCMD" WAYSHELL_KEY_CLI="$KEYCLI" DBUS_SYSTEM_BUS_ADDRESS=unix:path=/no-wayshell-test-system-bus
export PATH="$R/fakebin:\$PATH"
timeout 240 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
[ -n "${EXTRA:-}" ] && timeout 60 ${EXTRA:-true} >> "$R/log" 2>&1
labwc --exit
RUN
chmod +x "$R/run.sh"
# Fakes: launches/video wallpapers must never reach real programs.
mkdir -p "$R/fakebin"
for tool in systemd-run mpvpaper wl-copy pkexec; do
  printf '#!/bin/sh\necho "$0 $*" >> %s/fake.log\n' "$R" > "$R/fakebin/$tool"; chmod +x "$R/fakebin/$tool"
done
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep -vE "Gtk-WARNING|^$|gtk-modules" "$R/log" || true
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
