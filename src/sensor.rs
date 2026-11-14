//! The room's CO2, temperature and humidity from a Sensirion SCD40 on the I2C bus.
//!
//! One background thread owns the sensor and keeps the newest reading in a [`Latest`]; everything else (the overlay
//! plate now, the Matter node later) only reads it. A sensor that is missing or stops answering never stops the
//! frame: the thread logs, waits and opens it again, and a reading that is too old is treated as no reading.

use crate::config;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub co2_ppm: u16,
    pub temperature_c: f32,
    pub humidity_pct: f32,
}

/// Where readings come from: the real sensor, or a made-up one for development.
pub trait Source: Send {
    /// `Ok(None)` means no new sample yet.
    fn poll(&mut self) -> Result<Option<Reading>>;
}

/// The newest reading and when it arrived. Cloning shares it.
#[derive(Clone, Default)]
pub struct Latest(Arc<Mutex<Option<(Instant, Reading)>>>);

impl Latest {
    pub fn set(&self, r: Reading) {
        *self.lock() = Some((Instant::now(), r));
    }

    pub fn clear(&self) {
        *self.lock() = None;
    }

    /// The newest reading if it is no older than `max_age`.
    pub fn get(&self, max_age: Duration) -> Option<Reading> {
        self.lock().filter(|(at, _)| at.elapsed() <= max_age).map(|(_, r)| r)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(Instant, Reading)>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// How long a reading stays valid: three missed samples.
pub fn max_age(cfg: &config::Sensor) -> Duration {
    Duration::from_secs(if cfg.low_power { 90 + 10 } else { 15 + 5 })
}

/// Matter's six air quality levels (Air Quality cluster), from CO2 alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirQuality {
    /// No fresh reading.
    Unknown,
    Good,
    Fair,
    Moderate,
    Poor,
    VeryPoor,
    ExtremelyPoor,
}

impl AirQuality {
    /// `thresholds` are the CO2 levels (ppm) at which the level worsens, in increasing order.
    pub fn from_co2(ppm: u16, thresholds: &[u16; 5]) -> Self {
        const LEVELS: [AirQuality; 6] =
            [AirQuality::Good, AirQuality::Fair, AirQuality::Moderate, AirQuality::Poor, AirQuality::VeryPoor, AirQuality::ExtremelyPoor];
        LEVELS[thresholds.iter().filter(|&&t| ppm >= t).count()]
    }

    pub fn name(self) -> &'static str {
        match self {
            AirQuality::Unknown => "Unknown",
            AirQuality::Good => "Good",
            AirQuality::Fair => "Fair",
            AirQuality::Moderate => "Moderate",
            AirQuality::Poor => "Poor",
            AirQuality::VeryPoor => "Very poor",
            AirQuality::ExtremelyPoor => "Extremely poor",
        }
    }
}

/// Text for the bottom-left plate, e.g. `CO2 612 ppm  72°  41%`.
pub fn overlay_text(r: &Reading, fahrenheit: bool) -> String {
    let t = if fahrenheit { r.temperature_c * 9.0 / 5.0 + 32.0 } else { r.temperature_c };
    format!("CO2 {} ppm  {}°  {}%", r.co2_ppm, t.round() as i32, r.humidity_pct.round() as i32)
}

/// Pause between looks at the sensor (a new sample is ready every 5 s, or 30 s in low-power mode).
const POLL: Duration = Duration::from_secs(2);
/// Wait before opening a sensor that could not be opened or stopped answering.
const RETRY: Duration = Duration::from_secs(60);
/// Failed polls in a row before the sensor is closed and opened again.
const ERRORS_BEFORE_REOPEN: u32 = 5;

pub struct Timing {
    pub poll: Duration,
    pub retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { poll: POLL, retry: RETRY }
    }
}

fn nap(d: Duration, stop: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(Duration::from_millis(200)));
    }
}

/// Open the sensor, poll it, and open it again when it fails; until `stop` is set.
pub fn run_loop(mut open: impl FnMut() -> Result<Box<dyn Source>>, latest: &Latest, t: &Timing, stop: &AtomicBool) {
    let mut announced = false;
    while !stop.load(Ordering::Relaxed) {
        let mut source = match open() {
            Ok(s) => {
                log::info!("sensor ready");
                announced = false;
                s
            }
            Err(e) => {
                // One line per outage, not one every minute.
                if !announced {
                    log::warn!("sensor not available, will keep trying: {e:#}");
                    announced = true;
                }
                if !nap(t.retry, stop) {
                    return;
                }
                continue;
            }
        };
        let mut errors = 0;
        loop {
            match source.poll() {
                Ok(Some(r)) => {
                    latest.set(r);
                    errors = 0;
                }
                Ok(None) => {}
                Err(e) => {
                    errors += 1;
                    log::warn!("sensor read failed ({errors} in a row): {e:#}");
                    if errors >= ERRORS_BEFORE_REOPEN {
                        latest.clear();
                        break;
                    }
                }
            }
            if !nap(t.poll, stop) {
                return;
            }
        }
        if !nap(t.retry, stop) {
            return;
        }
    }
}

