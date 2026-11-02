#!/bin/sh
# Called from run_headless.sh (MEASURE=1) INSIDE the private nested labwc, with
# examples/fake_bluez serving the private "system" and session buses.
# Measures time-to-ready (D-Bus name owned) and idle RSS of the binary as `--popup`. Usage: measure.sh <rust-bin> <dir>
set -u
RUST=$1; R=$2
ms() { date +%s%3N; }
owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
          --method org.freedesktop.DBus.NameHasOwner org.tessel.Bluetooth 2>/dev/null | grep -q true; }
rss() { echo "VmRSS=$(awk '/VmRSS/ {print $2}' /proc/$1/status)kB PSS=$(awk '/^Pss:/ {print $2}' /proc/$1/smaps_rollup)kB"; }
measure() { name=$1; shift
  t0=$(ms); "$@" --popup >/dev/null 2>&1 & pid=$!
  until owned; do sleep 0.01; [ $(($(ms)-t0)) -gt 15000 ] && break; done
  echo "$name ready_ms=$(($(ms)-t0))"
  sleep 3; echo "$name idle_$(rss $pid)"
  grim "$R/$name-popup.png" 2>/dev/null || true
  t1=$(ms); "$@" --popup >/dev/null 2>&1; echo "$name second_launch_ms=$(($(ms)-t1))"
  kill -TERM $pid; wait $pid 2>/dev/null
  while owned; do sleep 0.05; done
}
measure rust "$RUST"
ls -l "$RUST" | awk '{print "rust binary bytes=" $5}'
