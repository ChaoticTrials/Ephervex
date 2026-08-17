//! The credential wrapper, which lives in `ephervex-model`.
//!
//! Re-exported here because this crate is where most of the workspace meets a
//! secret, and because moving the type would otherwise break every caller that
//! reaches it through the authentication surface. There is one implementation:
//! see [`ephervex_model::secret`] for what it refuses to do and why.

pub use ephervex_model::secret::{REDACTED, SecretString};
