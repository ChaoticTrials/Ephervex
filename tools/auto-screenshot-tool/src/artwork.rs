//! Project logos for the recorded mod rows.
//!
//! The fixtures name each logo by address and by the SHA-256 of the bytes the
//! catalog served. A full run fetches the missing ones into a local cache,
//! decodes them once, and leaves out, with a warning naming the address, any
//! logo it cannot fetch or whose digest differs: a project changing its logo
//! must not stop a build. The logos are not stored in the repository.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use image::imageops::FilterType;
use image::{ImageReader, Limits};
use sha2::{Digest, Sha256};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::fixtures::recorded;

/// The only host logos are fetched from.
const LOGO_HOST: &str = "cdn.modrinth.com";
/// The largest download accepted for one logo.
const MAX_DOWNLOAD_BYTES: usize = 4 * 1024 * 1024;
/// The side length every logo is scaled to, which is enough for the largest
/// size the interface draws one at.
const LOGO_SIDE: u32 = 64;
/// The largest source image side accepted before decoding allocates anything.
const MAX_SOURCE_SIDE: u32 = 2048;

/// A decoded logo, kept as plain pixels so it can be shared between threads.
struct Pixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

static LOGOS: OnceLock<BTreeMap<String, Pixels>> = OnceLock::new();

/// The logo for `url`, or an empty image when logos were not prepared or this
/// one is unavailable.
pub(crate) fn logo(url: &str) -> Image {
    let Some(pixels) = LOGOS.get().and_then(|logos| logos.get(url)) else {
        return Image::default();
    };
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &pixels.rgba,
        pixels.width,
        pixels.height,
    ))
}

/// Loads every logo the recorded rows name, fetching missing ones into `cache`
/// unless `offline`. A logo that is unavailable, cannot be fetched, or does not
/// match its recorded digest is left empty.
pub(crate) fn prepare(cache: &Path, offline: bool) -> Result<()> {
    let wanted = recorded::logo_sources();
    if !offline {
        std::fs::create_dir_all(cache)
            .with_context(|| format!("create the logo cache {}", cache.display()))?;
    }
    let mut logos = BTreeMap::new();
    let mut runtime = None;
    for (url, sha256) in wanted {
        let file = cache.join(format!("{sha256}.bin"));
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if offline {
                    continue;
                }
                let runtime = match &runtime {
                    Some(runtime) => runtime,
                    None => runtime.insert(
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .context("start the logo download runtime")?,
                    ),
                };
                let bytes = match runtime.block_on(download(&url)) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        eprintln!("warning: leaving the logo {url} empty: {error:#}");
                        continue;
                    }
                };
                if !matches_recorded(&url, &bytes, &sha256) {
                    continue;
                }
                publish(&file, &bytes)?;
                bytes
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read the cached logo {}", file.display()));
            }
        };
        ensure!(
            digest(&bytes) == sha256,
            "the cached logo {} does not match its recorded digest; delete that file and run again",
            file.display()
        );
        logos.insert(url, decode(&bytes)?);
    }
    LOGOS
        .set(logos)
        .map_err(|_| anyhow!("the project logos were already prepared in this process"))
}

/// Writes `bytes` beside `target` and renames them into place, so a run that
/// stops mid-write never leaves a partial file under the final name.
fn publish(target: &Path, bytes: &[u8]) -> Result<()> {
    let staged = target.with_extension("part");
    std::fs::write(&staged, bytes)
        .with_context(|| format!("stage the logo {}", staged.display()))?;
    std::fs::rename(&staged, target).with_context(|| format!("store the logo {}", target.display()))
}

/// Whether the bytes served for `url` are the ones the fixtures recorded. A
/// difference is reported, not fatal, so the logo is left out and the rest of
/// the run goes on.
fn matches_recorded(url: &str, bytes: &[u8], sha256: &str) -> bool {
    let matches = digest(bytes) == sha256;
    if !matches {
        eprintln!(
            "warning: leaving the logo {url} empty: it no longer matches the recorded digest; update the digest in data/ when the new logo is right"
        );
    }
    matches
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn download(url: &str) -> Result<Vec<u8>> {
    let parsed =
        reqwest::Url::parse(url).with_context(|| format!("parse the logo address {url}"))?;
    ensure!(
        parsed.scheme() == "https" && parsed.host_str() == Some(LOGO_HOST),
        "refusing to fetch a logo from {url}"
    );
    let client = reqwest::Client::builder()
        .user_agent("Ephervex documentation screenshots")
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build the logo client")?;
    let mut response = client
        .get(parsed)
        .send()
        .await
        .with_context(|| format!("fetch the logo {url}"))?;
    if !response.status().is_success() {
        bail!("the logo {url} answered {}", response.status());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("read the logo {url}"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_DOWNLOAD_BYTES,
            "the logo {url} is larger than {MAX_DOWNLOAD_BYTES} bytes"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<Pixels> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("recognize the logo format")?;
    reader.limits(limits);
    let source = reader.decode().context("decode a logo")?;
    let scaled = image::imageops::resize(
        &source.to_rgba8(),
        LOGO_SIDE,
        LOGO_SIDE,
        FilterType::Lanczos3,
    );
    Ok(Pixels {
        width: scaled.width(),
        height: scaled.height(),
        rgba: scaled.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logo_whose_bytes_changed_is_left_out_instead_of_stopping_the_run() {
        let recorded = digest(b"the recorded logo");
        assert!(matches_recorded(
            "https://cdn.modrinth.com/x.png",
            b"the recorded logo",
            &recorded
        ));
        assert!(!matches_recorded(
            "https://cdn.modrinth.com/x.png",
            b"a re-encoded logo",
            &recorded
        ));
    }
}
