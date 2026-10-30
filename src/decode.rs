//! Photo files to a picture fitted to the display: JPEG and PNG through the `image` crate, HEIC/HEIF through
//! `heic-rs` (pure Rust). The whole photo is kept and the leftover space is filled with the bar colour.

use crate::overlay::{DISPLAY_H, DISPLAY_W};
use crate::render::Prepared;
use crate::scale::{enlarge, fit_size, shrink};
use anyhow::{bail, Context, Result};
use image::metadata::Orientation;
use std::io::Cursor;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Photos with more pixels than this are refused before any pixel is decoded (memory on a 512 MB board).
    pub max_pixels: u64,
    pub max_file_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_pixels: 25_000_000, max_file_bytes: 100 * 1024 * 1024 }
    }
}

const EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "heic", "heif"];

/// Whether the file name looks like a photo we can read (the library ignores everything else, such as
/// the `.MOV` half of a Live Photo, hidden files and the temporary files Samba and macOS create).
pub fn is_photo_name(name: &str) -> bool {
    if name.starts_with('.') || name.starts_with('~') {
        return false;
    }
    match name.rsplit_once('.') {
        Some((_, ext)) => EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

fn is_heic(bytes: &[u8]) -> bool {
    bytes.len() > 12
        && &bytes[4..8] == b"ftyp"
        && [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1"].iter().any(|b| &bytes[8..12] == *b)
}

/// A decoded photo, RGB8. `width` and `height` are as stored; `orientation` (EXIF) says how to turn it upright, and
/// is applied after the photo has been shrunk, so a big rotated JPEG never needs a second full-size buffer.
pub struct Photo {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub orientation: Orientation,
}

impl Photo {
    pub fn upright(width: u32, height: u32, rgb: Vec<u8>) -> Self {
        Photo { width, height, rgb, orientation: Orientation::NoTransforms }
    }
}

fn swaps_axes(o: Orientation) -> bool {
    matches!(o, Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH)
}

pub fn decode_bytes(bytes: &[u8], limits: &Limits) -> Result<Photo> {
    if is_heic(bytes) {
        // The streaming path keeps memory to a few MB; it hands back `None` for files it does not handle exactly.
        match crate::heic_stream::decode_fitted(bytes, limits) {
            Ok(Some(photo)) => return Ok(photo),
            Ok(None) => log::info!("HEIC needs the whole-image decoder"),
            Err(e) => log::warn!("streaming HEIC decode failed ({e:#}); trying the whole-image decoder"),
        }
        let opts = heic_rs::DecodeOptions::default()
            .with_layout(heic_rs::PixelLayout::Rgb8)
            .with_max_pixels(Some(limits.max_pixels));
        let img = heic_rs::decode(bytes, &opts).map_err(|e| anyhow::anyhow!("HEIC decode failed: {e:?}"))?;
        return Ok(Photo::upright(img.width, img.height, img.data));
    }
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut decoder = reader.into_decoder().context("not a readable JPEG or PNG")?;
    let (w, h) = image::ImageDecoder::dimensions(&decoder);
    if w as u64 * h as u64 > limits.max_pixels {
        bail!("{w}x{h} is over the {} pixel limit", limits.max_pixels);
    }
    // Decoded size, not just pixel count: a 16-bit RGBA PNG needs eight bytes a pixel.
    let bytes_needed = image::ImageDecoder::total_bytes(&decoder);
    if bytes_needed > limits.max_pixels.saturating_mul(4) {
        bail!("{w}x{h} needs {bytes_needed} bytes to decode, over the limit");
    }
    let orientation = image::ImageDecoder::orientation(&mut decoder).unwrap_or(Orientation::NoTransforms);
    let rgb = image::DynamicImage::from_decoder(decoder)?.into_rgb8();
    Ok(Photo { width: rgb.width(), height: rgb.height(), rgb: rgb.into_raw(), orientation })
}

/// Fit the whole photo inside the display and surround it with `bars`.
pub fn fit_to_display(photo: &Photo, bars: [u8; 3]) -> Prepared {
    let turned = swaps_axes(photo.orientation);
    let (shown_w, shown_h) = if turned { (photo.height, photo.width) } else { (photo.width, photo.height) };
    let (fw, fh) = fit_size(shown_w, shown_h, DISPLAY_W as u32, DISPLAY_H as u32);
    // The small copy is made in the stored orientation, then turned upright.
    let (tw, th) = if turned { (fh, fw) } else { (fw, fh) };
    let small = if tw <= photo.width && th <= photo.height {
        shrink(&photo.rgb, photo.width, photo.height, tw, th)
    } else {
        enlarge(&photo.rgb, photo.width, photo.height, tw, th)
    };
    let small = if photo.orientation == Orientation::NoTransforms {
        small
    } else {
        let mut img = image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(tw, th, small).expect("size matches"));
        img.apply_orientation(photo.orientation);
        img.into_rgb8().into_raw()
    };
    let mut canvas = Prepared::solid(bars).rgb;
    let (ox, oy) = ((DISPLAY_W - fw as usize) / 2, (DISPLAY_H - fh as usize) / 2);
    for row in 0..fh as usize {
        let src = &small[row * fw as usize * 3..][..fw as usize * 3];
        let dst = ((oy + row) * DISPLAY_W + ox) * 3;
        canvas[dst..dst + src.len()].copy_from_slice(src);
    }
    Prepared { rgb: canvas }
}

pub fn prepare_file(path: &Path, bars: [u8; 3], limits: &Limits) -> Result<Prepared> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if meta.len() > limits.max_file_bytes {
        bail!("{} is {} bytes, over the limit", path.display(), meta.len());
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let photo = decode_bytes(&bytes, limits).with_context(|| format!("{}", path.display()))?;
    drop(bytes);
    Ok(fit_to_display(&photo, bars))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_names() {
        for ok in ["a.jpg", "A.JPEG", "IMG_0362.HEIC", "x.heif", "p.png"] {
            assert!(is_photo_name(ok), "{ok}");
        }
        for bad in ["IMG_0362.MOV", ".DS_Store", "._IMG.jpg", "~$a.jpg", "notes.txt", "noext", "Thumbs.db"] {
            assert!(!is_photo_name(bad), "{bad}");
        }
    }

    #[test]
    fn portrait_photo_gets_side_bars() {
        let photo = Photo::upright(300, 400, vec![0u8; 300 * 400 * 3]);
        let p = fit_to_display(&photo, [255, 255, 255]);
        let px = |x: usize, y: usize| p.rgb[(y * DISPLAY_W + x) * 3];
        assert_eq!(px(0, 200), 255, "left bar");
        assert_eq!(px(DISPLAY_W - 1, 200), 255, "right bar");
        assert_eq!(px(DISPLAY_W / 2, 200), 0, "photo in the middle");
    }

    #[test]
    fn small_photo_is_enlarged_not_left_tiny() {
        let photo = Photo::upright(60, 40, vec![0u8; 60 * 40 * 3]);
        let p = fit_to_display(&photo, [255, 255, 255]);
        assert_eq!(p.rgb[(DISPLAY_H / 2 * DISPLAY_W + 10) * 3], 0, "fills the width");
    }

    #[test]
    fn a_sideways_photo_is_turned_upright_after_shrinking() {
        // Stored 400 wide x 300 tall, but EXIF says rotate 90: it is a portrait, 300 wide x 400 tall.
        // Left half of the stored picture is black, right half white; turning it 90 degrees clockwise puts black on top.
        let mut rgb = vec![255u8; 400 * 300 * 3];
        for y in 0..300 {
            for x in 0..200 {
                rgb[(y * 400 + x) * 3..][..3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let photo = Photo { width: 400, height: 300, rgb, orientation: Orientation::Rotate90 };
        let p = fit_to_display(&photo, [128, 128, 128]);
        let px = |x: usize, y: usize| p.rgb[(y * DISPLAY_W + x) * 3];
        assert_eq!(px(DISPLAY_W / 2, 10), 0, "black half is on top");
        assert_eq!(px(DISPLAY_W / 2, DISPLAY_H - 10), 255, "white half is at the bottom");
        assert_eq!(px(5, 200), 128, "side bar: it is shown as a portrait");
    }

    #[test]
    fn exif_orientation_is_read_from_a_jpeg() {
        // A 4x2 JPEG carrying EXIF orientation 6 (rotate 90 clockwise).
        let mut jpeg = Vec::new();
        let img = image::RgbImage::from_pixel(4, 2, image::Rgb([200, 100, 50]));
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95);
        image::ImageEncoder::set_exif_metadata(&mut enc, exif_orientation(6)).unwrap();
        image::ImageEncoder::write_image(enc, img.as_raw(), 4, 2, image::ExtendedColorType::Rgb8).unwrap();
        let ph = decode_bytes(&jpeg, &Limits::default()).unwrap();
        assert_eq!((ph.width, ph.height), (4, 2), "stored size is kept");
        assert_eq!(ph.orientation, Orientation::Rotate90);
    }

    /// A minimal EXIF block (little endian TIFF) holding only the Orientation tag.
    fn exif_orientation(value: u8) -> Vec<u8> {
        vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, value, 0, 0, 0, 0, 0, 0, 0]
    }

    #[test]
    fn a_huge_decoded_size_is_refused_even_with_few_pixels() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(10, 10, image::Rgba([1u16, 2, 3, 4])))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        // 100 pixels is within a 100-pixel limit, but 800 bytes of decoded data is not within 400.
        assert!(decode_bytes(&png, &Limits { max_pixels: 100, ..Default::default() }).is_err());
        assert!(decode_bytes(&png, &Limits { max_pixels: 1000, ..Default::default() }).is_ok());
    }

    #[test]
    fn png_round_trip_and_limits() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 6, image::Rgb([10, 20, 30])))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let ph = decode_bytes(&png, &Limits::default()).unwrap();
        assert_eq!((ph.width, ph.height), (8, 6));
        assert_eq!(&ph.rgb[..3], &[10, 20, 30]);
        assert!(decode_bytes(&png, &Limits { max_pixels: 10, ..Default::default() }).is_err());
        assert!(decode_bytes(b"not an image", &Limits::default()).is_err());
    }
}
