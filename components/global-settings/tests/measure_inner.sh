#!/bin/sh
# Runs INSIDE tests/headless.sh (private compositor, fake HOME, private bus):
#   measure_inner.sh <app_id> <rust argv…> -- <python argv…>
# Prints time-to-mapped-window and idle RSS/PSS (3 s after mapping) for each,
# twice, alternating. Uses the real wlrctl/grim against the PRIVATE socket only.
set -u
case "$XDG_RUNTIME_DIR" in /run/user/1000/cbt/*) ;; *) echo "not private"; exit 1;; esac
APP=$1; shift
RUST=""; PY=""; side=rust
for a in "$@"; do
  if [ "$a" = "--" ]; then side=py; continue; fi
  if [ $side = rust ]; then RUST="$RUST '$a'"; else PY="$PY '$a'"; fi
done
ms() { date +%s%3N; }
mem() { echo "rss_kb=$(awk '/VmRSS/ {print $2}' /proc/$1/status) pss_kb=$(awk '/^Pss:/ {print $2}' /proc/$1/smaps_rollup)"; }
measure() {
  name=$1; cmd=$2
  t0=$(ms); ( eval "exec $cmd" ) > "$GS_ROOT/tmp/$name.out" 2>&1 & pid=$!
  until /usr/bin/wlrctl toplevel find "app_id:$APP" >/dev/null 2>&1; do sleep 0.01; [ $(($(ms)-t0)) -gt 20000 ] && break; done
  echo "$name window_ms=$(($(ms)-t0))"
  sleep 3; echo "$name idle $(mem $pid)"
  /usr/bin/grim "$GS_ROOT/tmp/$name.png" 2>/dev/null || true
  kill -TERM $pid 2>/dev/null; wait $pid 2>/dev/null
  n=0; while /usr/bin/wlrctl toplevel find "app_id:$APP" >/dev/null 2>&1 && [ $n -lt 300 ]; do sleep 0.02; n=$((n+1)); done
  sleep 0.5
}
for i in 1 2; do
  measure rust "$RUST"
  measure python "$PY"
done
