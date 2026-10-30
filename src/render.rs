//! From a prepared 600x448 picture to the panel's frame of palette indices.

use crate::dither::dither;
use crate::overlay::{draw, Overlay, OverlayStyle, DISPLAY_H, DISPLAY_W};
use crate::palette::{self, Palette};

/// A picture already fitted to the display (bars included), RGB8, `DISPLAY_W`x`DISPLAY_H`.
pub struct Prepared {
    pub rgb: Vec<u8>,
}

impl Prepared {
    /// A flat colour, used for the welcome screen background and in tests.
    pub fn solid(rgb: [u8; 3]) -> Self {
        Prepared { rgb: rgb.iter().copied().cycle().take(DISPLAY_W * DISPLAY_H * 3).collect() }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// Pimoroni's `saturation`: how strongly the panel's measured colours are used when quantising.
    pub saturation: f32,
    pub overlay: OverlayStyle,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { saturation: 0.5, overlay: OverlayStyle::default() }
    }
}

/// Dither, then draw the overlay. The result is one palette index per pixel, row by row.
pub fn render(p: &Prepared, overlay: &Overlay, opt: &RenderOptions) -> Vec<u8> {
    let pal: Palette = palette::blend(opt.saturation);
    let mut frame = dither(&p.rgb, DISPLAY_W, DISPLAY_H, &pal);
    draw(&mut frame, &pal, overlay, &opt.overlay);
    frame
}

/// What the panel will actually show, as RGB8 (the measured colours), for previews.
pub fn to_preview_rgb(frame: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(frame.len() * 3);
    for &i in frame {
        out.extend_from_slice(&palette::SATURATED[(i as usize).min(6)]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_picture_renders_white_apart_from_plates() {
        let frame = render(&Prepared::solid([255, 255, 255]), &Overlay::default(), &RenderOptions::default());
        assert_eq!(frame.len(), DISPLAY_W * DISPLAY_H);
        assert!(frame.iter().all(|&p| p == palette::WHITE));
    }
}
