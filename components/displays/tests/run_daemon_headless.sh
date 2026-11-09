#!/bin/sh
# tessel-displays --daemon in a PRIVATE nested headless labwc with two outputs
# (tools/headless.md): own XDG_RUNTIME_DIR, HOME and D-Bus. Never touches the
# live session. Prints each resulting layout, so another daemon can be compared:
#   DAEMON="/usr/bin/way-displays" SOCK=way-displays.sock CONF=way-displays run_daemon_headless.sh
set -eu
R=${RUNROOT:-/run/user/1000/cbt/rust-${AGENT:-displays-daemon}}/${SOCK:-tessel}
rm -rf "$R"; mkdir -p "$R/rt" "$R/cfg" "$R/home" "$R/tmp"; chmod 700 "$R/rt"
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
if [ -z "${DAEMON:-}" ]; then
    (cd "$ROOT" && cargo build -q -p tessel-displays)
    DAEMON="${CARGO_TARGET_DIR:-$ROOT/target}/debug/tessel-displays --daemon"
fi
SOCK=${SOCK:-tessel-displays.sock}
CONF=${CONF:-tessel/displays}
sed -e "s|<listen>.*</listen>|<listen>unix:tmpdir=$R</listen>|" -e "/<standard_session_servicedirs/d" /usr/share/dbus-1/session.conf > "$R/d.conf"
cat > "$R/ipc.py" <<PY
import socket,sys
s=socket.socket(socket.AF_UNIX); s.settimeout(8); s.connect("$R/rt/$SOCK")
s.sendall(sys.argv[1].encode().decode('unicode_escape').encode())
out=b""
while True:
    d=s.recv(65536)
    if not d: break
    out+=d
sys.stdout.write(out.decode())
PY
cat > "$R/run.sh" <<RUN
#!/bin/sh
case "\$XDG_RUNTIME_DIR:\$WAYLAND_DISPLAY" in /run/user/1000:*|*:) labwc --exit; exit 1;; esac
export HOME="$R/home" XDG_CONFIG_HOME="$R/home/.config" PATH="\$PATH"
C="\$XDG_CONFIG_HOME/$CONF"; mkdir -p "\$C"
cat > "\$C/cfg.yaml" <<'YAML'
ARRANGE: ROW
ALIGN: MIDDLE
ORDER:
- HEADLESS-2
- HEADLESS-1
SCALE:
  HEADLESS-2: 2
TRANSFORM:
  HEADLESS-1: 90
VRR_OFF:
- '!^HEADLESS'
LOG_THRESHOLD: INFO
CALLBACK_CMD: 'echo "\${CALLBACK_LEVEL}" >> $R/tmp/callback'
YAML
layout() { sleep 2; echo "== \$1"; wlr-randr --json | python3 -I -c '
import json,sys
for o in sorted(json.load(sys.stdin),key=lambda o:o["name"]):
    p=o.get("position") or {}
    print(o["name"],"on" if o["enabled"] else "off",o.get("scale"),o.get("transform"),p.get("x"),p.get("y"),o.get("adaptive_sync"))'; }
ipc() { python3 -I "$R/ipc.py" "\$1"; }
$DAEMON > "$R/tmp/daemon.log" 2>&1 &
D=\$!
layout "file: row, middle, order, scale, rotation"
ipc 'OP: GET\n' > "$R/tmp/get"
ipc 'OP: CFG_SET\nCFG:\n  ARRANGE: COLUMN\n  ALIGN: RIGHT\n  SCALE:\n    HEADLESS-1: 1.5\n' > "$R/tmp/set"
layout "CFG_SET: column, right, new scale"
ipc 'OP: CFG_DEL\nCFG:\n  SCALE:\n    HEADLESS-1: 1\n  TRANSFORM:\n    HEADLESS-1: 90\n' > "$R/tmp/del"
layout "CFG_DEL: scale and rotation removed"
ipc 'OP: CFG_SET\nCFG:\n  DISABLED:\n    HEADLESS-2:\n' > /dev/null
layout "CFG_SET: HEADLESS-2 disabled"
ipc 'OP: CFG_DEL\nCFG:\n  DISABLED:\n    HEADLESS-2:\n' > /dev/null
layout "CFG_DEL: HEADLESS-2 enabled again"
# Global Settings sends every saved rule back on each Apply: HEADLESS-2 must stay on.
RULE='OP: CFG_SET\nCFG:\n  DISABLED:\n    "!^HEADLESS-2":\n      IF:\n      - LID: CLOSED\n'
ipc "\$RULE" > /dev/null
layout "CFG_SET: conditional rule added, HEADLESS-2 stays on"
ipc "\$RULE" > /dev/null
layout "CFG_SET: same rule sent again, HEADLESS-2 stays on"
ipc 'OP: CFG_DEL\nCFG:\n  DISABLED:\n    "!^HEADLESS-2":\n' > /dev/null
layout "CFG_DEL: conditional rule removed"
ipc 'OP: CFG_WRITE\n' > "$R/tmp/write"
cp "\$C/cfg.yaml" "$R/tmp/written.yaml"
wlr-randr --output HEADLESS-1 --pos 5000,5000 >/dev/null
layout "another client moved HEADLESS-1: put back"
sed -i 's/ARRANGE: .*/ARRANGE: ROW/; s/ALIGN: .*/ALIGN: TOP/' "\$C/cfg.yaml"
layout "file edited: row, top"
kill \$D; wait \$D 2>/dev/null
echo "daemon exit \$?" > "$R/tmp/exit"
labwc --exit
RUN
chmod +x "$R/run.sh"
env -i GDK_DEBUG=no-portals GIO_USE_VFS=local GTK_USE_PORTAL=0 HOME="$R/home" PATH="$PATH" XDG_RUNTIME_DIR="$R/rt" TMPDIR="$R" \
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=2 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  timeout 120 dbus-run-session --config-file="$R/d.conf" -- labwc -C "$R/cfg" -s "$R/run.sh" > "$R/labwc.log" 2>&1 || true
grep '==\|^HEADLESS' "$R/labwc.log"
echo "$R"
