//! Normalized global Minecraft preferences and version-aware `options.txt`
//! adapters.
//!
//! # The feature in one sentence
//!
//! Configure a handful of Minecraft settings once, and have every *newly
//! created or imported* client instance come up with them applied — without
//! ever copying one release's `options.txt` into another release's instance.
//!
//! # Three rules, and where each is enforced
//!
//! * **Never copy a foreign options file**. Preferences are held as
//!   typed, normalized values in [`GlobalMinecraftPreferences`]; nothing in
//!   this crate can express "the file from my other instance".
//! * **Never rewrite what you were not asked to**. [`ParsedOptions`]
//!   is line-preserving, so unknown keys, mod settings, comments, and the
//!   version marker survive an edit byte for byte, and only *managed*
//!   preferences override what an imported pack shipped.
//! * **Never assume a key means the same thing in every release**.
//!   [`OptionsAdapter`] implementations declare tested compatibility ranges;
//!   a release outside all of them gets only the keys that are safe everywhere
//!   plus a developer-log warning.
//!
//! # What this crate deliberately does not do
//!
//! It does not know where instances live, what a `ClientRecord` is, or when
//! creation happens — [`apply_to_game_directory`] takes a directory and a
//! version string. Sequencing it into instance creation is `ephervex-core`'s
//! job, and reading the settings out of `config.toml` is `ephervex-storage`'s
//! shape to describe; this crate takes them as plain `(name, enabled, value)`
//! triples, the same split that keeps `ephervex-java` free of any opinion
//! about TOML.
//!
//! # Why it depends on `ephervex-minecraft`
//!
//! Choosing an adapter is a Minecraft version comparison, and
//! `MinecraftVersionId` already implements both published numbering schemes
//! and their ordering. A second parser here would be a second thing to keep in
//! step with Mojang, and the one that gets forgotten is the one that mis-sorts
//! a release.

pub mod adapter;
pub mod apply;
pub mod error;
pub mod file;
pub mod preferences;

pub use adapter::{
    ADAPTERS, AdapterSet, CONSERVATIVE, EraAdapter, LEGACY_1_12, MODERN_1_13, MODERN_1_18,
    OptionsAdapter, VERSION_KEY, decode_fov, encode_fov,
};
pub use apply::{OPTIONS_FILE_NAME, PreferenceApplication, apply_to_game_directory};
pub use error::OptionsError;
pub use file::ParsedOptions;
pub use preferences::{
    GlobalMinecraftPreferences, GuiScalePreference, InvalidPreferenceValue, LoadedPreferences,
    MAXIMUM_FOV_DEGREES, MINIMUM_FOV_DEGREES, Managed, PreferenceKey, PreferenceValue, WindowSize,
};
