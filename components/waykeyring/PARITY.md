# waykeyring vs. gnome-keyring / KWallet

The suite's own Secret Service (`org.freedesktop.secrets`), no Python twin.

| | waykeyring |
|---|---|
| Secret Service API 0.2 | Service, Collection, Item, Session, Prompt; `plain` and `dh-ietf1024-sha256-aes128-cbc-pkcs7` transports (libsecret, secretstorage, Chromium/Electron, git-credential-libsecret, KeePassXC browser integration, NetworkManager agents, ksshaskpass). Aliases (`default` -> `login`, `session` in memory), collections can be created and deleted, locked searches answered from a salted attribute index. |
| Flatpak apps | `org.freedesktop.impl.portal.Secret` at `/org/freedesktop/portal/desktop` (per-app 64-byte key, same item attributes as gnome-keyring). |
| Unlock with the login password | `pam_waykeyring.so` (auth: keeps the password; session: `waykeyring --login` as the user; password: re-wrap on change). The master key reaches the daemon through the kernel user keyring (`waykeyring:master`). No separate master password. |
| Fingerprint login / password changed elsewhere | One GTK prompt (`waykeyring-prompt`) the first time an application needs a secret. A first-time setup prompt checks the password with `unix_chkpwd`, so the keyring password stays the login password. |
| Storage | `~/.local/share/desktop-suite/keyring/` (0700, files 0600): master key wrapped with Argon2id(64 MiB, t=3) + XChaCha20-Poly1305; each collection sealed with the master key (labels, attributes and secrets encrypted; only item ids and salted SHA-256 attribute hashes in clear). |
| Migration | Unencrypted GNOME keyrings (`~/.local/share/keyrings/*.keyring` starting with `[keyring]`) are imported once into `login` at the first unlock; Global Settings > Passwords offers to delete the plain-text files afterwards. KDE wallets (`~/.local/share/kwalletd/*.kwl`, Blowfish CBC/ECB with PBKDF2-SHA512 keys, i.e. every wallet since KDE 4.13) are decrypted at login with the login password when it matches (sealed with the master key under `pending/` until the daemon merges them); others are imported from Global Settings > Passwords > Import with their own password. Entries keep the attributes from `<wallet>_attributes.json` (KWallet's Secret Service layer), portal keys become the portal's per-app items, everything else gets `kwallet-folder`/`kwallet-key`; entries whose attributes already exist are skipped. Not imported: GPG-encrypted wallets, pre-4.13 SHA-1 wallets, encrypted GNOME keyrings. |
| UI | Global Settings > Passwords (also "Passwords" in the app menu): status, Lock/Unlock, Import (KDE wallets, a chosen .kwl/.keyring file), search, copy (marked secret for clipboard managers), delete. No item editing, no per-application access control, no auto-lock timer, no SSH agent / PKCS#11 (gnome-keyring's other components). |

Crates: `waykeyring` (daemon + PAM helper), `pam/` (`pam_waykeyring.so`), `ui/` (`waykeyring-prompt`, the settings page).

Tests: `cargo test -p waykeyring -p pam-waykeyring -p waykeyring-ui`; `tests/run_bus_tests.sh` (private bus: secret-tool,
python secretstorage, portal, GNOME and KDE wallet imports, prompts, restart, lock, password change; 35 checks); Global Settings
`applets_headless` `passwords_page_lists_copies_and_deletes_waykeyring_entries`; `tools/test_install.py`
`test_keyring_adds_pam_lines_and_rolls_back_exactly`.
