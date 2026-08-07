//! Application services shared by the GUI and the CLI.
//!
//! This is the only crate either frontend depends on. Networking, archive
//! parsing, loader installation, authentication, and filesystem business logic
//! all live below this line — never in the GUI .

pub mod logging;

pub use logging::init_tracing;
