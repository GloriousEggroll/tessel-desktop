#!/bin/sh
# Called from run.sh inside the private compositor: the real tessel-keyboard binary,
# single-instance forwarding, exit codes, state file; MEASURE=1 compares with Python.
set -u
TARGET=$1 R=$2
case "$XDG_RUNTIME_DIR:$WAYLAND_DISPLAY" in /run/user/1000:*|*:) exit 1;; esac
STATE=$XDG_RUNTIME_DIR/tessel-keyboard/state.json
PY=$HOME/tessel-bluetooth/tessel-keyboard
fail=0
check() { if eval "$2"; then echo "ok service: $1"; else echo "FAILED service: $1"; fail=1; fi; }
rss() { awk '/VmRSS/{print $2}' /proc/$1/status; }
now() { date +%s%N; }
ready() { # pid: wait for state.json naming it
  i=0; while [ $i -lt 1000 ]; do
    python3 -c "import json,sys;sys.exit(json.load(open('$STATE'))['pid']!=$1)" 2>/dev/null && return 0
    i=$((i+1)); sleep 0.01; done; return 1; }
measure() { # label command...
  label=$1; shift; rm -f "$STATE"; start=$(now); "$@" --background & pid=$!
  ready $pid; end=$(now); sleep 3
  echo "measure $label: ready_ms=$(( (end-start)/1000000 )) idle_rss_kb=$(rss $pid)"
  "$@" --quit; wait $pid; }
export WAYKEYS_INPUT=$(ls -t "$TARGET"/build/on-screen-keyboard-*/out/tessel-keyboard-input | head -1)
"$TARGET/tessel-keyboard" --background & pid=$!
ready $pid; rc=$?; check "background publishes state" "[ $rc -eq 0 ]"
# Like Python, a primary started with a bad flag stays held; forwarded ones exit 2.
"$TARGET/tessel-keyboard" --bogus; rc=$?; check "unknown flag exits 2" "[ $rc -eq 2 ]"
check "idle: no helper" "! pgrep -P $pid >/dev/null"
check "idle state" "grep -q '\"visible\": false' $STATE"
"$TARGET/tessel-keyboard" --show; rc=$?; check "forwarded --show exits 0" "[ $rc -eq 0 ]"
sleep 1.5
check "shown via primary" "grep -q '\"visible\": true' $STATE && grep -q '\"ready\": true' $STATE"
check "helper running" "pgrep -P $pid -f tessel-keyboard-input >/dev/null"
check "status line" "timeout 1.5 $TARGET/tessel-keyboard-status | head -1 | grep -q '\"class\": \"active\"'"
"$TARGET/tessel-keyboard" --hide; sleep 3
check "hidden, helper reaped" "grep -q '\"visible\": false' $STATE && ! pgrep -P $pid -f tessel-keyboard-input >/dev/null"
"$TARGET/tessel-keyboard" --settings; sleep 1; "$TARGET/tessel-keyboard" --settings; sleep 0.5
"$TARGET/tessel-keyboard" --quit; wait $pid; rc=$?; check "quit" "[ $rc -eq 0 ]"
check "state removed at shutdown" "[ ! -e $STATE ]"
if [ -n "${MEASURE:-}" ]; then
  measure rust "$TARGET/tessel-keyboard"
  measure rust "$TARGET/tessel-keyboard"
  [ -f "$PY/osk_manager.py" ] && measure python /usr/bin/python3 "$PY/osk_manager.py"
  [ -f "$PY/osk_manager.py" ] && measure python /usr/bin/python3 "$PY/osk_manager.py"
  "$TARGET/tessel-keyboard-status" > /dev/null & s=$!; /usr/bin/python3 -u "$PY/osk_status.py" > /dev/null & p=$!; sleep 1.5
  echo "measure status: rust_rss_kb=$(rss $s) python_rss_kb=$(rss $p)"; kill $s $p
fi
exit $fail
