//! Driver for the Inky Impression 5.7" (UC8159, 600x448, 7 colours). The command sequence and pins are those of
//! Pimoroni's Python driver (MIT, see NOTICE); only the plumbing is new (spidev + the GPIO character device).

use super::{pack_nibbles, Panel};
use crate::overlay::{DISPLAY_H, DISPLAY_W};
use anyhow::{ensure, Context, Result};
use gpiocdev::line::{Bias, EdgeDetection, Value};
use gpiocdev::Request;
use spidev::{SpiModeFlags, Spidev, SpidevOptions};
use std::io::Write;
use std::time::{Duration, Instant};

// BCM pin numbers.
const CS: u32 = 8;
const DC: u32 = 22;
const RESET: u32 = 27;
const BUSY: u32 = 17;

const PSR: u8 = 0x00;
const PWR: u8 = 0x01;
const POF: u8 = 0x02;
const PFS: u8 = 0x03;
const PON: u8 = 0x04;
const DTM1: u8 = 0x10;
const DRF: u8 = 0x12;
const PLL: u8 = 0x30;
const TSE: u8 = 0x41;
const CDI: u8 = 0x50;
const TCON: u8 = 0x60;
const TRES: u8 = 0x61;
const DAM: u8 = 0x65;
const PWS: u8 = 0xE3;

const BORDER_WHITE: u8 = 1;
/// Debugging knobs (panel artefacts): INKYFRAME_SPI_HZ (default 3 MHz) and INKYFRAME_SPI_CHUNK (bytes per write, default 4096).
fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).filter(|&v| v > 0).unwrap_or(default)
}

pub struct Uc8159 {
    spi: Spidev,
    gpio: Request,
    chunk: usize,
}

/// The GPIO chip that carries the Pi's header pins (`pinctrl-bcm*`); `/dev/gpiochip0` on a Zero W.
fn find_chip() -> std::path::PathBuf {
    for p in gpiocdev::chip::chips().unwrap_or_default() {
        if let Ok(info) = gpiocdev::chip::Chip::from_path(&p).and_then(|c| c.info()) {
            if info.label.starts_with("pinctrl-bcm") || info.label.contains("rp1") {
                return p;
            }
        }
    }
    "/dev/gpiochip0".into()
}

impl Uc8159 {
    pub fn open() -> Result<Self> {
        let chip = find_chip();
        let gpio = Request::builder()
            .on_chip(&chip)
            .with_consumer("inkyframe")
            .with_line(CS)
            .as_output(Value::Active)
            .with_bias(Bias::Disabled)
            .with_line(DC)
            .as_output(Value::Inactive)
            .with_bias(Bias::Disabled)
            .with_line(RESET)
            .as_output(Value::Active)
            .with_bias(Bias::Disabled)
            .with_line(BUSY)
            .as_input()
            .with_bias(Bias::Disabled)
            .with_edge_detection(EdgeDetection::RisingEdge)
            .request()
            .with_context(|| {
                format!("cannot claim GPIO {CS}/{DC}/{RESET}/{BUSY} on {} (is dtoverlay=spi0-0cs set, and is the user in the gpio group?)", chip.display())
            })?;
        let mut spi = Spidev::open("/dev/spidev0.0").context("cannot open /dev/spidev0.0 (is SPI enabled, and is the user in the spi group?)")?;
        let hz = env_num("INKYFRAME_SPI_HZ", 3_000_000) as u32;
        let mut opts = SpidevOptions::new();
        opts.bits_per_word(8).max_speed_hz(hz).mode(SpiModeFlags::SPI_MODE_0 | SpiModeFlags::SPI_NO_CS);
        if spi.configure(&opts.build()).is_err() {
            // Some kernels refuse "no chip select"; the chip select line is driven by hand either way.
            let mut plain = SpidevOptions::new();
            plain.bits_per_word(8).max_speed_hz(hz).mode(SpiModeFlags::SPI_MODE_0);
            spi.configure(&plain.build()).context("cannot configure SPI")?;
        }
        let chunk = env_num("INKYFRAME_SPI_CHUNK", 4096) as usize;
        log::info!("SPI {hz} Hz, {chunk} bytes per write");
        let mut panel = Uc8159 { spi, gpio, chunk };
        // A previous run may have been killed between power-on and power-off, leaving the panel's booster running:
        // a hardware reset returns the controller to its idle state (every refresh begins with one anyway).
        panel.reset()?;
        Ok(panel)
    }

