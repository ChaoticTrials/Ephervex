//! Headless snapshots of the compiled desktop component.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use ephervex_gui::ui::{AppWindow, BuiltInPalettes, GuiI18n, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use image::{ImageBuffer, RgbaImage, imageops::FilterType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Image, LogicalPosition, Model, PhysicalSize};

use crate::catalog::{Interaction, Region, RegionBounds, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeName {
    Light,
    Dark,
    /// The Halloween season's theme, shown in place of Dark.
    Halloween,
}

impl ThemeName {
    pub(crate) const ALL: [Self; 3] = [Self::Light, Self::Dark, Self::Halloween];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Halloween => "halloween",
        }
    }
}

impl std::str::FromStr for ThemeName {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            "halloween" => Ok(Self::Halloween),
            _ => Err(format!(
                "unknown theme {value:?}; use light, dark, or halloween"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dimensions {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Dimensions {
    pub(crate) const HD: Self = Self {
        width: 1920,
        height: 1080,
    };
    pub(crate) const QHD: Self = Self {
        width: 2560,
        height: 1440,
    };
    pub(crate) const ALL: [Self; 2] = [Self::HD, Self::QHD];

    pub(crate) fn label(self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

impl std::str::FromStr for Dimensions {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (width, height) = value
            .split_once('x')
            .ok_or_else(|| "size must be WIDTHxHEIGHT".to_owned())?;
        let width: u32 = width.parse().map_err(|_| "invalid width".to_owned())?;
        let height: u32 = height.parse().map_err(|_| "invalid height".to_owned())?;
        if !(980..=3840).contains(&width) || !(680..=2160).contains(&height) {
            return Err("size must be within 980x680 and 3840x2160".to_owned());
        }
        Ok(Self { width, height })
    }
}

pub(crate) struct CaptureOptions<'a> {
    pub(crate) states: &'a [State],
    pub(crate) themes: &'a [ThemeName],
    pub(crate) sizes: &'a [Dimensions],
    pub(crate) output: &'a Path,
    pub(crate) derive_1080: bool,
}

pub(crate) fn initialize() -> Result<()> {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .context("initialize Slint's headless software renderer")
}

/// Holds the process-wide locale and the thread's headless platform for one test.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn render_session() -> std::sync::MutexGuard<'static, ()> {
    static RENDERING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = RENDERING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    initialize().expect("headless renderer");
    ephervex_i18n::select_language("en-US").expect("bundled locale");
    guard
}

pub(crate) fn generate(options: &CaptureOptions<'_>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for state in options.states {
        for size in options.sizes {
            let derive_full =
                options.derive_1080 && state.same_layout_at_both_sizes && *size == Dimensions::HD;
            if derive_full && state.regions.iter().all(|region| region.name == "full") {
                continue;
            }
            // Each size starts from a fresh window, so a menu or panel an
            // interaction opened never carries into the next size.
            let ui = new_window(state)?;
            // The testing backend converts this to logical coordinates at a
            // fixed 1.0 scale factor. No desktop compositor is involved.
            ui.window()
                .set_size(PhysicalSize::new(size.width, size.height));
            ui.show().context("show the headless Ephervex window")?;
            if let Some(interaction) = state.interaction {
                perform(&ui, interaction)?;
            }

            let state_logo = (ui.get_theme_logo(), ui.get_theme_icon());
            for theme in options.themes {
                show_theme(&ui, *theme, &state_logo);
                slint::platform::update_timers_and_animations();
                let buffer = ui
                    .window()
                    .take_snapshot()
                    .context("render the Ephervex window")?;
                ensure!(
                    buffer.width() == size.width && buffer.height() == size.height,
                    "renderer produced {}x{} for a requested {}",
                    buffer.width(),
                    buffer.height(),
                    size.label()
                );
                let image =
                    ImageBuffer::from_raw(size.width, size.height, buffer.as_bytes().to_vec())
                        .context("convert Slint's RGBA snapshot")?;
                for region in state.regions {
                    if derive_full && region.name == "full" {
                        continue;
                    }
                    let image = crop_region(&ui, &image, region)?;
                    let path = output_path(options.output, state, region, *theme, *size);
                    write_png(&path, &image)?;
                    files.push(path);
                }
            }
        }
        if options.derive_1080 && state.same_layout_at_both_sizes {
            let region = state
                .regions
                .iter()
                .find(|region| region.name == "full")
                .context("a documented state needs a full-window region")?;
            for theme in options.themes {
                let source = output_path(options.output, state, region, *theme, Dimensions::QHD);
                let source = image::open(&source)
                    .with_context(|| format!("open 1440p source {}", source.display()))?
                    .into_rgba8();
                let derivative = derive_1080(&source, state)?;
                let path = output_path(options.output, state, region, *theme, Dimensions::HD);
                write_png(&path, &derivative)?;
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Paints `ui` in `theme` the way the launcher does, with the season's note
/// and logo while the Halloween theme stands in for the chosen Dark. A state
/// that shows a theme of the user's own keeps it, as no season replaces one.
fn show_theme(ui: &AppWindow, theme: ThemeName, (logo, icon): &(Image, Image)) {
    let user_theme = logo.size().width > 0;
    let seasonal = theme == ThemeName::Halloween && !user_theme;
    let palettes = ui.global::<BuiltInPalettes>();
    let (palette, chosen) = match theme {
        ThemeName::Light => (palettes.get_workbench(), ThemeName::Light),
        _ if seasonal => (palettes.get_halloween(), ThemeName::Dark),
        ThemeName::Dark | ThemeName::Halloween => (palettes.get_workbench_dark(), ThemeName::Dark),
    };
    ui.global::<Theme>().set_palette(palette);
    ui.set_application_theme(
        if chosen == ThemeName::Light {
            "workbench"
        } else {
            "dark"
        }
        .into(),
    );
    ui.set_theme_index(i32::from(chosen == ThemeName::Dark));
    ui.set_shown_theme(
        if seasonal {
            "season:halloween"
        } else {
            chosen.name()
        }
        .into(),
    );
    ui.set_seasonal_theme_in_season(seasonal);
    ui.set_seasonal_theme_note(
        if seasonal {
            lookup_args(
                "gui-seasonal-theme-shown",
                [
                    ("season", Arg::from(lookup("gui-seasonal-theme-halloween"))),
                    ("value", Arg::from(lookup("gui-color-scheme-dark"))),
                ],
            )
        } else {
            String::new()
        }
        .into(),
    );
    if seasonal {
        let mascot = crate::fixtures::branding::halloween_mascot();
        ui.set_theme_logo(mascot.clone());
        ui.set_theme_icon(mascot);
    } else {
        ui.set_theme_logo(logo.clone());
        ui.set_theme_icon(icon.clone());
    }
}

fn new_window(state: &State) -> Result<AppWindow> {
    let ui = AppWindow::new().context("create the Ephervex window")?;
    install_i18n(&ui);
    fixtures_for_state(&ui, state);
    Ok(ui)
}

fn fixtures_for_state(ui: &AppWindow, state: &State) {
    crate::fixtures::install_base(ui);
    (state.show)(ui);
    crate::fixtures::java::fill_shown_hosts(ui);
}

fn install_i18n(ui: &AppWindow) {
    let i18n = ui.global::<GuiI18n>();
    i18n.on_text(|id| lookup(id.as_str()).into());
    i18n.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::Count(i64::from(count)))]).into()
    });
    i18n.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.to_string()))]).into()
    });
    i18n.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.to_string())),
                ("second", Arg::from(second.to_string())),
            ],
        )
        .into()
    });
}

