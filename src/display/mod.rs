//! Where finished frames go: the real panel (Linux, `hardware` feature) or a PNG file for development.

use crate::overlay::{DISPLAY_H, DISPLAY_W};
use crate::render::to_preview_rgb;
use anyhow::{ensure, Context, Result};
use std::path::PathBuf;

#[cfg(all(feature = "hardware", target_os = "linux"))]
pub mod uc8159;

/// A display that takes one frame of palette indices (`DISPLAY_W` x `DISPLAY_H`, row by row).
pub trait Panel: Send {
    fn show(&mut self, frame: &[u8]) -> Result<()>;
}

/// Writes each frame as a PNG (in the panel's measured colours), so the whole program can run on a Mac.
pub struct Emulator {
    pub out: PathBuf,
}

impl Panel for Emulator {
    fn show(&mut self, frame: &[u8]) -> Result<()> {
        ensure!(frame.len() == DISPLAY_W * DISPLAY_H, "frame has the wrong size");
        let img = image::RgbImage::from_raw(DISPLAY_W as u32, DISPLAY_H as u32, to_preview_rgb(frame)).context("frame size")?;
        img.save(&self.out).with_context(|| format!("writing {}", self.out.display()))?;
        log::info!("emulator: wrote {}", self.out.display());
        Ok(())
    }
}

/// Seven vertical bars in the panel's colours, for checking wiring and colours.
pub fn colour_bars() -> Vec<u8> {
    let mut f = vec![0u8; DISPLAY_W * DISPLAY_H];
    for y in 0..DISPLAY_H {
        for x in 0..DISPLAY_W {
            let edge = x < 4 || y < 4 || x >= DISPLAY_W - 4 || y >= DISPLAY_H - 4;
            f[y * DISPLAY_W + x] = if edge { crate::palette::BLACK } else { ((x - 4) * 7 / (DISPLAY_W - 8)) as u8 };
        }
    }
    f
}

/// Two palette indices per byte, the first in the high nibble, as the UC8159 expects.
pub fn pack_nibbles(frame: &[u8]) -> Vec<u8> {
    frame.chunks(2).map(|p| ((p[0] & 0x0f) << 4) | (p.get(1).copied().unwrap_or(0) & 0x0f)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nibbles_pack_high_first() {
        assert_eq!(pack_nibbles(&[1, 2, 3, 4]), vec![0x12, 0x34]);
        assert_eq!(pack_nibbles(&[6, 0]), vec![0x60]);
        assert_eq!(pack_nibbles(&vec![1u8; DISPLAY_W * DISPLAY_H]).len(), 134_400);
    }

    #[test]
    fn colour_bars_use_all_seven_colours() {
        let f = colour_bars();
        for c in 0..7u8 {
            assert!(f.contains(&c), "colour {c} missing");
        }
    }

    #[test]
    fn emulator_writes_a_png() {
        let d = tempfile::tempdir().unwrap();
        let mut e = Emulator { out: d.path().join("f.png") };
        e.show(&colour_bars()).unwrap();
        assert_eq!(image::open(&e.out).unwrap().width(), DISPLAY_W as u32);
        assert!(e.show(&[0; 10]).is_err());
    }
}
