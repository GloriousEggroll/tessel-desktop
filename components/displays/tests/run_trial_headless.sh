#!/bin/sh
# tests/trial_real_daemon.rs with the real tessel-displays --daemon in a PRIVATE
# nested headless labwc with two outputs (tools/headless.md): own XDG_RUNTIME_DIR,
# HOME and D-Bus. Never touches the live session.
set -eu
AGENT=${AGENT:-displays-trial}
R=/run/user/1000/cbt/rust-$AGENT/t
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/home/.config/tessel/displays" "$R/tmp" "$R/shots"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
(cd "$ROOT" && cargo build -q -p tessel-displays)
BIN=$(cd "$ROOT" && cargo test -p tessel-displays --test trial_real_daemon --no-run --message-format=json 2>/dev/null \
      | jq -r 'select(.reason == "compiler-artifact" and .target.name == "trial_real_daemon" and .executable) | .executable')
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
cat > "$R/home/.config/tessel/displays/cfg.yaml" <<'YAML'
ARRANGE: ROW
ALIGN: TOP
LOG_THRESHOLD: INFO
DISABLED:
  '!^HEADLESS-2':
    IF:
    - LID: CLOSED
YAML
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export HOME="$R/home" XDG_CONFIG_HOME="$R/home/.config" XDG_CACHE_HOME="$R/home/.cache" XDG_DATA_HOME="$R/home/.local/share" XDG_STATE_HOME="$R/home/.local/state"
"$TARGET/debug/tessel-displays" --daemon > "$R/daemon.log" 2>&1 &
D=\$!
sleep 2
TESSEL_DISPLAYS_TRIAL_ISOLATED=1 TRIAL_SHOTS="$R/shots" TRIAL_LABWC_RC="$R/cfg/rc.xml" DBUS_SYSTEM_BUS_ADDRESS="\$DBUS_SESSION_BUS_ADDRESS" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none \
GSK_RENDERER=cairo GSETTINGS_BACKEND=memory TMPDIR="$R/tmp" timeout 90 "$BIN" > "$R/log" 2>&1; echo \$? > "$R/result"
kill \$D; wait \$D 2>/dev/null
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" \
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  timeout 120 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep -E '^ok |all checks|panicked|timed out|assert' "$R/log" || tail -20 "$R/log"
echo "result: $(cat "$R/result" 2>/dev/null || echo none) ($R)"
[ "$(cat "$R/result" 2>/dev/null)" = 0 ]
