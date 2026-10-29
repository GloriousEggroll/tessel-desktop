#!/bin/sh
# Run tests/ui_headless.rs inside a private nested headless labwc (tools/headless.md),
# one process per case (each registers the real application id on the private bus).
# Never touches the live session: own XDG_RUNTIME_DIR, own D-Bus, no input devices.
set -eu
AGENT=tiling-manager
R=/run/user/1000/cbt/rust-$AGENT/n
mkdir -p "$R/rt" "$R/cfg"; chmod 700 "$R/rt"
sed "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" /usr/share/dbus-1/session.conf > "$R/d.conf"
ROOT=${CARGO_WS:-$(cd "$(dirname "$0")/../../.." && pwd)}
BIN=$(cd "$ROOT" && TMPDIR=/run/user/1000/cbt/rust-$AGENT cargo test -p tiling-manager --test ui_headless --no-run --message-format=json 2>/dev/null \
      | python3 -c 'import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get("reason")=="compiler-artifact" and m["target"]["name"]=="ui_headless" and m.get("executable"): print(m["executable"])')
CASES="shortcut_popup_controls_and_search worker_thread_controls second_launch_activates_the_first_instance"
rm -f "$R/result" "$R/log"
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
status=0
for case in $CASES; do
  TILING_UI=\$case DESKTOP_SUITE_LOCALE_DIR=/nonexistent GDK_BACKEND=wayland NO_AT_BRIDGE=1 GTK_A11Y=none GSK_RENDERER=cairo \
    timeout 60 "$BIN" >> "$R/log" 2>&1 || { echo "FAILED \$case" >> "$R/log"; status=1; }
done
echo \$status > "$R/result"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" \
  > "$R/labwc.log" 2>&1 || true
cat "$R/log"
exit "$(cat "$R/result" 2>/dev/null || echo 1)"
