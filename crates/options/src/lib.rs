//! Normalized global Minecraft preferences and generated `options.txt`
//! compatibility.
//!
//! # The feature in one sentence
//!
//! Configure a handful of Minecraft settings once, and have every *newly
//! created or imported* client instance come up with them applied — without
//! ever copying one release's `options.txt` into another release's instance.
//!
//! # Four rules, and where each is enforced
//!
//! * **Never copy a foreign options file**. Preferences are held as typed,
//!   normalized values in [`GlobalMinecraftPreferences`]; nothing in this crate
//!   can express "the file from my other instance".
//! * **Never rewrite what you were not asked to**. [`ParsedOptions`] is
//!   line-preserving, so unknown keys, mod settings, comments, and the version
//!   marker survive an edit byte for byte, and only *managed* preferences
//!   override what an imported pack shipped.
//! * **Never assume a key means the same thing in every release**. What each
//!   release stores comes from a generated compatibility dataset built by
//!   static analysis of official client JARs
//!   (`tools/minecraft-options/update.py`). A release nobody has analyzed is
//!   *unknown*, and unknown is never written.
//! * **Never let remote data compute anything**. The dataset can travel over
//!   the network so a new Minecraft release does not need a new Ephervex
//!   release, so its codecs are a closed set of typed variants
//!   ([`OptionCodec`]) rather than expressions. A variant this build does not
//!   implement fails the whole document.
//!
//! # Where the knowledge comes from at run time
//!
//! ```text
//! valid cached remote feed, newer than the bundled baseline
//!     -> bundled baseline compiled into this build
//! ```
//!
//! A fresh offline installation uses the baseline, which covers every release
//! verified when the build shipped. [`OptionsFeed`] refreshes the remote copy
//! in the background and only replaces the cache with a document that fully
//! validated, so a bad publish cannot brick preference application.
//!
//! # What this crate deliberately does not do
//!
//! It does not know where instances live, what a `ClientRecord` is, or when
//! creation happens — [`apply_to_game_directory`] takes a directory and a
//! version string. Sequencing it into instance creation is `ephervex-core`'s
//! job, reading the settings out of `config.toml` is `ephervex-storage`'s shape
//! to describe, and resolving a release's language catalog belongs to
//! `ephervex-minecraft`, which owns Mojang metadata; this crate takes the
//! verdict as [`LanguageAvailability`].

pub mod apply;
pub mod codec;
pub mod compatibility;
pub mod dataset;
pub mod error;
pub mod feed;
pub mod file;
pub mod preferences;

pub use apply::{
    LanguageAvailability, OPTIONS_FILE_NAME, PreferenceApplication, SkipReason, SkippedPreference,
    apply_to_game_directory,
};
pub use codec::{OptionCodec, SettingValue, Unrepresentable};
pub use compatibility::{
    CompatibilitySource, OptionsCompatibility, PreferenceComponent, PreferenceSupport,
    ResolvedField,
};
pub use dataset::{
    DATASET_FORMAT_VERSION, DatasetError, OptionsDataset, RegistrySetting, ReleaseMapping,
    Representation, SchemaFamily,
};
pub use error::OptionsError;
pub use feed::{
    DEFAULT_GIST_ID, FEED_FILE_NAME, FEED_HOSTS, FeedError, FeedSource, GIST_API_BASE,
    OPTIONS_FEED_CACHE_KEY, OptionsFeed,
};
pub use file::{ParsedOptions, VERSION_KEY};
pub use preferences::{
    GlobalMinecraftPreferences, GuiScalePreference, InvalidPreferenceValue, LoadedPreferences,
    MAXIMUM_FOV_DEGREES, MINIMUM_FOV_DEGREES, Managed, PreferenceKey, PreferenceValue, WindowSize,
};
