#!/bin/sh
# Switcher/overview widget tests and the real-daemon integration test
# (tests/switcher_ui.rs) in a private nested headless labwc with two outputs
# (tools/headless.md): own XDG_RUNTIME_DIR, own D-Bus, private XDG dirs and HOME.
#   run_headless_switcher.sh [test …]
set -eu
AGENT=${AGENT:-tessel-shell}
R=/run/user/1000/cbt/rust-$AGENT/h
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
FEATURES=${FEATURES-}
cd "$ROOT"
TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo build -q --release -p tessel-shell $FEATURES --bins
BIN=$(TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test --release -p tessel-shell $FEATURES --test switcher_ui --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="switcher_ui" and m.get("executable"): print(m["executable"])')
REL="$ROOT/target/release"
DAEMONCMD="$REL/tessel-shell"; KEYCLI="$REL/tessel-shell-cli"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0
export XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" WAYDOCK_TEST_ISOLATED=1
export WAYSHELL_DAEMON="$DAEMONCMD" WAYSHELL_KEY_CLI="$KEYCLI" DBUS_SYSTEM_BUS_ADDRESS=unix:path=/no-tessel-shell-test-system-bus
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
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
grep -vE "Gtk-WARNING|^$|gtk-modules" "$R/log" || true
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
