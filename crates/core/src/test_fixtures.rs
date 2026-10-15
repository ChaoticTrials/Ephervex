//! Fixtures the crate's own unit tests share.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, ContentStore, FileExtension, MetadataCache, MetadataKey};
use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest};

/// Mojang's manifest and the release documents the fixtures cover, stored as
/// fresh so resolving a release reaches no network.
pub(crate) async fn seed_minecraft(cache: &Path) {
    let layout = CacheLayout::new(cache);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minecraft");
    let manifest_json = std::fs::read_to_string(fixtures.join("version_manifest_v2.json")).unwrap();
    let manifest = VersionManifest::parse(&manifest_json).unwrap();
    MetadataCache::new(layout.clone())
        .store(
            &MetadataKey::new(VERSION_MANIFEST_CACHE_KEY).unwrap(),
            &VERSION_MANIFEST_URL.parse().unwrap(),
            serde_json::value::RawValue::from_string(manifest_json).unwrap(),
        )
        .await
        .unwrap();
    let json = FileExtension::new("json").unwrap();
    for record in &manifest.versions {
        let Ok(document) = std::fs::read(fixtures.join(format!("{}.json", record.id))) else {
            continue;
        };
        let path = layout.content(ContentStore::Metadata, &record.sha1, Some(&json));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &document).unwrap();
    }
}

/// The `os.arch` a JVM built for this machine reports.
pub(crate) const fn jvm_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "amd64"
    }
}

/// An executable under `home` that answers the property probe as a JVM of
/// `version` would, returned by its canonical path.
pub(crate) fn fake_java(home: &Path, version: &str) -> PathBuf {
    let executable = home.join("bin/java");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = {home}\n    \
             java.runtime.version = {version}+7\n    java.vendor = Eclipse Adoptium\n    \
             java.version = {version}\n    os.arch = {arch}\nEOF\n",
            home = home.display(),
            arch = jvm_arch(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::canonicalize(executable).unwrap()
}
