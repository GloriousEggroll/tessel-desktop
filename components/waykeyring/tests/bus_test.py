"""waykeyring against real Secret Service clients on a private bus.

secret-tool (libsecret: DH-encrypted transport, default alias, prompts) and
python secretstorage; plus the portal backend and the control interface.
"""
import json, os, subprocess, sys, time
from pathlib import Path

BIN, ROOT = sys.argv[1], Path(sys.argv[2])
DIR, GNOME, KWALLET = ROOT / "keyring", ROOT / "gnome", ROOT / "kwalletd"
import shutil
shutil.copytree(Path(__file__).parent / "fixtures/kwalletd", KWALLET)
PROMPT = ROOT / "prompt.sh"
failures = []


def check(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def write_prompt(answers):
    """Fake prompt: prints answers[n] on call n (empty string = cancel)."""
    (ROOT / "answers.json").write_text(json.dumps(answers))
    (ROOT / "calls").write_text("0")
    PROMPT.write_text(f"""#!/bin/sh
n=$(cat {ROOT}/calls); echo $((n+1)) > {ROOT}/calls; echo "$@" >> {ROOT}/prompt-args
a=$(python3 -I -c 'import json,sys; a=json.load(open("{ROOT}/answers.json")); n=int(sys.argv[1]); print(a[n] if n < len(a) else "")' $n)
[ -n "$a" ] || exit 1
printf '%s\\n' "$a"
""")
    PROMPT.chmod(0o755)


def calls():
    return int((ROOT / "calls").read_text())


def start():
    p = subprocess.Popen([BIN, "--dir", str(DIR), "--gnome-dir", str(GNOME), "--kwallet-dir", str(KWALLET), "--prompt", str(PROMPT),
                          "--no-kernel", "--no-chkpwd"])
    for _ in range(50):
        r = subprocess.run(["busctl", "--user", "status", "org.freedesktop.secrets"], capture_output=True)
        if r.returncode == 0:
            break
        time.sleep(0.1)
    return p


def stop(p):
    p.terminate()
    p.wait(5)


def tool(*args, stdin=None):
    r = subprocess.run(["secret-tool", *args], input=stdin, capture_output=True, text=True, timeout=20)
    return r.returncode, r.stdout, r.stderr


# A plain-text GNOME keyring to import.
GNOME.mkdir(parents=True)
(GNOME / "Default_keyring.keyring").write_text(
    "[keyring]\ndisplay-name=Default keyring\nctime=0\nmtime=0\nlock-on-idle=false\nlock-after=false\n\n"
    "[1]\nitem-type=0\ndisplay-name=Old SSH\nsecret=old\\ssecret\nmtime=0\nctime=0\n\n"
    "[1:attribute0]\nname=server\ntype=string\nvalue=192.168.0.39\n")
(GNOME / ".temp-ABC").write_text("[keyring]\ndisplay-name=leftover\n")
(GNOME / "login.keyring").write_bytes(b"GnomeKeyring\n\r\x00\nencrypted")

# The first login creates the keyring.
r = subprocess.run([BIN, "--login", "--no-kernel", "--dir", str(DIR), "--kwallet-dir", str(KWALLET)], input=b"pw")
check("--login creates the keyring", r.returncode == 0 and (DIR / "master.json").exists())
check("--login decrypts the same-password KDE wallet for import", sorted(p.name for p in (DIR / "pending").iterdir())
      == ["kwallet_kdewallet.kwl.json"])
r = subprocess.run([BIN, "--login", "--no-kernel", "--dir", str(DIR)], input=b"wrong")
check("--login with a wrong password fails", r.returncode == 1)

write_prompt(["bad", "pw"])
daemon = start()
code, out, err = tool("store", "--label=GitHub", "service", "github", "user", "me", stdin="token-123")
check("store prompts (wrong, then right password) and saves", code == 0 and calls() == 2, err)
check("prompt was told to retry", "--retry" in (ROOT / "prompt-args").read_text())
code, out, _ = tool("lookup", "service", "github")
check("lookup returns the secret", code == 0 and out == "token-123", out)
code, out, _ = tool("lookup", "server", "192.168.0.39")
check("GNOME plain-text keyring imported on unlock", out == "old secret", out)
code, out, _ = tool("lookup", "service", "kde-github", "user", "kde-me")
check("KDE wallet imported on unlock with its Secret Service attributes", out == "tøken", out)
code, out, _ = tool("lookup", "app_id", "org.kde.Imported", "xdg:schema", "org.freedesktop.impl.portal.Secret")
check("KDE wallet portal key kept for its Flatpak app", code == 0, out)
check("pending import consumed", not list((DIR / "pending").iterdir()))

def control(method, *args):
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib
    conn = Gio.bus_get_sync(Gio.BusType.SESSION)
    try:
        sig = {"ImportKWallet": "(say)", "ImportGnomeKeyring": "(s)"}[method]
        r = conn.call_sync("org.freedesktop.secrets", "/org/waydock/Keyring", "org.waydock.Keyring", method,
                           GLib.Variant(sig, args), None, Gio.DBusCallFlags.NONE, 20000, None)
        return r.unpack()
    except GLib.Error as e:
        return Gio.DBusError.get_remote_error(e)

status = json.loads(subprocess.run([BIN, "--status"], capture_output=True, text=True).stdout)
check("status lists the KDE wallet that still needs its password", status["kwallets"] == [str(KWALLET / "old.kwl")],
      str(status))
check("wrong KDE wallet password", control("ImportKWallet", str(KWALLET / "old.kwl"), b"nope")
      == "org.waydock.Keyring.Error.WrongPassword")
r = control("ImportKWallet", str(KWALLET / "old.kwl"), b"kpw")
# old.kwl has no _attributes.json: its github entry gets kwallet-folder/key attributes (new);
# the map entry and the portal key match what kdewallet already imported.
check("KDE wallet with its own password: same entries are skipped", r == (1, 2), str(r))
status = json.loads(subprocess.run([BIN, "--status"], capture_output=True, text=True).stdout)
check("no KDE wallets left to import", status["kwallets"] == [], str(status))
other = ROOT / "elsewhere.keyring"
other.write_text("[keyring]\ndisplay-name=x\n\n[1]\nitem-type=0\ndisplay-name=Mail\nsecret=m\n\n"
                 "[1:attribute0]\nname=server\ntype=string\nvalue=mail.example\n")
check("GNOME keyring file imported on request", control("ImportGnomeKeyring", str(other)) == (1, 0))
check("encrypted GNOME keyring refused", control("ImportGnomeKeyring", str(GNOME / "login.keyring"))
      == "org.waydock.Keyring.Error.Failed")
code, out, err = tool("search", "--all", "service", "github")
check("search shows label and attributes", "label = GitHub" in out and "attribute.user = me" in err, out + err)
code, out, _ = tool("store", "--label=GitHub 2", "service", "github", "user", "me", stdin="token-456")
code, out, _ = tool("search", "--all", "service", "github")
check("store with same attributes replaces", out.count("\n[/") + out.startswith("[/") == 1
      and "token-456" in out, out)

on_disk = "".join(p.read_text() for p in (DIR / "collections").glob("*.json"))
check("nothing readable on disk", not any(s in on_disk for s in ("token", "github", "GitHub", "old secret", "192.168")))
modes = {oct(p.stat().st_mode & 0o777) for p in DIR.rglob("*") if p.is_file()}
check("files are 0600", modes == {"0o600"}, str(modes))

# python secretstorage (DH session, own collection, delete).
import secretstorage
bus = secretstorage.dbus_init()
gh = list(secretstorage.search_items(bus, {"service": "github"}))
check("attributes readable over D-Bus", len(gh) == 1 and gh[0].get_attributes() == {"service": "github", "user": "me"},
      str([g.get_attributes() for g in gh]))
default = secretstorage.get_default_collection(bus)
check("secretstorage sees the default collection unlocked", not default.is_locked())
item = default.create_item("Browser field", {"origin": "https://example.org", "field": "email"}, b"me@example.org")
found = list(secretstorage.search_items(bus, {"origin": "https://example.org"}))
check("secretstorage create + search + get_secret", len(found) == 1 and found[0].get_secret() == b"me@example.org")
found[0].set_label("Renamed")
check("label can be changed", found[0].get_label() == "Renamed")
found[0].delete()
check("secretstorage delete", not list(secretstorage.search_items(bus, {"origin": "https://example.org"})))
extra = secretstorage.create_collection(bus, "Work")
extra.create_item("w", {"k": "v"}, b"work-secret")
check("new collections work", any(c.get_label() == "Work" for c in secretstorage.get_all_collections(bus)))

# Control interface.
status = json.loads(subprocess.run([BIN, "--status"], capture_output=True, text=True).stdout)
check("status", status["set_up"] and status["unlocked"] and status["imported"] == 6
      and len(status["legacy_files"]) == 2, str(status))

# Portal backend: the same key for the same app every time.
def portal(app):
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib
    conn = Gio.bus_get_sync(Gio.BusType.SESSION)
    r, w = os.pipe()
    fds = Gio.UnixFDList.new_from_array([w])
    conn.call_with_unix_fd_list_sync("org.freedesktop.secrets", "/org/freedesktop/portal/desktop",
        "org.freedesktop.impl.portal.Secret", "RetrieveSecret",
        GLib.Variant("(osha{sv})", ("/org/freedesktop/portal/desktop/request/1_1/t", app, 0, {})),
        GLib.VariantType("(ua{sv})"), Gio.DBusCallFlags.NONE, 5000, fds, None)
    data = os.read(r, 4096)
    os.close(r)
    return data
a1, a2, b1 = portal("org.example.App"), portal("org.example.App"), portal("org.other.App")
check("portal secret is stable per app and differs between apps", len(a1) == 64 and a1 == a2 and a1 != b1)

# Restart: data persists, starts locked, lock + unlock prompt again.
stop(daemon)
write_prompt(["pw"])
daemon = start()
check("restarted daemon is locked", secretstorage.get_default_collection(bus).is_locked())
code, out, _ = tool("lookup", "service", "github")
check("lookup after restart prompts once and works", out == "token-456" and calls() == 1, out)
code, out, _ = tool("lookup", "k", "v")
check("other collections unlock with it", out == "work-secret", out)
write_prompt([""])
subprocess.run(["busctl", "--user", "call", "org.freedesktop.secrets", "/org/waydock/Keyring", "org.waydock.Keyring", "Lock"])
code, out, _ = tool("lookup", "service", "github")
check("cancelled prompt gives nothing", out == "" and calls() == 1, out)

# Legacy files: removed only after import, and only the plain-text ones.
write_prompt(["pw"])
tool("lookup", "service", "github")
subprocess.run(["busctl", "--user", "call", "org.freedesktop.secrets", "/org/waydock/Keyring",
                "org.waydock.Keyring", "RemoveLegacyFiles"], capture_output=True)
check("plain-text GNOME files removed, encrypted one kept",
      sorted(p.name for p in GNOME.iterdir()) == ["login.keyring"], str(sorted(p.name for p in GNOME.iterdir())))

# Password change keeps everything.
stop(daemon)
r = subprocess.run([BIN, "--change-password", "--dir", str(DIR)], input=b"pw\0newpw")
check("--change-password", r.returncode == 0)
write_prompt(["pw", "newpw"])
daemon = start()
code, out, _ = tool("lookup", "service", "github")
check("old password refused, new one works", out == "token-456" and calls() == 2, out)
stop(daemon)

print(f"{len(failures)} failed" if failures else "all keyring bus checks passed")
sys.exit(1 if failures else 0)
