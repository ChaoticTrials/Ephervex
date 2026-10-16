//! Decoding an image that arrived from outside Ephervex.
//!
//! A bounded file is not a bounded image: a few kilobytes of PNG can declare a
//! canvas of gigabytes. The decoded size is therefore checked from the header
//! before anything is allocated and enforced again by the decoder, because a
//! header is only a claim.

use std::io::{BufRead, Seek};

use image::{DynamicImage, ImageFormat, ImageReader};

/// How large a decoded image may be.
///
/// The side and pixel limits catch different things: a square at the side
/// limit can still have far more pixels than wanted, and a long thin strip can
/// be within the pixel limit with an absurd side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageBounds {
    pub(crate) max_side: u32,
    pub(crate) max_pixels: u64,
}

impl ImageBounds {
    /// The ceiling the decoder itself is given, in bytes: four bytes per pixel
    /// at the pixel ceiling, doubled for the intermediate buffers a decoder
    /// may use.
    const fn max_decode_bytes(self) -> u64 {
        self.max_pixels * 4 * 2
    }
}

/// Bounds for mod artwork, which is kept as decoded.
///
/// Generous for something drawn at a few dozen logical pixels, and small enough
/// that the worst case is a rounding error rather than a memory event.
pub(crate) const ARTWORK_BOUNDS: ImageBounds = ImageBounds {
    max_side: 2048,
    max_pixels: 1024 * 1024,
};

/// Bounds for a picture a user picks as a host icon.
///
/// Only the scaled-down copy is kept, so these admit an ordinary photo or
/// screenshot. The decoder may briefly hold about 128 MiB at the ceiling.
pub(crate) const ICON_SOURCE_BOUNDS: ImageBounds = ImageBounds {
    max_side: 8192,
    max_pixels: 4096 * 4096,
};

/// Why an image was not decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageRefusal {
    /// Longer on a side than the bounds allow.
    TooLarge { width: u32, height: u32 },
    /// More pixels than the bounds allow.
    TooManyPixels { pixels: u64 },
    /// A format the caller did not allow.
    UnsupportedFormat,
    /// Malformed data, or a format this build has no decoder for.
    Unusable(String),
}

impl std::fmt::Display for ImageRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { width, height } => {
                write!(formatter, "{width}x{height} is larger than allowed")
            }
            Self::TooManyPixels { pixels } => {
                write!(formatter, "{pixels} pixels is more than allowed")
            }
            Self::UnsupportedFormat => formatter.write_str("this image format is not accepted"),
            Self::Unusable(detail) => formatter.write_str(detail),
        }
    }
}

/// Decodes one image within `bounds`.
///
/// The format is taken from the bytes, never from a file name, and `allowed`
/// (when given) narrows it further. `open` is called twice: once to read the
/// header and once to decode.
pub(crate) fn decode_bounded<R: BufRead + Seek>(
    open: impl Fn() -> std::io::Result<R>,
    allowed: Option<&[ImageFormat]>,
    bounds: ImageBounds,
) -> Result<DynamicImage, ImageRefusal> {
    let reader = || {
        open()
            .and_then(|source| ImageReader::new(source).with_guessed_format())
            .map_err(|error| ImageRefusal::Unusable(error.to_string()))
    };

    let header = reader()?;
    if let Some(allowed) = allowed
        && let Some(format) = header.format()
        && !allowed.contains(&format)
    {
        return Err(ImageRefusal::UnsupportedFormat);
    }
    let (width, height) = header
        .into_dimensions()
        .map_err(|error| ImageRefusal::Unusable(error.to_string()))?;
    if width > bounds.max_side || height > bounds.max_side {
        return Err(ImageRefusal::TooLarge { width, height });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > bounds.max_pixels {
        return Err(ImageRefusal::TooManyPixels { pixels });
    }

    let mut decoder = reader()?;
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(bounds.max_side);
    limits.max_image_height = Some(bounds.max_side);
    limits.max_alloc = Some(bounds.max_decode_bytes());
    decoder.limits(limits);
    decoder
        .decode()
        .map_err(|error| ImageRefusal::Unusable(error.to_string()))
}

/// Encodes a decoded image as PNG.
pub(crate) fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, ImageRefusal> {
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|error| ImageRefusal::Unusable(error.to_string()))?;
    Ok(png)
}
