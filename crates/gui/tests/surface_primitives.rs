//! The hard-shadow plate keeps the size of its face and offsets the shadow by
//! its lift, and the grid floor draws hairlines at the grid step up to a cap.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

slint::slint! {
    import { Plate, GridFloor } from "../ui/components/surface.slint";
    import { Theme } from "../ui/theme.slint";

    export component SurfaceProbe inherits Window {
        in property <length> lift: 6px;
        out property <brush> face-fill: Theme.surface;
        out property <brush> face-outline: Theme.border-strong;
        out property <brush> shadow: Theme.shadow-hard;
        out property <int> grid-size: Theme.grid-size / 1px;
        background: Theme.background;
        Plate {
            x: 10px;
            y: 20px;
            width: 120px;
            height: 80px;
            lift: root.lift;
        }
        GridFloor {
            y: 200px;
            width: parent.width;
            height: parent.height - 200px;
        }
    }
}

type Pixel = [u8; 3];

fn window(width: u32, height: u32) -> SurfaceProbe {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = SurfaceProbe::new().expect("window");
    ui.window()
        .set_size(slint::PhysicalSize::new(width, height));
    ui
}

fn render(ui: &SurfaceProbe) -> SharedPixelBuffer<Rgba8Pixel> {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render")
}

fn pixel(image: &SharedPixelBuffer<Rgba8Pixel>, x: u32, y: u32) -> Pixel {
    let at = image.as_slice()[(y * image.width() + x) as usize];
    [at.r, at.g, at.b]
}

fn rgb(brush: &slint::Brush) -> Pixel {
    let color = brush.color();
    [color.red(), color.green(), color.blue()]
}

#[test]
fn plate_keeps_the_face_size_and_offsets_the_shadow_by_the_lift() {
    let ui = window(640, 480);
    let image = render(&ui);
    let (face, outline, shadow) = (
        rgb(&ui.get_face_fill()),
        rgb(&ui.get_face_outline()),
        rgb(&ui.get_shadow()),
    );
    // The face is 120x80 at (10, 20) and the shadow reaches 6px past it.
    assert_eq!(pixel(&image, 60, 60), face);
    assert_eq!(pixel(&image, 10, 60), outline);
    assert_eq!(pixel(&image, 129, 60), outline);
    assert_eq!(pixel(&image, 133, 60), shadow);
    assert_eq!(pixel(&image, 60, 103), shadow);
    assert_ne!(pixel(&image, 137, 60), shadow);
    assert_ne!(pixel(&image, 60, 107), shadow);
    assert_ne!(pixel(&image, 12, 102), shadow);

    ui.set_lift(2.0);
    let image = render(&ui);
    assert_eq!(pixel(&image, 131, 60), shadow);
    assert_ne!(pixel(&image, 133, 60), shadow);
}

#[test]
fn grid_floor_draws_hairlines_at_the_grid_step_and_stops_at_the_cap() {
    let ui = window(6600, 300);
    let image = render(&ui);
    let step = u32::try_from(ui.get_grid_size()).expect("a positive grid size");
    let background = pixel(&image, step / 2, 250);
    assert_ne!(pixel(&image, step, 250), background, "first vertical line");
    assert_ne!(pixel(&image, 2 * step, 250), background);
    assert_eq!(pixel(&image, step + step / 2, 250), background);
    // Rows start 200px down.
    assert_ne!(
        pixel(&image, step / 2, 200 + step),
        background,
        "a row line"
    );
    assert_eq!(pixel(&image, step / 2, 200 + step / 2), background);
    // 200 lines cover x = 0..=199 steps; the next one is not drawn.
    assert_ne!(pixel(&image, 199 * step, 250), background);
    assert_eq!(pixel(&image, 200 * step, 250), background);
    // Nothing is drawn above the floor.
    assert_eq!(pixel(&image, step, 150), pixel(&image, step / 2, 150));
}
