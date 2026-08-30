//! Forge and NeoForge discovery and installation behind the loader-provider
//! boundary from ADR 0005.

mod discovery;
mod error;
mod install;
mod profile;
mod provider;

pub use discovery::{
    FORGE_MAVEN_METADATA_URL, FORGE_PROMOTIONS_URL, LoaderBuild, LoaderCatalog,
    LoaderVersionListing, NEOFORGE_MAVEN_METADATA_URL, NEOFORGE_MINIMUM_MINECRAFT,
    minimum_minecraft, supports_minecraft,
};
pub use error::{LoaderError, LoaderResult};
pub use install::{InstalledClient, InstalledServer, ServerLaunch};
pub use profile::{InstallProfile, InstallSide, Processor};
pub use provider::{
    ClientInstallRequest, ForgeProvider, LoaderProvider, LoaderRegistry, NeoForgeProvider,
    ServerInstallRequest, VanillaProvider,
};
