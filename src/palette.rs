//! The seven colours of the Inky Impression 5.7" (UC8159) and the quantisation palette.
//!
//! The values come from Pimoroni's driver (MIT, see NOTICE). The panel's real colours are the "saturated"
//! set; Pimoroni quantises against a blend of the saturated and a "desaturated" (pure) set, which looks
//! better on the panel than the measured colours alone.

pub const BLACK: u8 = 0;
pub const WHITE: u8 = 1;
pub const GREEN: u8 = 2;
pub const BLUE: u8 = 3;
pub const RED: u8 = 4;
pub const YELLOW: u8 = 5;
pub const ORANGE: u8 = 6;

pub type Palette = [[u8; 3]; 7];

pub const DESATURATED: Palette =
    [[0, 0, 0], [255, 255, 255], [0, 255, 0], [0, 0, 255], [255, 0, 0], [255, 255, 0], [255, 140, 0]];

pub const SATURATED: Palette =
    [[57, 48, 57], [255, 255, 255], [58, 91, 70], [61, 59, 94], [156, 72, 75], [208, 190, 71], [177, 106, 73]];

/// Pimoroni's `_palette_blend`: `saturation` 0.0 is the pure colours, 1.0 the measured panel colours.
pub fn blend(saturation: f32) -> Palette {
    let s = saturation.clamp(0.0, 1.0);
    let mut out = [[0u8; 3]; 7];
    for (i, o) in out.iter_mut().enumerate() {
        for c in 0..3 {
            o[c] = (SATURATED[i][c] as f32 * s + DESATURATED[i][c] as f32 * (1.0 - s)) as u8;
        }
    }
    out
}

/// Index of the palette colour closest to `rgb` (squared distance in RGB).
pub fn nearest(pal: &Palette, rgb: [i32; 3]) -> u8 {
    let mut best = 0u8;
    let mut best_d = i32::MAX;
    for (i, p) in pal.iter().enumerate() {
        let dr = rgb[0] - p[0] as i32;
        let dg = rgb[1] - p[1] as i32;
        let db = rgb[2] - p[2] as i32;
        let d = dr * dr + dg * dg + db * db;
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// Perceived brightness 0..=255 of a palette colour.
pub fn luma(rgb: [u8; 3]) -> u32 {
    (rgb[0] as u32 * 299 + rgb[1] as u32 * 587 + rgb[2] as u32 * 114) / 1000
}
