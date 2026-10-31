# waykeyring vs. gnome-keyring / KWallet

The suite's own Secret Service (`org.freedesktop.secrets`), no Python twin.

| | waykeyring |
|---|---|
| Secret Service API 0.2 | Service, Collection, Item, Session, Prompt; `plain` and `dh-ietf1024-sha256-aes128-cbc-pkcs7` transports (libsecret, secretstorage, Chromium/Electron, git-credential-libsecret, KeePassXC browser integration, NetworkManager agents, ksshaskpass). Aliases (`default` -> `login`, `session` in memory), collections can be created and deleted, locked searches answered from a salted attribute index. |
| Flatpak apps | `org.freedesktop.impl.portal.Secret` at `/org/freedesktop/portal/desktop` (per-app 64-byte key, same item attributes as gnome-keyring). |
| Unlock with the login password | `pam_waykeyring.so` (auth: keeps the password; session: `waykeyring --login` as the user; password: re-wrap on change). The master key reaches the daemon through the kernel user keyring (`waykeyring:master`). No separate master password. |
| Fingerprint login / password changed elsewhere | One GTK prompt (`waykeyring-prompt`) the first time an application needs a secret. A first-time setup prompt checks the password with `unix_chkpwd`, so the keyring password stays the login password. |
| Storage | `~/.local/share/desktop-suite/keyring/` (0700, files 0600): master key wrapped with Argon2id(64 MiB, t=3) + XChaCha20-Poly1305; each collection sealed with the master key (labels, attributes and secrets encrypted; only item ids and salted SHA-256 attribute hashes in clear). |
| Migration | Unencrypted GNOME keyrings (`~/.local/share/keyrings/*.keyring` starting with `[keyring]`) are imported once into `login` at the first unlock; Global Settings > Passwords offers to delete the plain-text files afterwards. Encrypted GNOME keyrings and KWallet files are not imported. |
| UI | Global Settings > Passwords: status, Lock/Unlock, search, copy (marked secret for clipboard managers), delete. No item editing, no per-application access control, no auto-lock timer, no SSH agent / PKCS#11 (gnome-keyring's other components). |

Crates: `waykeyring` (daemon + PAM helper), `pam/` (`pam_waykeyring.so`), `ui/` (`waykeyring-prompt`, the settings page).

Tests: `cargo test -p waykeyring -p pam-waykeyring -p waykeyring-ui`; `tests/run_bus_tests.sh` (private bus: secret-tool,
python secretstorage, portal, import, prompts, restart, lock, password change; 25 checks); Global Settings
`applets_headless` `passwords_page_lists_copies_and_deletes_waykeyring_entries`; `tools/test_install.py`
`test_keyring_adds_pam_lines_and_rolls_back_exactly`.
