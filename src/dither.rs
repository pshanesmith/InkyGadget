//! Floyd-Steinberg dithering to the panel palette (serpentine scan, integer error terms).

use crate::palette::{nearest, Palette};

/// `rgb` is tightly packed RGB8 of `w`x`h`; the result holds one palette index per pixel.
pub fn dither(rgb: &[u8], w: usize, h: usize, pal: &Palette) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    // Error rows carry 3 channels per pixel with one pixel of padding each side, scaled by 16.
    let stride = (w + 2) * 3;
    let mut cur = vec![0i32; stride];
    let mut next = vec![0i32; stride];
    for y in 0..h {
        let ltr = y % 2 == 0;
        for i in 0..w {
            let x = if ltr { i } else { w - 1 - i };
            let o = (y * w + x) * 3;
            let e = (x + 1) * 3;
            let mut v = [0i32; 3];
            for c in 0..3 {
                v[c] = (rgb[o + c] as i32 + (cur[e + c] + 8).div_euclid(16)).clamp(0, 255);
            }
            let idx = nearest(pal, v);
            out[y * w + x] = idx;
            let p = pal[idx as usize];
            for c in 0..3 {
                let err = v[c] - p[c] as i32;
                // Weights 7/16 ahead, 3/16 behind-below, 5/16 below, 1/16 ahead-below (mirrored on right-to-left rows).
                let (a, b) = if ltr { (e + 3, e - 3) } else { (e - 3, e + 3) };
                cur[a + c] += err * 7;
                next[b + c] += err * 3;
                next[e + c] += err * 5;
                next[a + c] += err;
            }
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|v| *v = 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::blend;

    #[test]
    fn exact_palette_colours_stay_put() {
        let pal = blend(0.5);
        let mut rgb = Vec::new();
        for p in pal.iter() {
            rgb.extend_from_slice(p);
        }
        let idx = dither(&rgb, 7, 1, &pal);
        assert_eq!(idx, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn error_diffusion_preserves_the_average_colour() {
        let pal = blend(0.5);
        for grey in [64u8, 128, 200] {
            let rgb = vec![grey; 48 * 48 * 3];
            let idx = dither(&rgb, 48, 48, &pal);
            let mut mean = [0f64; 3];
            for &i in &idx {
                for c in 0..3 {
                    mean[c] += pal[i as usize][c] as f64 / idx.len() as f64;
                }
            }
            for m in mean {
                assert!((m - grey as f64).abs() < 6.0, "grey {grey} came out as {mean:?}");
            }
        }
    }
}
