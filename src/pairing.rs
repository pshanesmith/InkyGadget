//! The Matter pairing screen: the QR code and the manual code, drawn on the panel while the frame has no controller.
//!
//! The screen is drawn in pure palette colours (no dithering), so the QR code stays crisp and scannable.

use crate::overlay::{DISPLAY_H, DISPLAY_W};
use crate::palette::{BLACK, BLUE, WHITE};
use crate::welcome::{centred, text};

/// What the controller needs to add the frame: the QR code and the manual pairing code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The text inside the QR code (`MT:...`).
    pub qr_text: String,
    /// The 11-digit manual code, grouped `XXXX-XXX-XXXX`.
    pub manual_code: String,
    /// Modules per side of the QR code.
    pub width: usize,
    /// The QR code's modules, row by row; `true` is dark.
    pub modules: Vec<bool>,
}

/// What the main loop can ask the Matter node to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatterCommand {
    /// Open the commissioning window again (the 15 minutes after start-up have passed).
    OpenWindow,
}

/// The quiet zone the QR specification asks for, in modules, on every side.
const QUIET: usize = 4;
/// The most the QR code may take, in pixels, including its quiet zone.
const QR_BOX: usize = 320;

/// Pixels per module: as large as fits, at most 8, and never below 3 (which a phone camera still reads).
pub fn module_px(width: usize) -> usize {
    (QR_BOX / (width + 2 * QUIET)).clamp(3, 8)
}

/// The whole screen as palette indices, `DISPLAY_W` x `DISPLAY_H`.
pub fn render(code: &Code) -> Vec<u8> {
    let mut b = vec![WHITE; DISPLAY_W * DISPLAY_H];
    for x in 0..DISPLAY_W {
        for y in 0..10 {
            b[y * DISPLAY_W + x] = BLUE;
            b[(DISPLAY_H - 1 - y) * DISPLAY_W + x] = BLUE;
        }
    }
    centred(&mut b, 24, "Pair InkyGadget", 4, BLACK);

    // The QR code on the left, white quiet zone included (the background is white already).
    let m = module_px(code.width);
    let (x0, y0) = (28 + QUIET * m, 86 + QUIET * m);
    for (i, &dark) in code.modules.iter().enumerate().take(code.width * code.width) {
        if dark {
            let (mx, my) = (i % code.width, i / code.width);
            for dy in 0..m {
                for dx in 0..m {
                    let (x, y) = (x0 + mx * m + dx, y0 + my * m + dy);
                    if x < DISPLAY_W && y < DISPLAY_H {
                        b[y * DISPLAY_W + x] = BLACK;
                    }
                }
            }
        }
    }

    // The words on the right.
    let x = 344;
    text(&mut b, x, 104, "Scan the QR or", 2, BLACK);
    text(&mut b, x, 128, "enter the code:", 2, BLACK);
    text(&mut b, x, 160, &code.manual_code, 2, BLACK);
    let help = ["Home Assistant:", "Add device >", "Matter device", "", "Apple Home:", "Add Accessory"];
    for (i, line) in help.iter().enumerate() {
        text(&mut b, x, 220 + i * 26, line, 2, BLACK);
    }
    centred(&mut b, 410, "Hold button D 3 s to show this screen again", 2, BLACK);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QR-like pattern of the given width with a dark frame, enough to check placement.
    fn code(width: usize) -> Code {
        let modules = (0..width * width).map(|i| i % width == 0 || i % width == width - 1 || i / width == 0 || i / width == width - 1).collect();
        Code { qr_text: "MT:TEST".into(), manual_code: "2310-483-8485".into(), width, modules }
    }

    #[test]
    fn the_qr_code_is_drawn_black_on_white_with_a_quiet_zone() {
        let c = code(29);
        let b = render(&c);
        let m = module_px(29);
        assert_eq!(m, 8);
        let (x0, y0) = (28 + 4 * m, 86 + 4 * m);
        assert_eq!(b[y0 * DISPLAY_W + x0], BLACK, "first module");
        assert_eq!(b[(y0 + 28 * m + m - 1) * DISPLAY_W + x0 + 28 * m + m - 1], BLACK, "last module");
        assert_eq!(b[(y0 + 3 * m) * DISPLAY_W + x0 + 3 * m], WHITE, "an inner light module");
        assert_eq!(b[y0 * DISPLAY_W + x0 - 1], WHITE, "quiet zone on the left");
        assert_eq!(b[(y0 - 1) * DISPLAY_W + x0], WHITE, "quiet zone above");
    }

    #[test]
    fn only_palette_black_white_and_blue_are_used() {
        let b = render(&code(29));
        assert!(b.iter().all(|&p| p == BLACK || p == WHITE || p == BLUE));
        assert!(b.iter().filter(|&&p| p == BLACK).count() > 3000, "text and code were drawn");
    }

    #[test]
    fn every_qr_size_fits_above_the_footer() {
        for width in (21..=41).step_by(4) {
            let m = module_px(width);
            assert!(m >= 3, "{width}");
            let bottom = 86 + (width + 2 * QUIET) * m;
            assert!(bottom <= 410, "width {width}: the code would reach {bottom}");
            let b = render(&code(width));
            assert_eq!(b.len(), DISPLAY_W * DISPLAY_H);
        }
    }

    #[test]
    fn a_short_module_list_never_panics() {
        render(&Code { qr_text: String::new(), manual_code: "x".repeat(200), width: 29, modules: vec![true; 5] });
    }
}
