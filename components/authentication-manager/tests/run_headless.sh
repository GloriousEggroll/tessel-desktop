#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc (tools/headless.md).
# Never touches the live session: own XDG_RUNTIME_DIR, own dbus-daemon (also used as
# the "system" bus, where the test serves a FAKE PolicyKit1 Authority), private XDG
# dirs, no input devices. No PAM helper / polkitd / real credentials are involved.
# MEASURE=1 also times the release binary and the Python agent the same way.
# CARGO (default: cargo) may point at a wrapper (e.g. a private overlay workspace).
set -eu
AGENT=authentication-manager
S=/run/user/1000/cbt/rust-$AGENT
R=$S/n
mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
CARGO=${CARGO:-cargo}
BIN=$(cd "$ROOT" && TMPDIR=$S $CARGO test -p $AGENT --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
EXTRA=""
if [ "${MEASURE:-0}" = 1 ]; then
  (cd "$ROOT" && TMPDIR=$S $CARGO build --release -p $AGENT >/dev/null 2>&1)
  EXTRA="AUTH_RELEASE_BIN=$ROOT/target/release/authentication-manager AUTH_PYTHON_SCRIPT=$HOME/btmanager/authentication-manager/authentication_manager.py"
fi
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
env DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" AUTH_UI_ISOLATED_TEST=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none \
GSK_RENDERER=cairo GSETTINGS_BACKEND=memory XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" \
TMPDIR="$S" $EXTRA timeout 300 "$BIN" > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log"
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
