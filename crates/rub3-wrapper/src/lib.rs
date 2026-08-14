pub mod agent_env;
pub mod license;
pub mod store;

#[cfg(feature = "session")]
pub mod identity;
#[cfg(feature = "session")]
pub mod session;
#[cfg(feature = "session")]
pub mod session_store;
#[cfg(feature = "device-key")]
pub mod device;
#[cfg(feature = "binary-encryption")]
pub mod decrypt;

// Headless (agent) front door. `signer` is the only module in the crate that
// touches raw key material; `tx` turns calldata into a broadcast transaction.
#[cfg(feature = "headless")]
pub mod signer;
#[cfg(feature = "headless")]
pub mod tx;

pub mod activation;
pub mod rpc;
mod supervisor;

// Interactive (human) front door. Optional: a `headless` build links neither
// `wry` nor `tao`.
#[cfg(feature = "webview")]
mod webview;

/// One process-wide guard for every unit test that touches process
/// environment variables.
///
/// `std::env::set_var` mutates state shared by the whole test binary, and
/// libtest runs tests on parallel threads, so a `setenv` racing a `getenv`
/// elsewhere is a genuine data race. Modules must not keep their own locks:
/// two lock domains do not exclude each other, which is the race in a costume.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use activation::{ensure, ActivationError};
pub use supervisor::run as supervisor_run;

#[cfg(feature = "headless")]
pub use activation::{ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome};