/// Start the sensor thread. With `emulate` (development) the readings are made up. A build without the hardware
/// driver and no emulation has no way to reach the sensor, which is logged once.
pub fn start(cfg: &config::Sensor, latest: Latest, stop: Arc<AtomicBool>, emulate: bool) {
    if !cfg.enabled {
        return;
    }
    let open: Box<dyn FnMut() -> Result<Box<dyn Source>> + Send> = if emulate {
        Box::new(|| Ok(Box::new(Emulated::new()) as Box<dyn Source>))
    } else if cfg!(all(feature = "hardware", target_os = "linux")) {
        let cfg = cfg.clone();
        Box::new(move || open_hardware(&cfg))
    } else {
        log::error!("[sensor] is enabled, but this build has no I2C driver; use --emulate, or build with --features hardware");
        return;
    };
    std::thread::spawn(move || {
        let mut open = open;
        run_loop(&mut *open, &latest, &Timing::default(), &stop);
    });
}

/// A slowly changing made-up room, so the plate and the rest can be developed on a Mac.
pub struct Emulated {
    start: Instant,
}

impl Emulated {
    pub fn new() -> Self {
        Emulated { start: Instant::now() }
    }
}

impl Default for Emulated {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Emulated {
    fn poll(&mut self) -> Result<Option<Reading>> {
        let t = self.start.elapsed().as_secs_f32();
        Ok(Some(Reading {
            co2_ppm: (950.0 + 500.0 * (t / 90.0).sin()) as u16,
            temperature_c: 22.0 + 1.5 * (t / 300.0).sin(),
            humidity_pct: 42.0 + 6.0 * (t / 200.0).cos(),
        }))
    }
}

#[cfg(all(feature = "hardware", target_os = "linux"))]
fn open_hardware(cfg: &config::Sensor) -> Result<Box<dyn Source>> {
    use anyhow::Context;
    use linux_embedded_hal::{Delay, I2cdev};
    use sensirion_scd4x::Scd4x;

    struct Scd40(Scd4x<I2cdev, Delay>);

    impl Source for Scd40 {
        fn poll(&mut self) -> Result<Option<Reading>> {
            let m = self.0.measurement().map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(m.map(|m| Reading { co2_ppm: m.co2_ppm, temperature_c: m.temperature_c, humidity_pct: m.humidity_pct }))
        }
    }

    let path = format!("/dev/i2c-{}", cfg.i2c_bus);
    let i2c = I2cdev::new(&path).with_context(|| format!("cannot open {path} (is I2C enabled, and is the service in the i2c group?)"))?;
    let mut dev = Scd4x::new(i2c, Delay);
    let e = |e: sensirion_scd4x::Error<_>| anyhow::anyhow!("{e}");
    // The sensor keeps measuring while this program restarts; init stops that so the settings below are accepted.
    dev.init().map_err(e).context("no answer from the sensor at 0x62")?;
    // Applied at every start and never stored in the sensor's EEPROM, which wears out after about 2000 writes.
    dev.set_temperature_offset(cfg.temperature_offset_c).map_err(e)?;
    dev.set_altitude(cfg.altitude_m).map_err(e)?;
    dev.set_automatic_self_calibration(cfg.auto_calibration).map_err(e)?;
    if cfg.pressure_hpa > 0 {
        dev.set_ambient_pressure(cfg.pressure_hpa).map_err(e)?;
    }
    if cfg.low_power {
        dev.start_low_power_periodic_measurement().map_err(e)?;
    } else {
        dev.start_periodic_measurement().map_err(e)?;
    }
    Ok(Box::new(Scd40(dev)))
}

#[cfg(not(all(feature = "hardware", target_os = "linux")))]
fn open_hardware(_cfg: &config::Sensor) -> Result<Box<dyn Source>> {
    anyhow::bail!("no I2C driver in this build")
}

/// `inkyframe hwtest --sensor`: print readings for about a minute.
pub fn test(cfg: &config::Sensor) -> Result<()> {
    let mut source = open_hardware(cfg)?;
    println!("sensor opened on /dev/i2c-{}; the first sample takes about 5 s (Ctrl-C to stop)", cfg.i2c_bus);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        if let Some(r) = source.poll()? {
            println!(
                "CO2 {} ppm ({}), {:.1} C / {:.1} F, humidity {:.0} %",
                r.co2_ppm,
                AirQuality::from_co2(r.co2_ppm, &cfg.thresholds_ppm).name(),
                r.temperature_c,
                r.temperature_c * 9.0 / 5.0 + 32.0,
                r.humidity_pct
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn reading(co2: u16) -> Reading {
        Reading { co2_ppm: co2, temperature_c: 22.0, humidity_pct: 40.0 }
    }

    #[test]
    fn air_quality_follows_the_thresholds() {
        let t = [800, 1000, 1500, 2000, 5000];
        for (ppm, want) in [
            (400, AirQuality::Good),
            (799, AirQuality::Good),
            (800, AirQuality::Fair),
            (1200, AirQuality::Moderate),
            (1500, AirQuality::Poor),
            (2500, AirQuality::VeryPoor),
            (5000, AirQuality::ExtremelyPoor),
            (u16::MAX, AirQuality::ExtremelyPoor),
        ] {
            assert_eq!(AirQuality::from_co2(ppm, &t), want, "{ppm}");
        }
    }

    #[test]
    fn plate_text_in_both_units() {
        let r = Reading { co2_ppm: 612, temperature_c: 22.2, humidity_pct: 41.4 };
        assert_eq!(overlay_text(&r, true), "CO2 612 ppm  72°  41%");
        assert_eq!(overlay_text(&r, false), "CO2 612 ppm  22°  41%");
        let cold = Reading { co2_ppm: 450, temperature_c: -3.6, humidity_pct: 80.0 };
        assert_eq!(overlay_text(&cold, false), "CO2 450 ppm  -4°  80%");
    }

    #[test]
    fn the_plate_fits_next_to_nothing_else() {
        let (w, _) = crate::overlay::plate_size("CO2 10000 ppm  -100°  100%", 2);
        assert!(w < crate::overlay::DISPLAY_W / 2 + 80, "{w}");
    }

    #[test]
    fn a_reading_goes_stale() {
        let l = Latest::default();
        assert_eq!(l.get(Duration::from_secs(60)), None);
        l.set(reading(700));
        assert_eq!(l.get(Duration::from_secs(60)), Some(reading(700)));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(l.get(Duration::from_millis(5)), None);
        l.clear();
        assert_eq!(l.get(Duration::from_secs(60)), None);
    }

    /// Plays back a script of poll results, then asks the loop to stop.
    struct Scripted {
        script: VecDeque<Result<Option<Reading>>>,
        stop: Arc<AtomicBool>,
    }

    impl Source for Scripted {
        fn poll(&mut self) -> Result<Option<Reading>> {
            let next = self.script.pop_front().unwrap_or_else(|| Ok(None));
            if self.script.is_empty() {
                self.stop.store(true, Ordering::Relaxed);
            }
            next
        }
    }

    const FAST: Timing = Timing { poll: Duration::ZERO, retry: Duration::ZERO };

    #[test]
    fn readings_are_published_and_failures_clear_them_after_a_while() {
        let (latest, stop) = (Latest::default(), Arc::new(AtomicBool::new(false)));
        let mut script: VecDeque<_> = VecDeque::from([Ok(Some(reading(640))), Ok(None)]);
        for _ in 0..ERRORS_BEFORE_REOPEN {
            script.push_back(Err(anyhow::anyhow!("nack")));
        }
        let s = stop.clone();
        let mut sources = vec![Scripted { script, stop: s }];
        run_loop(|| Ok(Box::new(sources.pop().unwrap()) as Box<dyn Source>), &latest, &FAST, &stop);
        assert_eq!(latest.get(Duration::from_secs(60)), None, "cleared once the sensor was given up on");

        let (latest, stop) = (Latest::default(), Arc::new(AtomicBool::new(false)));
        let mut sources = vec![Scripted { script: VecDeque::from([Ok(Some(reading(640)))]), stop: stop.clone() }];
        run_loop(|| Ok(Box::new(sources.pop().unwrap()) as Box<dyn Source>), &latest, &FAST, &stop);
        assert_eq!(latest.get(Duration::from_secs(60)), Some(reading(640)));
    }

    #[test]
    fn a_few_errors_do_not_drop_the_reading() {
        let (latest, stop) = (Latest::default(), Arc::new(AtomicBool::new(false)));
        let script = VecDeque::from([Ok(Some(reading(500))), Err(anyhow::anyhow!("crc")), Ok(None)]);
        let mut sources = vec![Scripted { script, stop: stop.clone() }];
        run_loop(|| Ok(Box::new(sources.pop().unwrap()) as Box<dyn Source>), &latest, &FAST, &stop);
        assert_eq!(latest.get(Duration::from_secs(60)), Some(reading(500)));
    }

    #[test]
    fn a_sensor_that_cannot_be_opened_is_retried() {
        let stop = Arc::new(AtomicBool::new(false));
        let (mut tries, s) = (0, stop.clone());
        run_loop(
            || {
                tries += 1;
                if tries == 3 {
                    s.store(true, Ordering::Relaxed);
                }
                anyhow::bail!("no such device")
            },
            &Latest::default(),
            &FAST,
            &stop,
        );
        assert_eq!(tries, 3);
    }

    #[test]
    fn the_emulated_room_is_plausible() {
        let r = Emulated::new().poll().unwrap().unwrap();
        assert!((400..=1500).contains(&r.co2_ppm) && (15.0..30.0).contains(&r.temperature_c) && (20.0..70.0).contains(&r.humidity_pct));
    }
}
