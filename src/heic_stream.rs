//! HEIC decoding that never holds the whole picture.
//!
//! A phone's HEIC is a grid of small independent tiles (512x512 on an iPhone). Each tile is decoded, turned into
//! RGB and added straight into the downscaler, so the peak memory is the file, one tile and the small result, not
//! the 100 MB or more that decoding a 24 MP photo into one RGB buffer takes. A single-picture HEIC is converted in
//! bands of rows instead.
//!
//! Anything this path does not handle exactly (a crop, a mirror, an unusual colour matrix, tiles of different sizes)
//! returns `Ok(None)` and the caller uses the whole-image decoder.

use crate::decode::{Limits, Photo};
use crate::overlay::{DISPLAY_H, DISPLAY_W};
use crate::scale::{fit_size, Scaler};
use anyhow::{bail, Context as _, Result};
use heic_rs::context::Context;
use heic_rs::hevc::{self, ChromaFormat, Frame};
use heic_rs::props::colr::{MatrixCoefficients, Nclx, Range};
use heic_rs::{Rotation, Transform};
use image::metadata::Orientation;

/// Fixed-point bits in the colour coefficients.
const FRAC: u32 = 12;
const ROUND: i32 = 1 << (FRAC - 1);
/// Rows converted at a time for a picture that is not tiled.
const BAND_ROWS: u32 = 64;

fn he<T>(r: heic_rs::Result<T>) -> Result<T> {
    r.map_err(|e| anyhow::anyhow!("HEIC decode failed: {e:?}"))
}

/// YCbCr to RGB for one matrix and range, in 12-bit fixed point on 8-bit samples.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Yuv {
    y_offset: i32,
    y_gain: i32,
    r_cr: i32,
    g_cr: i32,
    g_cb: i32,
    b_cb: i32,
}

impl Yuv {
    fn new(nclx: Nclx) -> Option<Yuv> {
        let (kr, kb) = match nclx.matrix {
            MatrixCoefficients::Bt709 | MatrixCoefficients::Unspecified => (0.2126, 0.0722),
            MatrixCoefficients::Bt601 => (0.299, 0.114),
            MatrixCoefficients::Smpte240m => (0.212, 0.087),
            MatrixCoefficients::Bt2020Ncl => (0.2627, 0.0593),
            // Identity (GBR), unknown codes and anything added later: leave those to the whole-image decoder.
            _ => return None,
        };
        let (y_offset, ky, kc) = match nclx.range {
            Range::Limited => (16, 255.0 / 219.0, 255.0 / 224.0),
            Range::Full => (0, 1.0, 1.0),
        };
        let kg = 1.0 - kr - kb;
        let fx = |v: f64| (v * (1 << FRAC) as f64).round() as i32;
        Some(Yuv {
            y_offset,
            y_gain: fx(ky),
            r_cr: fx(2.0 * (1.0 - kr) * kc),
            g_cr: fx(2.0 * kr * (1.0 - kr) / kg * kc),
            g_cb: fx(2.0 * kb * (1.0 - kb) / kg * kc),
            b_cb: fx(2.0 * (1.0 - kb) * kc),
        })
    }

    fn rgb(&self, y: i32, cb: i32, cr: i32) -> [u8; 3] {
        let (cb, cr) = (cb - 128, cr - 128);
        let l = (y - self.y_offset) * self.y_gain + ROUND;
        let clamp = |v: i32| (v >> FRAC).clamp(0, 255) as u8;
        [clamp(l + self.r_cr * cr), clamp(l - self.g_cr * cr - self.g_cb * cb), clamp(l + self.b_cb * cb)]
    }
}

/// A sample at any bit depth as an 8-bit value.
fn to8(v: u16, depth: u8) -> i32 {
    let shift = depth.saturating_sub(8) as u32;
    if shift == 0 {
        v as i32
    } else {
        ((v as u32 + (1 << (shift - 1))) >> shift).min(255) as i32
    }
}

/// RGB for rows `y0..y0 + rows` of `f`, tightly packed.
fn convert_rows(f: &Frame, yuv: &Yuv, y0: u32, rows: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(f.width as usize * rows as usize * 3);
    let mono = f.chroma == ChromaFormat::Monochrome;
    for y in y0..y0 + rows {
        let yrow = &f.y[y as usize * f.y_stride as usize..][..f.width as usize];
        let crow = (y >> (f.chroma == ChromaFormat::Yuv420) as u32) as usize * f.c_stride as usize;
        for (x, &yv) in yrow.iter().enumerate() {
            let (cb, cr) = if mono {
                (128, 128)
            } else {
                let cx = if f.chroma == ChromaFormat::Yuv444 { x } else { x >> 1 };
                (to8(f.cb[crow + cx], f.bit_depth), to8(f.cr[crow + cx], f.bit_depth))
            };
            out.extend_from_slice(&yuv.rgb(to8(yv, f.bit_depth), cb, cr));
        }
    }
    out
}

