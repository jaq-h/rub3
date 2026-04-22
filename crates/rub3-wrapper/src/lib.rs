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

mod activation;
pub mod rpc;
mod supervisor;
mod webview;

pub use activation::{ensure, ActivationError};
pub use supervisor::run as supervisor_run;
