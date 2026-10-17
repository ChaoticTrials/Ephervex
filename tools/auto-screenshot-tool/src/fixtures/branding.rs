//! Logos the screenshots show in place of the default: the one a documented
//! theme folder ships in its `branding/` folder, which is the Ephervex logo
//! with every hue turned a third of the way around the color wheel so it reads
//! as a different image, and the Halloween theme's compiled-in mascot.

use std::sync::OnceLock;

use image::{ImageFormat, RgbaImage};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

static LOGO: OnceLock<Option<RgbaImage>> = OnceLock::new();
static MASCOT: OnceLock<Option<RgbaImage>> = OnceLock::new();

/// The pumpkin ghost the launcher compiles in as the Halloween theme's logo
/// and window icon, or an empty image, which shows the default, if it cannot
/// be decoded.
pub(crate) fn halloween_mascot() -> Image {
    to_image(
        MASCOT
            .get_or_init(|| {
                image::load_from_memory_with_format(
                    include_bytes!("../../../../assets/themes/halloween.png"),
                    ImageFormat::Png,
                )
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
    fn the_halloween_mascot_is_decoded() {
        assert!(halloween_mascot().size().width > 0);
    }

    #[test]
    fn the_theme_logo_is_decoded_within_the_branding_size_limit() {
        let logo = theme_logo();
        let size = logo.size();
        assert!(size.width > 0 && size.width <= 1024);
        assert!(size.height > 0 && size.height <= 1024);
    }
}
