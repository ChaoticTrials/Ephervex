//! Provenance of files the user supplied by hand.
//!
//! A provider can refuse to serve a file to the launcher. When the user
//! downloads it themselves and places it where Ephervex can find it, the copy
//! is adopted into the shared download store at its content address, and
//! nothing about the store alone says that this entry, unlike every other one,
//! cannot be fetched again.
//!
//! That fact is recorded as a marker at [`CacheLayout::supplied_marker`]. Byte
//! budget collection and reference-based cleanup both read it and leave a marked
//! file alone: removing it would cost the user a manual download, not a
//! transfer. Only a cleanup the user confirmed for these files removes one, and
//! it removes the marker with the file, so a later automatic download of the
//! same digest is not protected by a stale marker. The marker is written before
//! the file is published, so a walk that sees the file always sees its marker.

use std::path::Path;

use ephervex_model::Hash;

use crate::cache::CacheLayout;

/// Records that the file addressed by `hash` was supplied by hand.
///
/// Idempotent. A marker with no file behind it is harmless: nothing reads a
/// marker except to spare a file of the same digest.
pub async fn record_supplied_download(layout: &CacheLayout, hash: &Hash) -> std::io::Result<()> {
    let marker = layout.supplied_marker(hash);
    if let Some(parent) = marker.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&marker)
        .await?;
    file.sync_all().await
}

/// Forgets that the file addressed by `hash` was supplied by hand.
///
/// A marker that is already gone counts as forgotten. Only a regular file is
/// removed: anything else at the marker's name never spared a file, and is left
/// as it is.
pub(crate) fn forget_supplied_download(layout: &CacheLayout, hash: &Hash) -> std::io::Result<()> {
    let marker = layout.supplied_marker(hash);
    match std::fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.is_file() => match std::fs::remove_file(&marker) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether the file addressed by `hash` was supplied by hand.
///
/// Judged on the marker itself, without following a link, so a marker that has
/// been replaced by anything other than a regular file spares nothing.
#[must_use]
pub fn is_supplied_download(layout: &CacheLayout, hash: &Hash) -> bool {
    is_regular_file(&layout.supplied_marker(hash))
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::HashAlgorithm;

    use super::*;

    fn hash() -> Hash {
        Hash::new(
            HashAlgorithm::Sha1,
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        )
        .expect("valid sha1")
    }

    #[tokio::test]
    async fn a_recorded_digest_is_supplied_and_recording_twice_is_harmless() {
        let root = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(root.path());
        assert!(!is_supplied_download(&layout, &hash()));

        record_supplied_download(&layout, &hash())
            .await
            .expect("record");
        record_supplied_download(&layout, &hash())
            .await
            .expect("record again");

        assert!(is_supplied_download(&layout, &hash()));
    }

    #[tokio::test]
    async fn forgetting_a_supplied_digest_removes_its_marker_and_is_harmless_twice() {
        let root = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(root.path());
        record_supplied_download(&layout, &hash())
            .await
            .expect("record");

        forget_supplied_download(&layout, &hash()).expect("forget");
        forget_supplied_download(&layout, &hash()).expect("forget again");

        assert!(!is_supplied_download(&layout, &hash()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_marker_replaced_by_a_link_spares_nothing() {
        let root = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(root.path());
        let target = root.path().join("elsewhere");
        std::fs::write(&target, b"").expect("write");
        let marker = layout.supplied_marker(&hash());
        std::fs::create_dir_all(marker.parent().expect("parent")).expect("directories");
        std::os::unix::fs::symlink(&target, &marker).expect("link");

        assert!(!is_supplied_download(&layout, &hash()));
    }
}
