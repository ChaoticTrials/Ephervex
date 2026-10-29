//! Logos the screenshots show in place of the default: the one a documented
//! theme folder ships in its `branding/` folder, which is the Ephervex logo
//! with every hue turned a third of the way around the color wheel so it reads
//! as a different image, and the seasonal themes' compiled-in mascots.

use std::sync::OnceLock;

use image::{ImageFormat, RgbaImage};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

static LOGO: OnceLock<Option<RgbaImage>> = OnceLock::new();

static MASCOTS: [OnceLock<Option<RgbaImage>>; 4] = [
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
    OnceLock::new(),
];

/// The mascot the launcher compiles in as a seasonal theme's logo and window
/// icon, or an empty image, which shows the default, if it cannot be decoded.
pub(crate) fn seasonal_mascot(season: &str) -> Image {
    let (slot, bytes): (usize, &[u8]) = match season {
        "halloween" => (0, include_bytes!("../../../../assets/themes/halloween.png")),
        "christmas" => (1, include_bytes!("../../../../assets/themes/christmas.png")),
        "pride" => (2, include_bytes!("../../../../assets/themes/pride.png")),
        "april-fools" => (
            3,
            include_bytes!("../../../../assets/themes/april-fools.png"),
        ),
        _ => return Image::default(),
    };
    to_image(
        MASCOTS[slot]
            .get_or_init(|| {
                image::load_from_memory_with_format(bytes, ImageFormat::Png)
                    .ok()
                    .map(image::DynamicImage::into_rgba8)
            })
            .as_ref(),
    )
}

fn to_image(image: Option<&RgbaImage>) -> Image {
    let Some(image) = image else {
        return Image::default();
    };
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

/// The theme's logo, or an empty image, which shows the default, if the
/// embedded logo cannot be decoded.
pub(crate) fn theme_logo() -> Image {
    to_image(LOGO.get_or_init(recolored_logo).as_ref())
}

fn recolored_logo() -> Option<RgbaImage> {
    let mut logo = image::load_from_memory_with_format(
        include_bytes!("../../../../assets/logo.png"),
        ImageFormat::Png,
    )
    .ok()?
    .into_rgba8();
    // Moving each channel to the next turns every hue by 120 degrees.
    for pixel in logo.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        pixel.0 = [blue, red, green, alpha];
    }
    Some(logo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_seasonal_mascot_is_decoded() {
        for season in ["halloween", "christmas", "pride", "april-fools"] {
            assert!(seasonal_mascot(season).size().width > 0, "{season}");
        }
    }

    #[test]
    fn the_theme_logo_is_decoded_within_the_branding_size_limit() {
        let logo = theme_logo();
        let size = logo.size();
        assert!(size.width > 0 && size.width <= 1024);
        assert!(size.height > 0 && size.height <= 1024);
    }
}
