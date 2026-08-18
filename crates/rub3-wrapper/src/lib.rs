pub mod agent_env;
pub mod license;
pub mod store;

// Pre-purchase contract attestation. Needs to read the chain, so it exists from
// tier-2 up and compiles away entirely below that: a tier-0 or tier-1 build
// never touches the chain, so it has nothing to attest.
#[cfg(feature = "onchain-read")]
pub mod attest;

#[cfg(feature = "binary-encryption")]
pub mod decrypt;
#[cfg(feature = "device-key")]
pub mod device;
#[cfg(feature = "session")]
pub mod identity;
#[cfg(feature = "session")]
pub mod session;
#[cfg(feature = "session")]
pub mod session_store;

// Headless (agent) front door. `signer` is the only module in the crate that
// touches raw key material; `tx` turns calldata into a broadcast transaction.
#[cfg(feature = "headless")]
pub mod signer;
#[cfg(feature = "headless")]
pub mod tx;

pub mod activation;
pub mod rpc;
mod supervisor;

// The wrapper's half of the SDK channel (implementation.md §3.5): the local
// socket a wrapped application asks "are you there" and "who is running me"
// over. Orthogonal to the tier bundles - what an application may ask does not
// depend on how hard the launch was gated - so no bundle enables it.
#[cfg(feature = "sdk")]
pub mod sdk;

// Interactive (human) front door. Optional: a `headless` build links neither
// `wry` nor `tao`.
#[cfg(feature = "webview")]
mod webview;

// Test-only helpers shared across modules. Declared here rather than nested in
// one module's `mod tests` so a second module can use them without a copy.
#[cfg(test)]
pub(crate) mod test_support;

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
pub use supervisor::{run as supervisor_run, Launch};

#[cfg(feature = "headless")]
pub use activation::{ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome};
