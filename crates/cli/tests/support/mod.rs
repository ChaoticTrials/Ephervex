//! A launcher home that answers Mojang's metadata from checked-in fixtures.
//!
//! # Why this exists
//!
//! Every command that names a Minecraft version resolves it against Mojang's
//! version manifest, and each of these tests runs the binary against a
//! throwaway home whose cache starts empty. Without this the whole suite
//! fetched `piston-meta.mojang.com` on every run: slow, unavailable offline,
//! and a test that fails when a third party has a bad minute is a test that
//! reports nothing about this repository.
//!
//! The launcher already serves a cached document without touching the network
//! while it is within its freshness window, so nothing here is a special mode:
//! the sandbox is simply started with the same cache a launcher that had
//! already asked once would have. That is also what makes it honest coverage -
//! the code under test takes its ordinary cached path rather than a test-only
//! one.
//!
//! The fixtures are the ones documented in `fixtures/minecraft/SOURCES.md` and
//! `fixtures/loaders/SOURCES.md`. Each version document is written under the
//! digest its manifest entry declares, so the launcher's own hash verification
//! is what proves the pair still agree.

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, ContentStore, FileExtension, MetadataCache, MetadataKey};
use ephervex_loaders::{
    FORGE_MAVEN_METADATA_URL, FORGE_PROMOTIONS_URL, NEOFORGE_MAVEN_METADATA_URL,
};
use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest};

/// Seeds one sandbox's cache with the provider metadata a version or loader
/// selection is checked against.
///
/// `base` is the sandbox root, the directory the XDG variables point at.
pub(crate) fn seed_provider_metadata(base: &Path) {
    let layout = CacheLayout::new(base.join("cache").join("ephervex"));
    seed_minecraft(&layout);
    seed_loaders(&layout);
}

/// Mojang's manifest and every version document the fixtures cover.
fn seed_minecraft(layout: &CacheLayout) {
    let manifest_json = std::fs::read_to_string(fixture("version_manifest_v2.json"))
        .expect("the checked-in version manifest");
    let manifest = VersionManifest::parse(&manifest_json).expect("the fixture manifest parses");

    let payload = serde_json::value::RawValue::from_string(manifest_json)
        .expect("the fixture manifest is valid JSON");
    let key = MetadataKey::new(VERSION_MANIFEST_CACHE_KEY).expect("a valid cache key");
    // `store` stamps the entry with the current time, which is what makes the
    // launcher treat it as fresh and skip the fetch.
    store(layout, &key, VERSION_MANIFEST_URL, payload);

    let json = FileExtension::new("json").expect("a valid extension");
    for record in &manifest.versions {
        let Ok(document) = std::fs::read(fixture(&format!("{}.json", record.id))) else {
            // The manifest names every release Mojang has; only a handful are
            // captured, and a version no test asks for needs no document.
            continue;
        };
        let path = layout.content(ContentStore::Metadata, &record.sha1, Some(&json));
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("cache directory");
        std::fs::write(&path, &document).expect("seed a version document");
    }
}

/// The loader catalogs a `--loader` selection is resolved against.
///
/// Two of the three are XML rather than JSON, which the cache carries as a JSON
/// string; `CachedMetadata::text` is the other side of that, so writing them
/// this way is what the launcher itself does.
fn seed_loaders(layout: &CacheLayout) {
    for (key, url, name, is_text) in [
        (
            "forge-promotions",
            FORGE_PROMOTIONS_URL,
            "forge-promotions.json",
            false,
        ),
        (
            "forge-maven-metadata",
            FORGE_MAVEN_METADATA_URL,
            "forge-maven-metadata.xml",
            true,
        ),
        (
            "neoforge-maven-metadata",
            NEOFORGE_MAVEN_METADATA_URL,
            "neoforge-maven-metadata.xml",
            true,
        ),
    ] {
        let body = std::fs::read_to_string(loader_fixture(name)).expect("a loader fixture");
        let json = if is_text {
            serde_json::to_string(&body).expect("a JSON string")
        } else {
            body
        };
        let payload = serde_json::value::RawValue::from_string(json).expect("valid JSON");
        store(
            layout,
            &MetadataKey::new(key).expect("a valid cache key"),
            url,
            payload,
        );
    }
}

/// Writes one document into a sandbox's cache, as the launcher would.
///
/// `source` is the address the document really comes from, kept because the
/// cache records provenance and a seeded entry should not claim otherwise.
fn store(
    layout: &CacheLayout,
    key: &MetadataKey,
    source: &str,
    payload: Box<serde_json::value::RawValue>,
) {
    let url = source.parse().expect("a provider URL");
    block_on(MetadataCache::new(layout.clone()).store(key, &url, payload))
        .expect("seed a cache entry");
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/minecraft")
        .join(name)
}

fn loader_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/loaders")
        .join(name)
}

/// Runs one future to completion on a current-thread runtime.
///
/// The cache's write path is async because the launcher's is; a test harness
/// has no executor of its own and needs no more than this.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(future)
}
