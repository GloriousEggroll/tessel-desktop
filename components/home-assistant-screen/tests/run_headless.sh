#!/bin/sh
# Run tests/dashboard_headless.rs inside a private nested headless labwc
# (tools/headless.md). Never touches the live session: own XDG_RUNTIME_DIR and
# Wayland socket, own D-Bus (session.conf WITHOUT standard service dirs; the run
# FAILS if anything is bus-activated), fake HOME and XDG dirs, no input devices.
# pactl/wpctl/systemctl/swayidle are logging fakes first on PATH; the Kasumi
# scheduler socket, voice log and admin web server are fakes inside the test
# (random 127.0.0.1 ports / private paths). Extra arguments filter check names.
set -eu
AGENT=${AGENT:-dashboard}
S=/run/user/1000/cbt/rust-$AGENT
R=$S/n
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/tmp" "$R/bin" "$R/app" "$R/home" \
  "$R/xdg/config" "$R/xdg/cache" "$R/xdg/data" "$R/xdg/state"; chmod 700 "$R/rt"
printf '<labwc_config/>\n' > "$R/cfg/rc.xml"
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
CALLS="$R/tmp/calls.log"
# Fake pactl: one sink "fake_sink", volume/mute kept in $R/tmp/sink.
printf '50 false\n' > "$R/tmp/sink"
cat > "$R/bin/pactl" <<FAKE
#!/bin/sh
echo "pactl \$*" >> "$CALLS"
read v m < "$R/tmp/sink"
case "\$*" in
  get-default-sink) echo fake_sink;;
  "--format=json list sinks") r=\$((v * 65536 / 100)); echo "[{\"name\":\"fake_sink\",\"mute\":\$m,\"volume\":{\"front-left\":{\"value\":\$r},\"front-right\":{\"value\":\$r}}}]";;
  "set-sink-volume fake_sink "*) n=\${3%\%}; echo "\$n \$m" > "$R/tmp/sink";;
  "set-sink-mute fake_sink 1") echo "\$v true" > "$R/tmp/sink";;
  "set-sink-mute fake_sink 0") echo "\$v false" > "$R/tmp/sink";;
  "set-sink-mute fake_sink toggle") if [ "\$m" = true ]; then echo "\$v false"; else echo "\$v true"; fi > "$R/tmp/sink";;
  *) exit 1;;
esac
FAKE
printf '#!/bin/sh\necho "wpctl $*" >> "%s"\n' "$CALLS" > "$R/bin/wpctl"
printf '#!/bin/sh\necho "systemctl $*" >> "%s"\nexit 0\n' "$CALLS" > "$R/bin/systemctl"
cat > "$R/bin/swayidle" <<FAKE
#!/bin/sh
echo "swayidle \$*" >> "$CALLS"
trap 'echo "swayidle exited" >> "$CALLS"; exit 0' TERM INT
while :; do sleep 1; done
FAKE
chmod +x "$R/bin/"*
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$ROOT"
export TMPDIR=$S
cargo build -p home-assistant-screen --bins >"$S/build.log" 2>&1
BIN=$(cargo test -p home-assistant-screen --test dashboard_headless --no-run --message-format=json 2>>"$S/build.log" \
      | python3 -I -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="dashboard_headless" and m.get("executable"): print(m["executable"])')
HELPER=$(ls -t "$ROOT"/target/debug/build/on-screen-keyboard-*/out/tessel-keyboard-input 2>/dev/null | head -1)
cp target/debug/luna-clock target/debug/voice-controls "$R/app/"
[ -n "$HELPER" ] && cp "$HELPER" "$R/app/tessel-keyboard-input"
printf '{"wallpaper_directory": "%s/home", "wallpapers": [], "wallpaper_seconds": 300, "idle_seconds": 60, "voice_log": "%s/tmp/agent.log"}\n' "$R" "$R" > "$R/app/config.json"
rm -f "$R/result"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
PATH="$R/bin:\$PATH" HOME="$R/home" KASUMI_HEADLESS=1 KASUMI_TEST_TMP="$R/tmp" KASUMI_CALLS="$CALLS" KASUMI_APP_DIR="$R/app" \
WAYKEYS_INPUT="$R/app/tessel-keyboard-input" GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo GSETTINGS_BACKEND=memory \
GTK_USE_PORTAL=0 GDK_DEBUG=no-portals GIO_USE_VFS=local TMPDIR="$R/tmp" XDG_CONFIG_HOME="$R/xdg/config" XDG_CACHE_HOME="$R/xdg/cache" \
XDG_DATA_HOME="$R/xdg/data" XDG_STATE_HOME="$R/xdg/state" XDG_CONFIG_DIRS="$R/none" XDG_DATA_DIRS="$R/xdg/data:/usr/share" \
LANG=C.UTF-8 LC_ALL= LANGUAGE= DESKTOP_SUITE_LOCALE_DIR="$R/none" \
timeout 600 "$BIN" $* > "$R/log" 2>&1; echo \$? > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u TMPDIR -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" \
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep -v "^\(Gtk\|Gdk\)-\(DEBUG\|INFO\)" "$R/log" | grep -E "^(ok|FAIL|[0-9]+ passed)" || true
grep "Activating service" "$R/labwc.log" && { echo "UNEXPECTED D-Bus activation on the private bus"; exit 1; }
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
