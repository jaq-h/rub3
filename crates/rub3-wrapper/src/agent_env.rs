//! The environment variables that carry the agent's credential.
//!
//! Two modules care about these names from opposite directions: [`signer`]
//! reads them to find a key, [`supervisor`] removes them so the wrapped binary
//! cannot. They live here, compiled into every build, for two reasons:
//!
//!   * One list. A variable added for the reading side is stripped on the
//!     spawning side by construction, instead of by someone remembering.
//!   * `signer` exists only behind the `headless` feature, and the strip must
//!     not: what matters is that the child never sees the credential, however
//!     this wrapper was built.
//!
//! [`signer`]: crate::signer
//! [`supervisor`]: crate::supervisor

/// Raw hex private key. Highest precedence. Dev and CI only - an env var is
/// readable by anything sharing the process environment.
pub const ENV_AGENT_KEY: &str = "RUB3_AGENT_KEY";

/// Path to an encrypted Web3 Secret Storage (V3) keystore file.
pub const ENV_AGENT_KEYSTORE: &str = "RUB3_AGENT_KEYSTORE";

/// Keystore password, supplied inline.
pub const ENV_AGENT_KEYSTORE_PASSWORD: &str = "RUB3_AGENT_KEYSTORE_PASSWORD";

/// Path to a file whose contents are the keystore password. Preferred over
/// [`ENV_AGENT_KEYSTORE_PASSWORD`] - a file can be mode 0600, an env var cannot.
pub const ENV_AGENT_KEYSTORE_PASSWORD_FILE: &str = "RUB3_AGENT_KEYSTORE_PASSWORD_FILE";

/// Every variable above: the credential itself, and the two paths plus the
/// password that lead to it.
///
/// A keystore path and its password file are not the key, but together they
/// are enough to decrypt one, so they belong on the same list.
pub const AGENT_ENV_VARS: [&str; 4] = [
    ENV_AGENT_KEY,
    ENV_AGENT_KEYSTORE,
    ENV_AGENT_KEYSTORE_PASSWORD,
    ENV_AGENT_KEYSTORE_PASSWORD_FILE,
];
