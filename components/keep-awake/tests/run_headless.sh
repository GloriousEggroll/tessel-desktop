#!/bin/sh
# Run tests/ui.rs inside a private nested headless labwc (tools/headless.md).
# Never touches the live session: own XDG_RUNTIME_DIR, own D-Bus, no input devices.
# CARGO_WS overrides the workspace to build in (default: the repository root).
set -eu
AGENT=keep-awake
R=/run/user/1000/cbt/rust-$AGENT/n
mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=${CARGO_WS:-$(cd "$(dirname "$0")/../../.." && pwd)}
BIN=$(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test -p keep-awake --test ui --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui" and m.get("executable"): print(m["executable"])')
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
KEEP_AWAKE_UI=1 DESKTOP_SUITE_LOCALE_DIR=/nonexistent GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo timeout 120 "$BIN" > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