    fn reset(&mut self) -> Result<()> {
        self.gpio.set_value(RESET, Value::Inactive)?;
        std::thread::sleep(Duration::from_millis(100));
        self.gpio.set_value(RESET, Value::Active)?;
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    fn write(&mut self, data_mode: bool, bytes: &[u8]) -> Result<()> {
        self.gpio.set_value(CS, Value::Inactive)?;
        self.gpio.set_value(DC, if data_mode { Value::Active } else { Value::Inactive })?;
        for chunk in bytes.chunks(self.chunk) {
            self.spi.write_all(chunk)?;
        }
        self.gpio.set_value(CS, Value::Active)?;
        Ok(())
    }

    fn command(&mut self, cmd: u8, data: &[u8]) -> Result<()> {
        self.write(false, &[cmd])?;
        if !data.is_empty() {
            self.write(true, data)?;
        }
        Ok(())
    }

    /// The panel pulls BUSY low while it works and releases it when done. Like Pimoroni's driver, a line that is
    /// still high right after a command is treated as "no signal" and waited out in full.
    fn busy_wait(&mut self, timeout: Duration) -> Result<()> {
        if self.gpio.value(BUSY)? == Value::Active {
            // Normal right after a reset (Pimoroni's driver warns here too); a real problem only if it happens after DRF.
            log::debug!("busy line held high; waiting {timeout:.1?}");
            std::thread::sleep(timeout);
            return Ok(());
        }
        let start = Instant::now();
        loop {
            let left = timeout.saturating_sub(start.elapsed());
            if left.is_zero() {
                log::warn!("busy wait timed out after {timeout:.1?}");
                return Ok(());
            }
            if self.gpio.wait_edge_event(left)? {
                self.gpio.read_edge_event()?;
                return Ok(());
            }
        }
    }

    fn drain_edges(&self) {
        while self.gpio.has_edge_event().unwrap_or(false) {
            let _ = self.gpio.read_edge_event();
        }
    }

    fn setup(&mut self) -> Result<()> {
        self.reset()?;
        self.busy_wait(Duration::from_secs(1))?;

        let (w, h) = (DISPLAY_W as u16, DISPLAY_H as u16);
        self.command(TRES, &[(w >> 8) as u8, w as u8, (h >> 8) as u8, h as u8])?;
        // Resolution 600x448 (0b11), LUT from registers/flash as the vendor driver sets, 7 colours.
        self.command(PSR, &[(0b11 << 6) | 0b101111, 0x08])?;
        self.command(PWR, &[(0x06 << 3) | (0x01 << 2) | (0x01 << 1) | 0x01, 0x00, 0x23, 0x23])?;
        self.command(PLL, &[0x3C])?;
        self.command(TSE, &[0x00])?;
        self.command(CDI, &[(BORDER_WHITE << 5) | 0x17])?;
        self.command(TCON, &[0x22])?;
        self.command(DAM, &[0x00])?;
        self.command(PWS, &[0xAA])?;
        self.command(PFS, &[0x00])?;
        Ok(())
    }
}

impl Panel for Uc8159 {
    fn show(&mut self, frame: &[u8]) -> Result<()> {
        ensure!(frame.len() == DISPLAY_W * DISPLAY_H, "frame has the wrong size");
        let started = Instant::now();
        self.setup()?;
        self.command(DTM1, &pack_nibbles(frame))?;
        self.drain_edges();
        self.command(PON, &[])?;
        self.busy_wait(Duration::from_millis(200))?;
        self.drain_edges();
        self.command(DRF, &[])?;
        self.busy_wait(Duration::from_secs(32))?;
        self.drain_edges();
        self.command(POF, &[])?;
        self.busy_wait(Duration::from_millis(200))?;
        log::info!("panel refreshed in {:.1?}", started.elapsed());
        Ok(())
    }
}