/// The largest visible element with this accessible label.
fn find_visible(ui: &AppWindow, label: &str) -> Result<ElementHandle> {
    find_visible_by(ui, label, |left, right| left.total_cmp(&right))
}

/// The visible element with this label that `order` ranks last by area.
fn find_visible_by(
    ui: &AppWindow,
    label: &str,
    order: impl Fn(f32, f32) -> std::cmp::Ordering,
) -> Result<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.size().width > 0.0 && element.size().height > 0.0)
        .max_by(|left, right| {
            let left_area = left.size().width * left.size().height;
            let right_area = right.size().width * right.size().height;
            order(left_area, right_area)
        })
        .with_context(|| format!("find visible UI region {label:?}"))
}

/// Opens the menu or dropdown a state documents by pressing the real control.
// Window sizes are capped at 3840x2160, well inside f32 precision.
#[allow(clippy::cast_precision_loss)]
fn perform(ui: &AppWindow, interaction: Interaction) -> Result<()> {
    let (label, button) = match interaction {
        Interaction::Click(label) => (label, PointerEventButton::Left),
        Interaction::RightClick(label) => (label, PointerEventButton::Right),
        Interaction::ScrollTo(label) => return scroll_to(ui, label),
    };
    let element = match interaction {
        // A click targets the control itself, which sits inside the larger
        // card that may share its label. A context menu belongs to the card.
        Interaction::Click(_) => find_visible_by(ui, label, |left, right| right.total_cmp(&left)),
        Interaction::RightClick(_) | Interaction::ScrollTo(_) => find_visible(ui, label),
    }?;
    let position = element.absolute_position();
    let size = element.size();
    let at = LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    );
    let window = ui.window();
    let window_size = window.size();
    ensure!(
        at.x >= 0.0
            && at.y >= 0.0
            && at.x < window_size.width as f32
            && at.y < window_size.height as f32,
        "UI element {label:?} is outside the window"
    );
    let before = window
        .take_snapshot()
        .context("render the window before the interaction")?;
    window.dispatch_event(WindowEvent::PointerMoved { position: at });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: at,
        button,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: at,
        button,
    });
    slint::platform::update_timers_and_animations();
    let after = window
        .take_snapshot()
        .context("render the window after the interaction")?;
    ensure!(
        before.as_bytes() != after.as_bytes(),
        "interacting with {label:?} changed nothing"
    );
    Ok(())
}