fn decode_item(ctx: &Context<'_>, id: u32) -> Result<Frame> {
    let p = he(ctx.props(id))?;
    let hvcc = p.hvcc.as_ref().context("HEIC tile has no hvcC")?;
    let data = he(ctx.item_data(id))?;
    let nals = he(hvcc.split_nals(&data))?;
    let params = hvcc.parameter_sets();
    if params.is_empty() {
        bail!("hvcC carries no parameter sets");
    }
    let frame = he(hevc::decode_still(&params, &nals))?;
    he(frame.validate())?;
    Ok(frame)
}

/// The EXIF-style orientation equal to the file's transforms, or `None` if they are more than a plain rotation.
fn orientation_of(transforms: &[Transform]) -> Option<Orientation> {
    match transforms {
        [] => Some(Orientation::NoTransforms),
        // `irot` counts anticlockwise, `Orientation` clockwise.
        [Transform::Rotate(r)] => Some(match r {
            Rotation::None => Orientation::NoTransforms,
            Rotation::Ccw90 => Orientation::Rotate270,
            Rotation::Ccw180 => Orientation::Rotate180,
            Rotation::Ccw270 => Orientation::Rotate90,
        }),
        _ => None,
    }
}

/// Decode a HEIC straight into a copy no bigger than the display needs (the upright size is what `fit_to_display`
/// would pick; a photo smaller than the display stays full size). `Ok(None)` means "use the whole-image decoder".
pub fn decode_fitted(bytes: &[u8], limits: &Limits) -> Result<Option<Photo>> {
    let ctx = he(Context::open(bytes))?;
    let id = ctx.meta.primary;
    let p = he(ctx.props(id))?;
    let Some(orientation) = orientation_of(&p.transforms) else { return Ok(None) };
    let Some(yuv) = Yuv::new(heic_rs::decode::effective_nclx(&p)) else { return Ok(None) };
    let (cw, ch) = he(ctx.coded_size(id, &p))?;
    if cw == 0 || ch == 0 || cw as u64 * ch as u64 > limits.max_pixels {
        bail!("{cw}x{ch} is over the {} pixel limit", limits.max_pixels);
    }

    let turned = matches!(orientation, Orientation::Rotate90 | Orientation::Rotate270);
    let (shown_w, shown_h) = if turned { (ch, cw) } else { (cw, ch) };
    let (fw, fh) = fit_size(shown_w, shown_h, DISPLAY_W as u32, DISPLAY_H as u32);
    let (tw, th) = if fw <= shown_w && fh <= shown_h {
        if turned { (fh, fw) } else { (fw, fh) }
    } else {
        (cw, ch)
    };
    let tw = tw.min(cw);
    let th = th.min(ch);
    let mut scaler = Scaler::new(cw, ch, tw, th);

    match he(ctx.grid(id))? {
        Some((grid, tiles)) => {
            let (mut tile_w, mut tile_h) = (0, 0);
            for (i, tile) in tiles.iter().enumerate() {
                let frame = decode_item(&ctx, *tile)?;
                if i == 0 {
                    (tile_w, tile_h) = (frame.width, frame.height);
                } else if (frame.width, frame.height) != (tile_w, tile_h) {
                    return Ok(None);
                }
                if i == 0 && ((tile_w as u64 * grid.columns as u64) < cw as u64 || (tile_h as u64 * grid.rows as u64) < ch as u64) {
                    return Ok(None); // the tiles do not cover the picture: let the strict decoder judge the file
                }
                let (col, row) = (i as u32 % grid.columns, i as u32 / grid.columns);
                let rgb = convert_rows(&frame, &yuv, 0, frame.height);
                scaler.add_block(col * tile_w, row * tile_h, frame.width, frame.height, &rgb);
            }
        }
        None => {
            let frame = decode_item(&ctx, id)?;
            if (frame.width, frame.height) != (cw, ch) {
                return Ok(None);
            }
            let mut y = 0;
            while y < ch {
                let rows = BAND_ROWS.min(ch - y);
                scaler.add_block(0, y, cw, rows, &convert_rows(&frame, &yuv, y, rows));
                y += rows;
            }
        }
    }
    Ok(Some(Photo { width: tw, height: th, rgb: scaler.finish(), orientation }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{decode_bytes, fit_to_display};

    fn nclx(matrix: MatrixCoefficients, range: Range) -> Nclx {
        Nclx { primaries: 1, transfer: 13, matrix, matrix_code: 1, range }
    }

    #[test]
    fn limited_range_black_and_white_map_to_the_extremes() {
        let y = Yuv::new(nclx(MatrixCoefficients::Bt709, Range::Limited)).unwrap();
        assert_eq!(y.rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(y.rgb(235, 128, 128), [255, 255, 255]);
        let full = Yuv::new(nclx(MatrixCoefficients::Bt709, Range::Full)).unwrap();
        assert_eq!(full.rgb(0, 128, 128), [0, 0, 0]);
        assert_eq!(full.rgb(255, 128, 128), [255, 255, 255]);
    }

    #[test]
    fn known_colours_in_bt709_and_bt601() {
        let y709 = Yuv::new(nclx(MatrixCoefficients::Bt709, Range::Limited)).unwrap();
        // Pure red in BT.709 limited range is about (63, 102, 240).
        let [r, g, b] = y709.rgb(63, 102, 240);
        assert!(r >= 253 && g <= 2 && b <= 2, "{r} {g} {b}");
        let y601 = Yuv::new(nclx(MatrixCoefficients::Bt601, Range::Limited)).unwrap();
        // Pure blue in BT.601 limited range is about (41, 240, 110).
        let [r, g, b] = y601.rgb(41, 240, 110);
        assert!(r <= 2 && g <= 2 && b >= 253, "{r} {g} {b}");
        assert!(Yuv::new(nclx(MatrixCoefficients::Identity, Range::Full)).is_none());
        assert!(Yuv::new(nclx(MatrixCoefficients::Other(99), Range::Full)).is_none());
    }

    #[test]
    fn a_plain_rotation_maps_to_the_matching_orientation() {
        assert_eq!(orientation_of(&[]), Some(Orientation::NoTransforms));
        assert_eq!(orientation_of(&[Transform::Rotate(Rotation::Ccw90)]), Some(Orientation::Rotate270));
        assert_eq!(orientation_of(&[Transform::Rotate(Rotation::Ccw270)]), Some(Orientation::Rotate90));
        assert_eq!(orientation_of(&[Transform::Mirror(heic_rs::Mirror::LeftRight)]), None);
    }

    #[test]
    fn eight_bit_conversion_of_deeper_samples() {
        assert_eq!(to8(255, 8), 255);
        assert_eq!(to8(1023, 10), 255);
        assert_eq!(to8(512, 10), 128);
        assert_eq!(to8(0, 12), 0);
    }

    /// The streaming decode must agree with the whole-image decoder on real photos (kept out of the repo, so the
    /// test only runs where `samples/` has HEIC files).
    #[test]
    fn matches_the_whole_image_decoder_on_real_photos() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut checked = 0;
        for e in rd.flatten() {
            let path = e.path();
            if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("heic")) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let limits = Limits::default();
            let streamed = decode_fitted(&bytes, &limits).unwrap().unwrap_or_else(|| panic!("{} fell back", path.display()));
            let whole = {
                let opts = heic_rs::DecodeOptions::default().with_layout(heic_rs::PixelLayout::Rgb8).with_max_pixels(Some(limits.max_pixels));
                let img = heic_rs::decode(&bytes, &opts).unwrap();
                Photo::upright(img.width, img.height, img.data)
            };
            let (a, b) = (fit_to_display(&streamed, [255; 3]), fit_to_display(&whole, [255; 3]));
            let diff: u64 = a.rgb.iter().zip(&b.rgb).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
            let mean = diff as f64 / a.rgb.len() as f64;
            let worst = a.rgb.iter().zip(&b.rgb).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
            assert!(mean < 1.5 && worst < 40, "{}: mean difference {mean:.2}, worst {worst}", path.display());
            // The ordinary entry point takes the streaming path too.
            let via_decode = decode_bytes(&bytes, &limits).unwrap();
            assert_eq!((via_decode.width, via_decode.height), (streamed.width, streamed.height));
            checked += 1;
        }
        eprintln!("compared {checked} HEIC photos");
    }
}
