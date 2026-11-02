"""Run inside the private headless labwc only (tests/measure_headless.sh).

End-to-end checks of the real binaries (exit codes, SIGTERM-as-close, stdin)
and time-to-window-mapped / RSS of the Rust prompt vs wayshell_prompt.py.
"""
import json, os, signal, subprocess, sys, threading, time

assert os.environ.get("WAYSHELL_UI_ISOLATED") == "1" and os.environ["XDG_RUNTIME_DIR"] != "/run/user/1000"
rust, helper = sys.argv[1], sys.argv[2]
python = ["/usr/bin/python3", "-B", os.path.expanduser("~/tessel-bluetooth/tessel-shell/wayshell_prompt.py")]
cache = os.environ["XDG_CACHE_HOME"] + "/tessel-dock"
os.makedirs(cache, exist_ok=True)
with open(cache + "/palette.json", "w") as f:
    json.dump({"version": 1, "is_dark": True, "monitors": [{"name": "HEADLESS-1", "geometry": [0, 0, 1920, 1080],
               "background": [25, 20, 30], "foreground": [225, 230, 235], "accent": [190, 50, 90]}]}, f)

events, lock = [], threading.Condition()
watcher = subprocess.Popen([helper], stdout=subprocess.PIPE, stdin=subprocess.PIPE, text=True)
def pump():
    for line in watcher.stdout:
        with lock:
            events.append((time.monotonic(), line))
            lock.notify_all()
threading.Thread(target=pump, daemon=True).start()
time.sleep(0.5)

def mapped_after(start, timeout=10):
    deadline = time.monotonic() + timeout
    with lock:
        while time.monotonic() < deadline:
            for stamp, line in events:
                if stamp >= start and '"org.tessel.Prompt"' in line:
                    return stamp
            lock.wait(0.05)
    return None

def memory(pid):
    values = {}
    for name in ("status", "smaps_rollup"):
        with open(f"/proc/{pid}/{name}") as f:
            for row in f:
                key, _, rest = row.partition(":")
                if key in ("VmRSS", "Pss", "Private_Dirty"):
                    values[key] = int(rest.split()[0])
    return values

def run(command, args, stdin=None, signal_after=True):
    start = time.monotonic()
    process = subprocess.Popen(command + args, stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if stdin is not None:
        process.stdin.write(stdin)
        process.stdin.close()
    stamp = mapped_after(start)
    time.sleep(1.0)
    mem = memory(process.pid)
    if signal_after:
        process.send_signal(signal.SIGTERM)
    code = process.wait(10)
    return (stamp - start) if stamp else None, mem, code

report = {}
failures = []
for label, command in (("rust", [rust]), ("python", python)):
    rows = []
    for _ in range(5):
        ready, mem, code = run(command, ["--message", "Measure this prompt?"])
        rows.append({"ready_ms": round(ready * 1000) if ready else None, **mem, "code": code})
        if code != 254 or ready is None:
            failures.append(f"{label} --message SIGTERM -> {code} ready={ready}")
    _, _, code = run(command, ["--error"], stdin=b"line 3: bad element\n")
    if code != 0:
        failures.append(f"{label} --error SIGTERM -> {code}")
    _, _, code = run(command, ["--message-stdin"], stdin=b"Apostrophe ' $(x)\n")
    if code != 254:
        failures.append(f"{label} --message-stdin SIGTERM -> {code}")
    usage = subprocess.run(command, capture_output=True)
    if usage.returncode != 2:
        failures.append(f"{label} no args -> {usage.returncode}")
    report[label] = rows
for label, rows in report.items():
    ready = sorted(r["ready_ms"] for r in rows if r["ready_ms"] is not None)
    rss = sorted(r["VmRSS"] for r in rows)
    pss = sorted(r["Pss"] for r in rows)
    print(f"{label}: time-to-mapped median {ready[len(ready)//2]} ms (min {ready[0]}, max {ready[-1]}); "
          f"RSS median {rss[len(rss)//2]/1024:.1f} MiB; PSS median {pss[len(pss)//2]/1024:.1f} MiB")
print(json.dumps(report))
watcher.terminate()
print("FAILURES:", failures if failures else "none")
sys.exit(1 if failures else 0)
