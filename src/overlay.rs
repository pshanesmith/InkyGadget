//! The day/date and temperature plates, drawn after dithering in pure palette colours so the text stays crisp.
//!
//! The text is Inter Bold (SIL OFL, see NOTICE). The panel has no grey, so each glyph is rasterised and cut off at a
//! coverage threshold instead of anti-aliased.

use crate::palette::{luma, Palette, BLACK, WHITE};
use ab_glyph::{point, Font, FontRef, Glyph, PxScale, ScaleFont};
use std::sync::OnceLock;

pub const DISPLAY_W: usize = 600;
pub const DISPLAY_H: usize = 448;

/// What to print. `None` hides that plate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overlay {
    pub left: Option<String>,
    pub right: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlateStyle {
    /// Black on white or white on black, whichever contrasts with the picture under it.
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy)]
pub struct OverlayStyle {
    /// Text size: the font is `FONT_PX_PER_SCALE` x this many pixels tall (2 gives 22 px).
    pub scale: usize,
    pub margin: usize,
    pub plate: PlateStyle,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        OverlayStyle { scale: 2, margin: 0, plate: PlateStyle::Auto }
    }
}

const FONT_PX_PER_SCALE: f32 = 11.0;
/// Share of a glyph pixel that must be covered to be inked; below 0.5 makes the strokes a little bolder.
const COVERAGE: f32 = 0.42;
const PAD_X: usize = 8;
const PAD_Y: usize = 4;
const CORNER_RADIUS: f32 = 7.0;

static FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/Inter-Bold.ttf");

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(FONT_BYTES).expect("embedded font is valid"))
}

struct Run {
    glyphs: Vec<Glyph>,
    width: f32,
    height: f32,
}

fn layout(text: &str, scale: usize) -> Run {
    let px = PxScale::from(FONT_PX_PER_SCALE * scale as f32);
    let sf = font().as_scaled(px);
    let (mut x, mut prev) = (0.0f32, None);
    let mut glyphs = Vec::new();
    for ch in text.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            x += sf.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(px, point(x, sf.ascent())));
        x += sf.h_advance(id);
        prev = Some(id);
    }
    Run { glyphs, width: x, height: sf.ascent() - sf.descent() }
}

/// Width and height in pixels of the plate that `text` gets.
pub fn plate_size(text: &str, scale: usize) -> (usize, usize) {
    let run = layout(text, scale);
    (run.width.ceil() as usize + 2 * PAD_X, run.height.ceil() as usize + 2 * PAD_Y)
}

fn put(buf: &mut [u8], x: i32, y: i32, v: u8) {
    if x >= 0 && y >= 0 && (x as usize) < DISPLAY_W && (y as usize) < DISPLAY_H {
        buf[y as usize * DISPLAY_W + x as usize] = v;
    }
}

fn draw_text(buf: &mut [u8], x0: usize, y0: usize, run: &Run, colour: u8) {
    for g in &run.glyphs {
        if let Some(outline) = font().outline_glyph(g.clone()) {
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cover| {
                if cover >= COVERAGE {
                    put(buf, x0 as i32 + b.min.x as i32 + gx as i32, y0 as i32 + b.min.y as i32 + gy as i32, colour);
                }
            });
        }
    }
}

fn plate_colours(buf: &[u8], pal: &Palette, x: usize, y: usize, w: usize, h: usize, style: PlateStyle) -> (u8, u8) {
    match style {
        PlateStyle::Light => (WHITE, BLACK),
        PlateStyle::Dark => (BLACK, WHITE),
        PlateStyle::Auto => {
            let (mut sum, mut n) = (0u64, 0u64);
            for yy in y..(y + h).min(DISPLAY_H) {
                for xx in x..(x + w).min(DISPLAY_W) {
                    sum += luma(pal[buf[yy * DISPLAY_W + xx] as usize]) as u64;
                    n += 1;
                }
            }
            if n > 0 && sum / n > 140 {
                (BLACK, WHITE) // bright picture: dark plate
            } else {
                (WHITE, BLACK)
            }
        }
    }
}

/// Is the pixel centre (px, py) inside a `w`x`h` rectangle with the given corner radii (top-left, top-right,
/// bottom-right, bottom-left), shrunk by `inset` on every side?
fn inside(px: f32, py: f32, w: f32, h: f32, r: [f32; 4], inset: f32) -> bool {
    if px < inset || px >= w - inset || py < inset || py >= h - inset {
        return false;
    }
    let corners = [(r[0], r[0], r[0]), (r[1], w - r[1], r[1]), (r[2], w - r[2], h - r[2]), (r[3], r[3], h - r[3])];
    for (i, (rad, cx, cy)) in corners.into_iter().enumerate() {
        let in_region = match i {
            0 => px < rad && py < rad,
            1 => px > cx && py < rad,
            2 => px > cx && py > cy,
            _ => px < rad && py > cy,
        };
        if rad > 0.0 && in_region {
            return ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() <= rad - inset;
        }
    }
    true
}

