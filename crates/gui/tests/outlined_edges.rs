//! A switch is drawn on whole pixels with nothing between its outline and its
//! fill, wherever a layout puts it, and a disabled one has one uniform outline.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

slint::slint! {
    import { Toggle } from "../ui/components/controls.slint";
    import { Theme } from "../ui/theme.slint";

    export component EdgeProbe inherits Window {
        out property <brush> page: Theme.surface;
        out property <brush> ink: Theme.border-strong;
        out property <brush> track: Theme.toggle-on;
        out property <brush> faded-ink: Theme.border-strong.mix(Theme.surface, Theme.disabled-opacity);
        out property <brush> faded-track: Theme.toggle-on.mix(Theme.surface, Theme.disabled-opacity);
        out property <int> outline-px: Theme.strong-border-width / 1px;
        background: Theme.surface;
        // One on a whole pixel and one half a pixel off both ways.
        Toggle { x: 10px; y: 10px; checked: true; }
        Toggle { x: 80.5px; y: 10.5px; checked: true; }
        Toggle { x: 10px; y: 40px; checked: true; enabled: false; }
    }
}

type Pixel = [u8; 3];

fn rgb(brush: &slint::Brush) -> Pixel {
    let color = brush.color();
    [color.red(), color.green(), color.blue()]
}

fn pixel(image: &SharedPixelBuffer<Rgba8Pixel>, x: usize, y: usize) -> Pixel {
    let at = image.as_slice()[y * image.width() as usize + x];
    [at.r, at.g, at.b]
}

/// The first and last rows of exact `ink` pixels inside the columns `from..to`.
fn ink_rows(
    image: &SharedPixelBuffer<Rgba8Pixel>,
    ink: Pixel,
    from: usize,
    to: usize,
    rows: std::ops::Range<usize>,
) -> (usize, usize) {
    let hits: Vec<usize> = rows
        .filter(|&y| (from..to).any(|x| pixel(image, x, y) == ink))
        .collect();
    (hits[0], hits[hits.len() - 1])
}

/// Whether `color` lies on the line from `ink` to `fill`, where an antialiased
/// edge between the two puts it. A pixel the page shows through lies off it.
fn blends_ink_and_fill(color: Pixel, ink: Pixel, fill: Pixel) -> bool {
    let along: Vec<f64> = (0..3)
        .filter(|&c| ink[c] != fill[c])
        .map(|c| {
            (f64::from(color[c]) - f64::from(ink[c])) / (f64::from(fill[c]) - f64::from(ink[c]))
        })
        .collect();
    let first = along[0];
    (0.0..=1.0).contains(&first) && along.iter().all(|t| (t - first).abs() < 0.03)
}

fn render() -> (EdgeProbe, SharedPixelBuffer<Rgba8Pixel>) {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = EdgeProbe::new().expect("window");
    ui.window().set_size(slint::PhysicalSize::new(160, 80));
    ui.show().expect("show");
    slint::platform::update_timers_and_animations();
    let image = ui.window().take_snapshot().expect("render");
    (ui, image)
}

#[test]
fn a_switch_has_a_crisp_outline_and_no_gap_to_its_fill_wherever_it_is_placed() {
    let (ui, image) = render();
    let (page, ink, track) = (
        rgb(&ui.get_page()),
        rgb(&ui.get_ink()),
        rgb(&ui.get_track()),
    );
    let outline = usize::try_from(ui.get_outline_px()).expect("a positive width");

    // The second switch is placed at 80.5, so it sits at 81 once snapped. The
    // outermost column of a rounded end has no full-ink pixel, so the left edge
    // comes from the placement and the rows from the ink.
    for (from, to, left) in [(0, 70, 10), (70, 160, 81)] {
        let (top, bottom) = ink_rows(&image, ink, from, to, 0..36);
        assert_eq!(bottom - top + 1, 24, "height at {from}");
        let (middle_x, middle_y) = (left + 21, usize::midpoint(top, bottom));
        // The straight edges: page, outline, fill, with no half-covered row.
        assert_eq!(
            pixel(&image, middle_x - 8, top - 1),
            page,
            "above at {from}"
        );
        assert_eq!(
            pixel(&image, middle_x - 8, top + outline),
            track,
            "below the top at {from}"
        );
        assert_eq!(
            pixel(&image, middle_x - 8, bottom + 1),
            page,
            "under at {from}"
        );
        assert_eq!(
            pixel(&image, middle_x - 8, bottom - outline),
            track,
            "above the bottom at {from}"
        );
        assert_eq!(
            pixel(&image, left - 1, middle_y),
            page,
            "left of the end at {from}"
        );
        // On the curve of the left end, within a pixel of the outline's inner
        // edge, the outline covers part of each pixel. The rest is the fill:
        // a fill that stopped at the inner edge would leave the page showing
        // through those pixels, off the line between ink and fill.
        let (cx, cy) = (
            f64::from(u32::try_from(left + 12).expect("small")),
            f64::from(u32::try_from(top).expect("small")) + 12.0,
        );
        let mut probed = 0;
        for y in top..=bottom {
            for x in left..left + 12 {
                let (dx, dy) = (
                    f64::from(u32::try_from(x).expect("small")) + 0.5 - cx,
                    f64::from(u32::try_from(y).expect("small")) + 0.5 - cy,
                );
                let distance = dx.hypot(dy);
                if (9.0..=11.0).contains(&distance) && dx < -2.0 {
                    probed += 1;
                    let edge = pixel(&image, x, y);
                    assert!(
                        blends_ink_and_fill(edge, ink, track),
                        "{edge:?} at ({x}, {y}) of the switch at {from} is not between the outline and the fill"
                    );
                }
            }
        }
        assert!(probed > 10, "the probe covers the curve at {from}");
    }
}

#[test]
fn a_disabled_switch_has_one_uniform_outline() {
    let (ui, image) = render();
    let (page, faded_ink, faded_track) = (
        rgb(&ui.get_page()),
        rgb(&ui.get_faded_ink()),
        rgb(&ui.get_faded_track()),
    );
    let (top, bottom) = ink_rows(&image, faded_ink, 0, 70, 36..80);
    let middle_x = 10 + 13;
    assert_eq!(bottom - top + 1, 24);
    // Both rows of the outline are the same faded ink, not a fill-tinted inner row.
    assert_eq!(pixel(&image, middle_x, top + 1), faded_ink);
    assert_eq!(pixel(&image, middle_x, bottom - 1), faded_ink);
    assert_eq!(pixel(&image, middle_x, top - 1), page);
    assert_eq!(pixel(&image, middle_x, top + 2), faded_track);
}
