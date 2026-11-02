//! tessel-keyring: Tessel's Secret Service.
//!
//! Applications (browsers, git-credential-libsecret, Slack, password managers,
//! SSH askpass helpers, Flatpaks through the secret portal) store passwords
//! through `org.freedesktop.secrets`. tessel-keyring keeps them encrypted
//! ([`crypto`], [`store`]) and unlocks them with the login password: the PAM
//! module runs `tessel-keyring --login` at sign-in, which unwraps the master key
//! and hands it to the daemon through the kernel keyring ([`kernel`]).
//! Without it (fingerprint login, password changed elsewhere) the first
//! application that needs a secret gets one password prompt.

pub mod crypto;
pub mod dh;
pub mod gnome_import;
pub mod imports;
pub mod kernel;
pub mod kwallet_import;
pub mod login;
pub mod service;
pub mod store;

pub const BUS_NAME: &str = "org.freedesktop.secrets";
/// Status and maintenance interface for Global Settings.
pub const CONTROL_INTERFACE: &str = "org.tessel.Keyring";
pub const CONTROL_PATH: &str = "/org/tessel/Keyring";
