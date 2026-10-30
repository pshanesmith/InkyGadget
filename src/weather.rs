//! Today's high and low from Open-Meteo (no key). Only the daily values are used.

use crate::config::Location;
use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    /// The day the numbers are for, in the forecast location's time zone.
    pub date: NaiveDate,
    pub high: f64,
    pub low: f64,
    pub fahrenheit: bool,
}

pub fn build_url(loc: &Location, fahrenheit: bool) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&daily=temperature_2m_max,temperature_2m_min\
         &temperature_unit={}&timezone=auto&forecast_days=1",
        loc.latitude,
        loc.longitude,
        if fahrenheit { "fahrenheit" } else { "celsius" }
    )
}

pub fn parse(data: &Value, fahrenheit: bool) -> Result<Forecast> {
    let daily = data.get("daily").context("no \"daily\" in the forecast")?;
    let first = |key: &str| daily.get(key).and_then(|v| v.get(0)).with_context(|| format!("no {key} in the forecast"));
    let date = first("time")?.as_str().context("bad date")?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").context("bad date")?;
    let num = |key: &str| first(key)?.as_f64().with_context(|| format!("{key} is not a number"));
    let (high, low) = (num("temperature_2m_max")?, num("temperature_2m_min")?);
    let range = if fahrenheit { -100.0..=150.0 } else { -75.0..=65.0 };
    if !range.contains(&high) || !range.contains(&low) || low > high + 0.5 {
        bail!("implausible temperatures {high}/{low}");
    }
    Ok(Forecast { date, high, low, fahrenheit })
}

pub fn fetch(loc: &Location, fahrenheit: bool, timeout: Duration) -> Result<Forecast> {
    if loc.latitude == 0.0 && loc.longitude == 0.0 {
        bail!("no location configured (run: inkyframe geocode ZIP)");
    }
    let agent = ureq::AgentBuilder::new().timeout(timeout).user_agent("InkyGadget/0.1").build();
    let body: Value = agent.get(&build_url(loc, fahrenheit)).call().context("weather request failed")?.into_json()?;
    parse(&body, fahrenheit)
}

/// "H 62° L 41°"
pub fn overlay_text(f: &Forecast) -> String {
    format!("H {}° L {}°", f.high.round() as i64, f.low.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_open_meteo_daily() {
        let data = json!({"daily": {"time": ["2026-10-29"], "temperature_2m_max": [61.6], "temperature_2m_min": [40.5]}});
        let f = parse(&data, true).unwrap();
        assert_eq!(f.date, NaiveDate::from_ymd_opt(2026, 10, 29).unwrap());
        assert_eq!(overlay_text(&f), "H 62° L 41°");
    }

    #[test]
    fn negative_temperatures() {
        let data = json!({"daily": {"time": ["2026-01-05"], "temperature_2m_max": [-3.4], "temperature_2m_min": [-12.6]}});
        assert_eq!(overlay_text(&parse(&data, false).unwrap()), "H -3° L -13°");
    }

    #[test]
    fn plausibility_depends_on_the_unit() {
        let d = |hi: f64, lo: f64| json!({"daily": {"time": ["2026-07-01"], "temperature_2m_max": [hi], "temperature_2m_min": [lo]}});
        assert!(parse(&d(73.1, 44.2), true).is_ok());
        assert!(parse(&d(73.1, 44.2), false).is_err(), "73 C is not a weather forecast");
        assert!(parse(&d(20.0, 30.0), false).is_err(), "low above high");
    }

    #[test]
    fn bad_payloads_are_errors() {
        assert!(parse(&json!({}), true).is_err());
        assert!(parse(&json!({"daily": {"time": [], "temperature_2m_max": [], "temperature_2m_min": []}}), true).is_err());
        assert!(parse(&json!({"daily": {"time": ["x"], "temperature_2m_max": [1], "temperature_2m_min": [1]}}), true).is_err());
        assert!(parse(&json!({"daily": {"time": ["2026-10-29"], "temperature_2m_max": ["hot"], "temperature_2m_min": [1]}}), true).is_err());
    }

    #[test]
    fn url_has_units_and_location() {
        let loc = Location { latitude: 40.7484, longitude: -73.9967, ..Default::default() };
        let u = build_url(&loc, true);
        assert!(u.contains("latitude=40.7484") && u.contains("longitude=-73.9967") && u.contains("fahrenheit"));
        assert!(build_url(&loc, false).contains("celsius"));
    }
}