/// Where a workspace surface's scroll area starts, below the host header.
const SURFACE_TOP: f32 = 190.0;
/// Room left above a scrolled-to element, so the heading over it stays in view.
const HEADING_ROOM: f32 = 90.0;
/// The most wheel steps a scroll takes before it gives up.
const MAX_SCROLL_STEPS: usize = 64;

/// The top of the visible scroll area: below a failed-run notice when one sits
/// above the surface.
fn scroll_area_top(ui: &AppWindow) -> f32 {
    ui.get_host_views()
        .iter()
        .filter(|view| !view.run_failure_title.is_empty())
        .filter_map(|view| find_visible(ui, &view.run_failure_title).ok())
        .map(|notice| notice.absolute_position().y + notice.size().height)
        .fold(SURFACE_TOP, f32::max)
}

/// Scrolls the content under the window's center until the element with this
/// label sits near the top of the scroll area, the way a reader scrolls to it.
/// Content outside the scroll viewport is not laid out, so the scroll steps
/// down until the element appears before lining it up.
// Window sizes are capped at 3840x2160, well inside f32 precision.
#[allow(clippy::cast_precision_loss)]
fn scroll_to(ui: &AppWindow, label: &str) -> Result<()> {
    let window = ui.window();
    let size = window.size();
    let at = LogicalPosition::new(size.width as f32 / 2.0, size.height as f32 / 2.0);
    let wheel = |delta_y: f32| -> Result<Vec<u8>> {
        window.dispatch_event(WindowEvent::PointerMoved { position: at });
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: at,
            delta_x: 0.0,
            delta_y,
        });
        slint::platform::update_timers_and_animations();
        Ok(window
            .take_snapshot()
            .context("lay out the window after scrolling")?
            .as_bytes()
            .to_vec())
    };
    let mut shown = wheel(0.0)?;
    let area_top = scroll_area_top(ui);
    for _ in 0..MAX_SCROLL_STEPS {
        if let Ok(element) = find_visible(ui, label) {
            let top = element.absolute_position().y;
            let target = area_top + HEADING_ROOM;
            if (top - target).abs() > 1.0 {
                wheel(target - top)?;
            }
            let settled = find_visible(ui, label)?.absolute_position().y;
            ensure!(
                settled >= area_top,
                "scrolling left {label:?} under the top of its scroll area"
            );
            return Ok(());
        }
        let next = wheel(-(size.height as f32) / 3.0)?;
        ensure!(
            next != shown,
            "scrolled to the end without finding {label:?}"
        );
        shown = next;
    }
    bail!("{label:?} did not appear within {MAX_SCROLL_STEPS} scroll steps")
}

