//! Box-average downscaling that accepts the source in any order of pieces.
//!
//! Every source pixel is added to the output pixel whose area it falls in, and each output pixel is the
//! mean of its bin. That makes it exact for whole ratios and good for the rest, and it lets a decoder feed
//! tiles one at a time so the full-size picture never has to be in memory.

pub struct Scaler {
    sw: u32,
    sh: u32,
    tw: u32,
    th: u32,
    sum: Vec<u32>,
    cols: Vec<u32>,
    rows: Vec<u32>,
    /// Output column and row of every source column and row, worked out once: a 64-bit divide per pixel is slow on
    /// ARMv6 (no divide instruction), and a 24 MP photo has 24 million pixels.
    xbin: Vec<u16>,
    ybin: Vec<u16>,
}

/// Size that fits `sw`x`sh` inside `mw`x`mh` keeping the aspect ratio (never zero).
pub fn fit_size(sw: u32, sh: u32, mw: u32, mh: u32) -> (u32, u32) {
    let (sw64, sh64) = (sw.max(1) as u64, sh.max(1) as u64);
    if sw64 * mh as u64 >= sh64 * mw as u64 {
        (mw, ((sh64 * mw as u64 + sw64 / 2) / sw64).clamp(1, mh as u64) as u32)
    } else {
        (((sw64 * mh as u64 + sh64 / 2) / sh64).clamp(1, mw as u64) as u32, mh)
    }
}

impl Scaler {
    /// `tw`x`th` must not be larger than `sw`x`sh` in either direction (downscaling only).
    pub fn new(sw: u32, sh: u32, tw: u32, th: u32) -> Self {
        assert!(tw >= 1 && th >= 1 && tw <= sw && th <= sh, "Scaler only shrinks");
        assert!(tw <= u16::MAX as u32 && th <= u16::MAX as u32, "target too large");
        let xbin: Vec<u16> = (0..sw).map(|x| Self::bin(x, sw, tw) as u16).collect();
        let ybin: Vec<u16> = (0..sh).map(|y| Self::bin(y, sh, th) as u16).collect();
        let mut cols = vec![0u32; tw as usize];
        for &b in &xbin {
            cols[b as usize] += 1;
        }
        let mut rows = vec![0u32; th as usize];
        for &b in &ybin {
            rows[b as usize] += 1;
        }
        Scaler { sw, sh, tw, th, sum: vec![0; tw as usize * th as usize * 3], cols, rows, xbin, ybin }
    }

    fn bin(v: u32, src: u32, dst: u32) -> usize {
        ((v as u64 * dst as u64) / src as u64) as usize
    }

    /// Add an RGB8 block whose top-left corner is at (`x0`, `y0`) in the source.
    pub fn add_block(&mut self, x0: u32, y0: u32, w: u32, h: u32, rgb: &[u8]) {
        debug_assert_eq!(rgb.len(), w as usize * h as usize * 3);
        if x0 >= self.sw || y0 >= self.sh {
            return;
        }
        let tw = self.tw as usize;
        let cols = (w as usize).min(self.sw.saturating_sub(x0) as usize);
        for row in 0..h {
            let y = y0 + row;
            if y >= self.sh {
                break;
            }
            let ty = self.ybin[y as usize] as usize;
            let src = &rgb[row as usize * w as usize * 3..][..cols * 3];
            let xs = &self.xbin[x0 as usize..][..cols];
            let sums = &mut self.sum[ty * tw * 3..][..tw * 3];
            for (px, &tx) in src.chunks_exact(3).zip(xs) {
                let o = tx as usize * 3;
                sums[o] += px[0] as u32;
                sums[o + 1] += px[1] as u32;
                sums[o + 2] += px[2] as u32;
            }
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.tw, self.th)
    }

    /// The averaged image as tightly packed RGB8.
    pub fn finish(self) -> Vec<u8> {
        let tw = self.tw as usize;
        let mut out = vec![0u8; self.sum.len()];
        for ty in 0..self.th as usize {
            for tx in 0..tw {
                let n = (self.cols[tx] * self.rows[ty]).max(1);
                let o = (ty * tw + tx) * 3;
                for c in 0..3 {
                    out[o + c] = ((self.sum[o + c] + n / 2) / n) as u8;
                }
            }
        }
        out
    }
}

