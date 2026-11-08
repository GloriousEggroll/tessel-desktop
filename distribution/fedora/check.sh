#!/usr/bin/python3 -I
"""Check that the built RPMs carry everything tools/install.py installs: every
program of the workspace (the Kasumi screen has its own installer), every unit,
desktop file and script, each exactly once. Run by build.sh after a full build."""
import collections
import json
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
RPMS = sorted((REPO / "build-fedora/RPMS").glob("*.rpm"))
owners = collections.defaultdict(list)
for rpm in RPMS:
    listed = subprocess.run(["rpm", "-qlp", str(rpm)], capture_output=True, text=True, check=True).stdout
    for path in listed.split("\n"):
        if path.startswith("/") and not path.startswith("/usr/lib/.build-id"):
            owners[path].append(rpm.name)

metadata = json.loads(subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=REPO,
                                     capture_output=True, text=True, check=True).stdout)
wanted = [f"/usr/lib/tessel/{target['name']}" for package in metadata["packages"] if package["name"] != "home-assistant-screen"
          for target in package["targets"] if "bin" in target["kind"]]
wanted += [f"/usr/lib/tessel/{name}" for name in ("tessel-keyboard-input", "libcapture-pipewire.so")]
wanted += [f"/usr/lib/tessel/{path.name}" for directory in ("data/bin", "data/login", "data/session") for path in (REPO / directory).iterdir()]
wanted += [f"/usr/lib/systemd/user/{path.name}" for path in (REPO / "data/units").iterdir()]
wanted += [f"/usr/share/applications/{path.name}" for path in
           list((REPO / "data/applications").glob("*.desktop")) + list(REPO.glob("components/*/data/*.desktop"))]
wanted += ["/usr/lib/systemd/user/tessel-keyring.service", "/usr/share/wayland-sessions/tessel.desktop"]

missing = sorted(set(wanted) - set(owners))
shared = sorted(path for path, packages in owners.items() if len(packages) > 1 and not path.endswith("/"))
expected = {path.parent.name for path in (REPO / "distribution/fedora").glob("*/*.spec")}
print(f"{len(RPMS)} RPMs from {len(expected)} specs, {len(owners)} files.")
for path in missing:
    print("MISSING", path)
for path in shared:
    print("IN SEVERAL PACKAGES", path, owners[path])
sys.exit(1 if missing or shared or len(RPMS) != len(expected) else 0)