/// Crops the element with the region's accessible label, with its own margin on
/// each side: `(left, right, top, bottom)`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn crop_around_label(
    ui: &AppWindow,
    image: &RgbaImage,
    region: &Region,
    (left, right, top, bottom): (f32, f32, f32, f32),
) -> Result<RgbaImage> {
    let label = region
        .accessible_label
        .context("an AroundLabel region needs an accessible label")?;
    let element = find_visible(ui, label)?;
    let position = element.absolute_position();
    let size = element.size();
    let x = (position.x - left).max(0.0).floor() as u32;
    let y = (position.y - top).max(0.0).floor() as u32;
    let end_x = ((position.x + size.width + right).ceil() as u32).min(image.width());
    let end_y = ((position.y + size.height + bottom).ceil() as u32).min(image.height());
    ensure!(end_x > x && end_y > y, "UI region {label:?} has no area");
    Ok(image::imageops::crop_imm(image, x, y, end_x - x, end_y - y).to_image())
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn crop_region(ui: &AppWindow, image: &RgbaImage, region: &Region) -> Result<RgbaImage> {
    if let Some(bounds) = region.bounds {
        let (x, y, width, height) = match bounds {
            RegionBounds::Centered {
                left,
                top,
                width,
                height,
            } => (image.width() as f32 / 2.0 + left, top, width, height),
            RegionBounds::RightAligned {
                right,
                top,
                width,
                height,
            } => (image.width() as f32 - right - width, top, width, height),
            RegionBounds::AroundLabel {
                left,
                right,
                top,
                bottom,
            } => return crop_around_label(ui, image, region, (left, right, top, bottom)),
        };
        ensure!(
            x.is_finite()
                && y.is_finite()
                && width.is_finite()
                && height.is_finite()
                && x >= 0.0
                && y >= 0.0
                && width > 0.0
                && height > 0.0
                && x + width <= image.width() as f32
                && y + height <= image.height() as f32,
            "UI region {:?} exceeds the window",
            region.name
        );
        return Ok(image::imageops::crop_imm(
            image,
            x.floor() as u32,
            y.floor() as u32,
            width.ceil() as u32,
            height.ceil() as u32,
        )
        .to_image());
    }
    let Some(label) = region.accessible_label else {
        return Ok(image.clone());
    };
    let element = find_visible(ui, label)?;
    let position = element.absolute_position();
    let size = element.size();
    ensure!(
        position.x.is_finite()
            && position.y.is_finite()
            && size.width.is_finite()
            && size.height.is_finite(),
        "UI region {label:?} has invalid geometry"
    );
    ensure!(
        position.x >= 0.0
            && position.y >= 0.0
            && position.x + size.width <= image.width() as f32
            && position.y + size.height <= image.height() as f32,
        "UI region {label:?} exceeds the window"
    );
    // The checked window is at most 3840x2160. Converting its logical
    // coordinates to integer pixel bounds intentionally drops fractions.
    let (padding_x, padding_y) = region.padding;
    let x = (position.x - padding_x).max(0.0).floor() as u32;
    let y = (position.y - padding_y).max(0.0).floor() as u32;
    let right = ((position.x + size.width + padding_x).ceil() as u32).min(image.width());
    let bottom = ((position.y + size.height + padding_y).ceil() as u32).min(image.height());
    ensure!(right > x && bottom > y, "UI region {label:?} has no area");
    Ok(image::imageops::crop_imm(image, x, y, right - x, bottom - y).to_image())
}

fn output_path(
    output: &Path,
    state: &State,
    region: &Region,
    theme: ThemeName,
    size: Dimensions,
) -> PathBuf {
    let directory = output.join(state.name);
    let directory = if region.name == "full" {
        directory
    } else {
        directory.join(region.name)
    };
    directory.join(format!("{}-{}.png", theme.name(), size.label()))
}

fn write_png(path: &Path, image: &RgbaImage) -> Result<()> {
    let parent = path
        .parent()
        .context("screenshot has no output directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    image
        .save_with_format(path, image::ImageFormat::Png)
        .with_context(|| format!("write {}", path.display()))
}

/// Resizing is reserved for states explicitly marked as layout-identical.
pub(crate) fn derive_1080(source: &RgbaImage, state: &State) -> Result<RgbaImage> {
    if !state.same_layout_at_both_sizes {
        bail!("{} changes layout between 1440p and 1080p", state.name);
    }
    ensure!(
        source.width() == 2560 && source.height() == 1440,
        "a derivative needs a 2560x1440 source"
    );
    Ok(image::imageops::resize(
        source,
        1920,
        1080,
        FilterType::Lanczos3,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn dimensions_reject_unbounded_and_malformed_sizes() {
        assert!("1920x1080".parse::<Dimensions>().is_ok());
        assert!("10000x10000".parse::<Dimensions>().is_err());
        assert!("1920".parse::<Dimensions>().is_err());
    }

    #[test]
    fn derivative_requires_an_explicitly_identical_layout() {
        let source = RgbaImage::new(2560, 1440);
        assert!(derive_1080(&source, &crate::catalog::STATES[0]).is_err());
        let identical = State {
            same_layout_at_both_sizes: true,
            ..crate::catalog::STATES[0]
        };
        let derivative = derive_1080(&source, &identical).expect("explicitly identical layout");
        assert_eq!(derivative.dimensions(), (1920, 1080));
    }

    #[test]
    fn actual_window_captures_every_theme_and_a_ui_region() {
        let _session = crate::capture::render_session();
        let output = tempfile::tempdir().expect("temporary output");
        let files = generate(&CaptureOptions {
            states: &crate::catalog::STATES[..1],
            themes: &ThemeName::ALL,
            sizes: &[Dimensions::HD],
            output: output.path(),
            derive_1080: false,
        })
        .expect("capture the real component");

        assert_eq!(files.len(), 6);
        let full = crate::catalog::STATES[0].regions[0];
        let light = image::open(output_path(
            output.path(),
            &crate::catalog::STATES[0],
            &full,
            ThemeName::Light,
            Dimensions::HD,
        ))
        .expect("light PNG")
        .into_rgba8();
        let dark = image::open(output_path(
            output.path(),
            &crate::catalog::STATES[0],
            &full,
            ThemeName::Dark,
            Dimensions::HD,
        ))
        .expect("dark PNG")
        .into_rgba8();
        let halloween = image::open(output_path(
            output.path(),
            &crate::catalog::STATES[0],
            &full,
            ThemeName::Halloween,
            Dimensions::HD,
        ))
        .expect("Halloween PNG")
        .into_rgba8();
        assert_eq!(light.dimensions(), (1920, 1080));
        assert_eq!(dark.dimensions(), (1920, 1080));
        assert_ne!(light.get_pixel(300, 600), dark.get_pixel(300, 600));
        assert_ne!(dark.get_pixel(300, 600), halloween.get_pixel(300, 600));

        let crop = image::open(output_path(
            output.path(),
            &crate::catalog::STATES[0],
            &crate::catalog::STATES[0].regions[1],
            ThemeName::Light,
            Dimensions::HD,
        ))
        .expect("selected client crop");
        assert!(crop.width() > 300 && crop.height() > 100);
        assert!(crop.width() < light.width() && crop.height() < light.height());
    }

    #[test]
    fn derived_1080_full_window_is_not_rendered_natively() {
        let _session = crate::capture::render_session();
        let derived_output = tempfile::tempdir().expect("temporary derivative output");
        let state = State {
            same_layout_at_both_sizes: true,
            ..crate::catalog::STATES[0]
        };
        let files = generate(&CaptureOptions {
            states: &[state],
            themes: &[ThemeName::Light],
            sizes: &[Dimensions::HD, Dimensions::QHD],
            output: derived_output.path(),
            derive_1080: true,
        })
        .expect("capture and derive images");
        assert_eq!(files.len(), 4);
        let full = &state.regions[0];
        let qhd = image::open(output_path(
            derived_output.path(),
            &state,
            full,
            ThemeName::Light,
            Dimensions::QHD,
        ))
        .expect("native 1440p image")
        .into_rgba8();
        let hd = image::open(output_path(
            derived_output.path(),
            &state,
            full,
            ThemeName::Light,
            Dimensions::HD,
        ))
        .expect("derived 1080p image")
        .into_rgba8();
        assert_eq!(hd, derive_1080(&qhd, &state).expect("resize source"));
        let crop = image::open(output_path(
            derived_output.path(),
            &state,
            &state.regions[1],
            ThemeName::Light,
            Dimensions::HD,
        ))
        .expect("native 1080p crop");
        assert!(crop.width() < hd.width());

        let full_only_output = tempfile::tempdir().expect("temporary full-window output");
        let full_only = State {
            same_layout_at_both_sizes: true,
            ..crate::catalog::STATES[3]
        };
        let files = generate(&CaptureOptions {
            states: &[full_only],
            themes: &[ThemeName::Light],
            sizes: &[Dimensions::HD, Dimensions::QHD],
            output: full_only_output.path(),
            derive_1080: true,
        })
        .expect("derive a state with no native 1080p regions");
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|path| path.is_file()));
    }
}