fn plate(buf: &mut [u8], pal: &Palette, text: &str, right_aligned: bool, st: &OverlayStyle) {
    let run = layout(text, st.scale);
    let (w, h) = plate_size(text, st.scale);
    let x = if right_aligned { DISPLAY_W.saturating_sub(st.margin + w) } else { st.margin };
    let y = st.margin;
    let (fill, ink) = plate_colours(buf, pal, x, y, w, h, st.plate);
    // A corner on the edge of the panel stays square; the others are rounded.
    let round = |cx: usize, cy: usize| if cx == 0 || cx >= DISPLAY_W || cy == 0 || cy >= DISPLAY_H { 0.0 } else { CORNER_RADIUS };
    let r = [round(x, y), round(x + w, y), round(x + w, y + h), round(x, y + h)];
    let (wf, hf) = (w as f32, h as f32);
    for yy in 0..h {
        for xx in 0..w {
            let (px, py) = (xx as f32 + 0.5, yy as f32 + 0.5);
            if inside(px, py, wf, hf, r, 0.0) {
                let v = if inside(px, py, wf, hf, r, 1.0) { fill } else { ink };
                put(buf, (x + xx) as i32, (y + yy) as i32, v);
            }
        }
    }
    draw_text(buf, x + PAD_X, y + PAD_Y, &run, ink);
}

/// Draw the plates onto a `DISPLAY_W`x`DISPLAY_H` buffer of palette indices.
pub fn draw(buf: &mut [u8], pal: &Palette, overlay: &Overlay, st: &OverlayStyle) {
    assert_eq!(buf.len(), DISPLAY_W * DISPLAY_H);
    if let Some(t) = &overlay.left {
        plate(buf, pal, t, false, st);
    }
    if let Some(t) = &overlay.right {
        plate(buf, pal, t, true, st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::blend;

    #[test]
    fn plates_land_in_their_corners_and_leave_the_rest_alone() {
        let pal = blend(0.5);
        let mut buf = vec![WHITE; DISPLAY_W * DISPLAY_H];
        let o = Overlay { left: Some("Thu Oct 29".into()), right: Some("H 62° L 41°".into()) };
        draw(&mut buf, &pal, &o, &OverlayStyle { plate: PlateStyle::Light, ..Default::default() });
        assert_eq!(buf[0], BLACK, "left plate outline starts in the top-left corner");
        assert_eq!(buf[DISPLAY_W - 1], BLACK, "right plate ends in the top-right corner");
        let mut spaced = vec![WHITE; DISPLAY_W * DISPLAY_H];
        draw(&mut spaced, &pal, &o, &OverlayStyle { margin: 8, plate: PlateStyle::Light, ..Default::default() });
        assert_eq!(spaced[8 * DISPLAY_W + 8 + 30], BLACK, "a margin moves the plate in");
        assert_eq!(spaced[8 * DISPLAY_W + 8], WHITE, "its corner is rounded");
        assert_eq!(spaced[0], WHITE);
        assert_eq!(buf[(DISPLAY_H - 1) * DISPLAY_W + DISPLAY_W / 2], WHITE, "the picture area is untouched");
        let text_pixels = buf.iter().filter(|&&p| p == BLACK).count();
        assert!(text_pixels > 300, "some text was drawn: {text_pixels}");
    }

    #[test]
    fn auto_plate_is_dark_on_a_bright_picture_and_light_on_a_dark_one() {
        let pal = blend(0.5);
        let mut bright = vec![WHITE; DISPLAY_W * DISPLAY_H];
        draw(&mut bright, &pal, &Overlay { left: Some("Mon".into()), right: None }, &OverlayStyle::default());
        assert_eq!(bright[3 * DISPLAY_W + 4], BLACK);
        let mut dark = vec![BLACK; DISPLAY_W * DISPLAY_H];
        draw(&mut dark, &pal, &Overlay { left: Some("Mon".into()), right: None }, &OverlayStyle::default());
        assert_eq!(dark[3 * DISPLAY_W + 4], WHITE);
    }

    #[test]
    fn only_the_corner_away_from_the_panel_edge_is_rounded() {
        let pal = blend(0.5);
        const BG: u8 = 200; // not a plate colour, so "untouched" is visible
        let mut buf = vec![BG; DISPLAY_W * DISPLAY_H];
        let o = Overlay { left: Some("Friday October 30".into()), right: None };
        draw(&mut buf, &pal, &o, &OverlayStyle { plate: PlateStyle::Dark, ..Default::default() });
        let (w, h) = plate_size("Friday October 30", 2);
        assert_eq!(buf[(h - 1) * DISPLAY_W], WHITE, "bottom-left sits on the panel edge and stays square");
        assert_eq!(buf[w - 1], WHITE, "top-right sits on the panel edge and stays square");
        assert_eq!(buf[(h - 1) * DISPLAY_W + w - 1], BG, "bottom-right is rounded");
    }

    #[test]
    fn the_longest_date_and_temperatures_fit_side_by_side() {
        let (a, _) = plate_size("Wednesday September 30", 2);
        let (b, _) = plate_size("H 100° L -100°", 2);
        assert!(a + b <= DISPLAY_W, "{a} + {b}");
    }

    #[test]
    fn nothing_to_draw_changes_nothing() {
        let pal = blend(0.5);
        let mut buf = vec![WHITE; DISPLAY_W * DISPLAY_H];
        draw(&mut buf, &pal, &Overlay::default(), &OverlayStyle::default());
        assert!(buf.iter().all(|&p| p == WHITE));
    }
}
