#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc (tools/headless.md).
# Never touches the live session: own XDG_RUNTIME_DIR, own D-Bus (also used as the
# "system" bus), private XDG config/cache dirs, no input devices, no Internet use
# (only simulated MPRIS players exist on the private bus). Optional $1 filters tests.
# WS=<dir> builds from another workspace root (default: this repository).
set -eu
AGENT=${AGENT:-media}
R=/run/user/1000/cbt/rust-$AGENT/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/tmp"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=${WS:-$(cd "$(dirname "$0")/../../.." && pwd)}
BIN=$(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test -p media-manager --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
FILTER=${1:-}
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" MEDIA_PRIVATE_BUS=1 GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none \
GSK_RENDERER=cairo GSETTINGS_BACKEND=memory XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" XDG_DATA_HOME="$R/xdg/data" \
TMPDIR="$R/tmp" timeout 180 "$BIN" $FILTER > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log"
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
