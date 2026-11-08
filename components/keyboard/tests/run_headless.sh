#!/bin/sh
# Run tests/ui.rs (and a service/measurement phase) inside a private nested
# headless labwc with two outputs (tools/headless.md). Never touches the live
# session: own XDG_RUNTIME_DIR, own D-Bus, own XDG config/cache/data dirs.
# tessel-keyboard-input (virtual keyboard + input method) only ever connects here.
#   tests/run_headless.sh [case-filter...]      MEASURE=1 also measures startup and memory.
set -eu
AGENT=${AGENT:-tessel-keyboard}
R=/run/user/1000/cbt/rust-$AGENT/n
LABWC=${LABWC:-labwc}
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
# No standard service dirs: the private bus must never D-Bus-activate a real portal, gvfs or other session service.
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
export TMPDIR=/run/user/1000/cbt/rust-$AGENT
(cd "$ROOT" && cargo build -q -p tessel-keyboard ${PROFILE:+--profile $PROFILE})
BIN=$(cd "$ROOT" && cargo test -p tessel-keyboard --test ui --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui" and m.get("executable"): print(m["executable"])')
TARGET=$ROOT/target/${PROFILE:-debug}
[ "${PROFILE:-}" = dev ] && TARGET=$ROOT/target/debug
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export XDG_CONFIG_HOME=$R/xdg/config XDG_CACHE_HOME=$R/xdg/cache XDG_DATA_HOME=$R/xdg/data XDG_STATE_HOME=$R/xdg/state
export GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0
TESSEL_KEYBOARD_UI=1 timeout 300 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
if [ -n "${SERVICE:-1}" ]; then sh "$ROOT/components/keyboard/tests/service_phase.sh" "$TARGET" "$R" >> "$R/log" 2>&1 || echo 1 > "$R/result"; fi
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 \
  HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 MEASURE="${MEASURE:-}" LANG="${LANG:-C.UTF-8}" \
  dbus-run-session --config-file="$R/d.conf" -- "$LABWC" -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
cat "$R/log"
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
