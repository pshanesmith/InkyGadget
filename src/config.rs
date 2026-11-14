//! Configuration (`/etc/inkyframe/config.toml`). Secrets (the Home Assistant token) are never in here: they
//! come from the environment (`INKYFRAME_HA_TOKEN`, set by `secrets.env`).

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub photos: Photos,
    pub display: Display,
    pub overlay: Overlay,
    pub location: Location,
    pub homeassistant: HomeAssistant,
    pub buttons: Buttons,
    pub sensor: Sensor,
    pub matter: Matter,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Photos {
    /// The folder shared over SMB.
    pub dir: PathBuf,
    /// Fitted copies of the photos and the rotation history.
    pub state_dir: PathBuf,
    /// Times of day (local, "HH:MM") at which a new photo is shown.
    pub refresh_times: Vec<String>,
    /// Also rotate every this many minutes (0 = only at `refresh_times`). Never below 5.
    pub interval_minutes: u32,
    /// Colour of the bars around a photo that does not fill the screen: "white" or "black".
    pub bar_colour: String,
    /// Photos with more pixels are skipped (a JPEG or PNG is decoded whole, 3 to 4 bytes a pixel; a HEIC streams).
    pub max_pixels: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Display {
    /// 0.0 to 1.0: how strongly the panel's measured colours are used when choosing colours.
    pub saturation: f32,
    /// Fastest the panel may be refreshed, in seconds (it flashes for about 30 s each time).
    pub min_refresh_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Overlay {
    pub enabled: bool,
    /// chrono format of the top-left text.
    pub date_format: String,
    /// "fahrenheit" or "celsius".
    pub units: String,
    /// Size of the overlay text: 11 px per step, so 2 is a 22 px font.
    pub scale: usize,
    /// Gap in pixels between the plates and the panel edge: 0 puts them right in the corners.
    pub margin: usize,
    /// "auto", "light" or "dark".
    pub plate: String,
    /// Bottom-left plate with the CO2, temperature and humidity of the room (needs `[sensor] enabled`).
    pub show_air: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Location {
    pub postal_code: String,
    pub latitude: f64,
    pub longitude: f64,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HomeAssistant {
    /// Overridden by `INKYFRAME_HA_URL`.
    pub url: String,
    pub timeout_seconds: u64,
}

/// Each button runs one thing: `scene.<name>` or `script.<name>` (called in Home Assistant), or the built-in
/// `next_photo`. An empty string disables the button.
/// The Sensirion SCD40 CO2, temperature and humidity sensor on the I2C bus (Adafruit SCD-40 breakout, address 0x62).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Sensor {
    pub enabled: bool,
    /// The sensor is on `/dev/i2c-N`; 1 is the header's SDA/SCL (GPIO2/GPIO3).
    pub i2c_bus: u8,
    /// One sample every 30 s instead of every 5 s, using less power.
    pub low_power: bool,
    /// Degrees Celsius to subtract for the heat of the board and the panel (0 to 175). The sensor's factory value is 4;
    /// tune it against a thermometer once the frame is in its case.
    pub temperature_offset_c: f32,
    /// Height above sea level in metres, to compensate CO2 for air pressure.
    pub altitude_m: u16,
    /// Ambient pressure in hPa (overrides `altitude_m`; 0 = not used).
    pub pressure_hpa: u16,
    /// Automatic self calibration: assumes the sensor sees fresh air (about 400 ppm) now and then. Switch it off for a
    /// room that is never aired.
    pub auto_calibration: bool,
    /// CO2 ppm at which the air quality drops from Good to Fair, Moderate, Poor, Very poor and Extremely poor.
    pub thresholds_ppm: [u16; 5],
}

/// The Matter node: the frame as an Air Quality Sensor for Home Assistant, Apple Home and the like. Needs `[sensor]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Matter {
    pub enabled: bool,
    /// The name controllers show for the device.
    pub device_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Buttons {
    pub a: String,
    pub b: String,
    pub c: String,
    pub d: String,
}

impl Default for Photos {
    fn default() -> Self {
        Photos {
            dir: "/srv/inkyframe/Images".into(),
            state_dir: "/var/lib/inkyframe".into(),
            refresh_times: vec!["00:00".into()],
            interval_minutes: 0,
            bar_colour: "white".into(),
            max_pixels: 25_000_000,
        }
    }
}
impl Default for Display {
    fn default() -> Self {
        Display { saturation: 0.5, min_refresh_seconds: 300 }
    }
}
impl Default for Overlay {
    fn default() -> Self {
        Overlay { enabled: true, date_format: "%A %B %-d".into(), units: "fahrenheit".into(), scale: 2, margin: 0, plate: "auto".into(), show_air: true }
    }
}
impl Default for Matter {
    fn default() -> Self {
        Matter { enabled: false, device_name: "InkyGadget".into() }
    }
}
impl Default for Sensor {
    fn default() -> Self {
        Sensor {
            enabled: false,
            i2c_bus: 1,
            low_power: false,
            temperature_offset_c: 4.0,
            altitude_m: 0,
            pressure_hpa: 0,
            auto_calibration: true,
            thresholds_ppm: [800, 1000, 1500, 2000, 5000],
        }
    }
}
impl Default for HomeAssistant {
    fn default() -> Self {
        HomeAssistant { url: "http://homeassistant:8123".into(), timeout_seconds: 10 }
    }
}
impl Default for Buttons {
    fn default() -> Self {
        Buttons { a: String::new(), b: String::new(), c: String::new(), d: "next_photo".into() }
    }
}

/// "HH:MM" to minutes since midnight.
pub fn parse_time(s: &str) -> Result<u32> {
    let (h, m) = s.split_once(':').with_context(|| format!("bad time {s:?}, expected HH:MM"))?;
    let (h, m): (u32, u32) = (h.trim().parse()?, m.trim().parse()?);
    if h > 23 || m > 59 {
        bail!("bad time {s:?}, expected HH:MM");
    }
    Ok(h * 60 + m)
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("{}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        for t in &self.photos.refresh_times {
            parse_time(t)?;
        }
        let p = &self.photos;
        if p.interval_minutes != 0 && p.interval_minutes < 5 {
            bail!("photos.interval_minutes must be 0 or at least 5 (the panel flashes for 30 s per refresh)");
        }
        if p.refresh_times.is_empty() && p.interval_minutes == 0 {
            bail!("set photos.refresh_times or photos.interval_minutes, or the picture would never change");
        }
        if !["white", "black"].contains(&p.bar_colour.as_str()) {
            bail!("photos.bar_colour must be \"white\" or \"black\"");
        }
        if !["fahrenheit", "celsius"].contains(&self.overlay.units.as_str()) {
            bail!("overlay.units must be \"fahrenheit\" or \"celsius\"");
        }
        if !["auto", "light", "dark"].contains(&self.overlay.plate.as_str()) {
            bail!("overlay.plate must be \"auto\", \"light\" or \"dark\"");
        }
        if self.overlay.margin > 40 {
            bail!("overlay.margin must be 0 to 40");
        }
        if !(1..=4).contains(&self.overlay.scale) {
            bail!("overlay.scale must be 1 to 4");
        }
        if chrono::format::StrftimeItems::new(&self.overlay.date_format).any(|i| matches!(i, chrono::format::Item::Error)) {
            bail!("overlay.date_format {:?} is not a valid strftime format (a lone % or an unknown %-code)", self.overlay.date_format);
        }
        if !(0.0..=1.0).contains(&self.display.saturation) {
            bail!("display.saturation must be between 0 and 1");
        }
        if self.display.min_refresh_seconds < 60 {
            bail!("display.min_refresh_seconds must be at least 60");
        }
        if self.matter.enabled && !self.sensor.enabled {
            bail!("[matter] needs [sensor] enabled = true: the Matter node serves the sensor's readings");
        }
        if self.matter.device_name.trim().is_empty() || self.matter.device_name.len() > 32 {
            bail!("matter.device_name must be 1 to 32 characters");
        }
        let sn = &self.sensor;
        if !(0.0..=175.0).contains(&sn.temperature_offset_c) {
            bail!("sensor.temperature_offset_c must be 0 to 175");
        }
        if sn.thresholds_ppm[0] == 0 || sn.thresholds_ppm.windows(2).any(|w| w[0] >= w[1]) {
            bail!("sensor.thresholds_ppm must be five increasing, positive numbers");
        }
        for (name, v) in [("a", &self.buttons.a), ("b", &self.buttons.b), ("c", &self.buttons.c), ("d", &self.buttons.d)] {
            if !v.is_empty() && v != "next_photo" && v != "matter_pairing" && !(v.starts_with("scene.") || v.starts_with("script.")) {
                bail!("buttons.{name} must be a scene.* or script.* entity, \"next_photo\", \"matter_pairing\", or empty");
            }
        }
        Ok(())
    }

    pub fn bar_rgb(&self) -> [u8; 3] {
        if self.photos.bar_colour == "black" { [0, 0, 0] } else { [255, 255, 255] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config/config.example.toml");
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        assert!(toml::from_str::<Config>("[photos]\nbogus = 1").is_err());
        let mut c = Config::default();
        c.photos.interval_minutes = 2;
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.photos.refresh_times = vec!["25:00".into()];
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.buttons.a = "light.turn_on".into();
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.photos.refresh_times.clear();
        assert!(c.validate().is_err());
    }

    #[test]
    fn a_bad_date_format_is_a_config_error_not_a_crash() {
        for bad in ["%Q", "%", "day %", "%-"] {
            let mut c = Config::default();
            c.overlay.date_format = bad.into();
            assert!(c.validate().is_err(), "{bad:?}");
        }
        for good in ["%A %B %-d", "%a %b %-d", "%d/%m", "plain text", ""] {
            let mut c = Config::default();
            c.overlay.date_format = good.into();
            c.validate().unwrap_or_else(|e| panic!("{good:?}: {e}"));
        }
    }

    #[test]
    fn sensor_settings_are_checked() {
        let mut c = Config::default();
        c.sensor.temperature_offset_c = 176.0;
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.sensor.temperature_offset_c = f32::NAN;
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.sensor.thresholds_ppm = [800, 800, 1500, 2000, 5000];
        assert!(c.validate().is_err());
        assert!(toml::from_str::<Config>("[sensor]\nenabeld = true").is_err());
        let c: Config = toml::from_str("[sensor]\nenabled = true\ni2c_bus = 3").unwrap();
        assert!(c.sensor.enabled && c.sensor.i2c_bus == 3 && c.sensor.auto_calibration);
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("00:00").unwrap(), 0);
        assert_eq!(parse_time("6:05").unwrap(), 365);
        assert!(parse_time("noon").is_err());
    }
}
