//! One-time ZIP code lookup (`inkyframe geocode 10001`), never used while running.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub label: String,
    pub latitude: f64,
    pub longitude: f64,
}

pub fn parse_zippopotam(data: &Value) -> Result<Place> {
    let p = data.get("places").and_then(|v| v.get(0)).context("no place for that ZIP code")?;
    let text = |k: &str| p.get(k).and_then(Value::as_str).with_context(|| format!("missing {k}"));
    Ok(Place {
        label: format!("{}, {}", text("place name")?, text("state abbreviation").unwrap_or("US")),
        latitude: text("latitude")?.parse()?,
        longitude: text("longitude")?.parse()?,
    })
}

pub fn lookup_us_zip(zip: &str) -> Result<Place> {
    if zip.len() != 5 || !zip.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{zip:?} is not a 5-digit US ZIP code");
    }
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).user_agent("InkyGadget/0.1 (one-time ZIP lookup)").build();
    let body: Value = agent.get(&format!("https://api.zippopotam.us/us/{zip}")).call().context("ZIP lookup failed")?.into_json()?;
    parse_zippopotam(&body)
}

/// Replace the `[location]` values in a config file's text, keeping everything else (and the comments).
pub fn set_location(text: &str, zip: &str, place: &Place) -> String {
    let mut out = Vec::new();
    let mut in_location = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_location = t.starts_with("[location]");
        }
        let key = t.split(['=', ' ']).next().unwrap_or("");
        let replaced = match (in_location, key) {
            (true, "postal_code") => Some(format!("postal_code = \"{zip}\"")),
            (true, "latitude") => Some(format!("latitude = {:.4}", place.latitude)),
            (true, "longitude") => Some(format!("longitude = {:.4}", place.longitude)),
            (true, "label") => Some(format!("label = \"{}\"", place.label.replace('"', ""))),
            _ => None,
        };
        out.push(replaced.unwrap_or_else(|| line.to_string()));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_zip_answer() {
        let d = json!({"post code": "10001", "places": [{"place name": "New York", "longitude": "-73.9967", "latitude": "40.7484", "state abbreviation": "NY"}]});
        let p = parse_zippopotam(&d).unwrap();
        assert_eq!(p, Place { label: "New York, NY".into(), latitude: 40.7484, longitude: -73.9967 });
        assert!(parse_zippopotam(&json!({})).is_err());
    }

    #[test]
    fn rejects_bad_zips_without_a_request() {
        assert!(lookup_us_zip("1234").is_err());
        assert!(lookup_us_zip("abcde").is_err());
    }

    #[test]
    fn set_location_only_touches_the_location_section() {
        let cfg = "[overlay]\nunits = \"celsius\"\n\n[location]\n# comment\npostal_code = \"\"\nlatitude = 0.0\nlongitude = 0.0\nlabel = \"\"\n\n[buttons]\na = \"\"\n";
        let p = Place { label: "New York, NY".into(), latitude: 40.7484, longitude: -73.9967 };
        let out = set_location(cfg, "10001", &p);
        assert!(out.contains("postal_code = \"10001\"") && out.contains("latitude = 40.7484") && out.contains("longitude = -73.9967"));
        assert!(out.contains("label = \"New York, NY\"") && out.contains("units = \"celsius\"") && out.contains("# comment") && out.contains("a = \"\""));
        let cfg: crate::config::Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.location.postal_code, "10001");
        assert_eq!(cfg.location.latitude, 40.7484);
    }
}