/// Shrink a whole RGB8 image in one go.
pub fn shrink(rgb: &[u8], sw: u32, sh: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut s = Scaler::new(sw, sh, tw, th);
    s.add_block(0, 0, sw, sh, rgb);
    s.finish()
}

/// Bilinear resize, used only when a photo is smaller than the display and has to be enlarged.
pub fn enlarge(rgb: &[u8], sw: u32, sh: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut out = vec![0u8; tw as usize * th as usize * 3];
    let fx = sw as f32 / tw as f32;
    let fy = sh as f32 / th as f32;
    for ty in 0..th {
        let sy = ((ty as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let y0 = sy as u32;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = sy - y0 as f32;
        for tx in 0..tw {
            let sx = ((tx as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let x0 = sx as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let wx = sx - x0 as f32;
            for c in 0..3 {
                let p = |x: u32, y: u32| rgb[(y as usize * sw as usize + x as usize) * 3 + c] as f32;
                let top = p(x0, y0) * (1.0 - wx) + p(x1, y0) * wx;
                let bot = p(x0, y1) * (1.0 - wx) + p(x1, y1) * wx;
                out[(ty as usize * tw as usize + tx as usize) * 3 + c] = (top * (1.0 - wy) + bot * wy + 0.5) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_ratio() {
        assert_eq!(fit_size(4032, 3024, 600, 448), (597, 448));
        assert_eq!(fit_size(3024, 4032, 600, 448), (336, 448));
        assert_eq!(fit_size(600, 448, 600, 448), (600, 448));
        assert_eq!(fit_size(1, 1000, 600, 448), (1, 448));
    }

    #[test]
    fn whole_ratio_is_exact_mean() {
        let img = [0u8, 0, 0, 100, 100, 100, 200, 200, 200, 100, 100, 100];
        assert_eq!(shrink(&img, 2, 2, 1, 1), vec![100, 100, 100]);
    }

    #[test]
    fn pieces_in_any_order_match_whole() {
        let (w, h) = (8u32, 6u32);
        let img: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let whole = shrink(&img, w, h, 3, 2);
        let mut s = Scaler::new(w, h, 3, 2);
        // Left half first, then right half, one row block each.
        let half = |x0: u32| -> Vec<u8> {
            let mut b = Vec::new();
            for y in 0..h {
                b.extend_from_slice(&img[((y * w + x0) * 3) as usize..][..12]);
            }
            b
        };
        s.add_block(4, 0, 4, h, &half(4));
        s.add_block(0, 0, 4, h, &half(0));
        assert_eq!(s.finish(), whole);
    }

    #[test]
    fn blocks_overhanging_the_picture_are_clipped() {
        // A decoder's tiles can run past the edge of the picture; the overhang must be ignored, not panic.
        let mut s = Scaler::new(3, 3, 1, 1);
        s.add_block(0, 0, 4, 4, &[100u8; 4 * 4 * 3]);
        assert_eq!(s.finish(), vec![100, 100, 100]);
        let mut s = Scaler::new(3, 3, 1, 1);
        s.add_block(2, 2, 4, 4, &[100u8; 4 * 4 * 3]);
        s.add_block(5, 5, 2, 2, &[7u8; 2 * 2 * 3]); // entirely outside, past both edges
        s.add_block(5, 0, 2, 2, &[7u8; 2 * 2 * 3]); // past the right edge only
        s.add_block(0, 5, 2, 2, &[7u8; 2 * 2 * 3]); // past the bottom edge only
        // Only the corner pixel was inside: 100 spread over the nine pixels the bin should hold (the rest never arrived).
        assert_eq!(s.finish(), vec![11, 11, 11]);
    }

    #[test]
    fn enlarge_of_flat_colour_is_flat() {
        let img = vec![10u8, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30];
        let out = enlarge(&img, 2, 2, 5, 4);
        assert!(out.chunks(3).all(|p| p == [10, 20, 30]));
    }
}
