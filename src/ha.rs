//! Calling Home Assistant scenes and scripts from the buttons (REST API, long-lived token).

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::time::Duration;

pub const TOKEN_ENV: &str = "INKYFRAME_HA_TOKEN";
pub const URL_ENV: &str = "INKYFRAME_HA_URL";

/// `scene.foo` -> ("scene", "turn_on"); `script.foo` -> ("script", "turn_on").
pub fn service_for(entity: &str) -> Result<(&str, &'static str)> {
    let (domain, name) = entity.split_once('.').context("entity must look like scene.name")?;
    let ok_name = !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    match domain {
        "scene" | "script" if ok_name => Ok((domain, "turn_on")),
        _ => bail!("{entity:?} is not a scene.* or script.* entity"),
    }
}

pub fn endpoint(base: &str, domain: &str, service: &str) -> String {
    format!("{}/api/services/{domain}/{service}", base.trim_end_matches('/'))
}

/// One attempt. `retry` is true only when the request never reached Home Assistant (no route, no connection), so
/// trying again cannot run a script twice.
struct Attempt {
    error: anyhow::Error,
    retry: bool,
}

fn try_activate(base_url: &str, token: &str, entity: &str, timeout: Duration) -> std::result::Result<(), Attempt> {
    let no_retry = |error: anyhow::Error| Attempt { error, retry: false };
    let (domain, service) = service_for(entity).map_err(no_retry)?;
    let agent = ureq::AgentBuilder::new().timeout(timeout).user_agent("InkyGadget/0.1").build();
    let r = agent
        .post(&endpoint(base_url, domain, service))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_string(&json!({ "entity_id": entity }).to_string());
    match r {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(401 | 403, _)) => Err(no_retry(anyhow::anyhow!("Home Assistant rejected the token"))),
        Err(ureq::Error::Status(404, _)) => Err(no_retry(anyhow::anyhow!("Home Assistant does not know {entity} (HTTP 404)"))),
        Err(ureq::Error::Status(code, _)) => Err(no_retry(anyhow::anyhow!("Home Assistant answered HTTP {code}"))),
        Err(ureq::Error::Transport(t)) => {
            let retry = matches!(t.kind(), ureq::ErrorKind::Dns | ureq::ErrorKind::ConnectionFailed);
            Err(Attempt { error: anyhow::anyhow!("Home Assistant unreachable ({:?})", t.kind()), retry })
        }
    }
}

/// Run the scene or script once. Errors never contain the token.
pub fn activate(base_url: &str, token: &str, entity: &str, timeout: Duration) -> Result<()> {
    try_activate(base_url, token, entity, timeout).map_err(|a| a.error)
}

/// Scenes and scripts known to Home Assistant, as (entity id, friendly name), sorted. Read-only (`GET /api/states`).
pub fn list_scenes_and_scripts(base_url: &str, token: &str, timeout: Duration) -> Result<Vec<(String, String)>> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).user_agent("InkyGadget/0.1").build();
    let r = agent.get(&format!("{}/api/states", base_url.trim_end_matches('/'))).set("Authorization", &format!("Bearer {token}")).call();
    let body: serde_json::Value = match r {
        Ok(r) => r.into_json().context("Home Assistant returned something that is not JSON")?,
        Err(ureq::Error::Status(401 | 403, _)) => bail!("Home Assistant rejected the token"),
        Err(ureq::Error::Status(code, _)) => bail!("Home Assistant answered HTTP {code}"),
        Err(ureq::Error::Transport(t)) => bail!("Home Assistant unreachable ({:?})", t.kind()),
    };
    Ok(scenes_from_states(&body))
}

pub fn scenes_from_states(states: &serde_json::Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s.get("entity_id")?.as_str()?;
            if !(id.starts_with("scene.") || id.starts_with("script.")) {
                return None;
            }
            let name = s.pointer("/attributes/friendly_name").and_then(|n| n.as_str()).unwrap_or("");
            Some((id.to_string(), name.to_string()))
        })
        .collect();
    out.sort();
    out
}

/// A second try when the first never reached Home Assistant (the Zero W's Wi-Fi may be asleep on the first packet
/// after a long idle). A timeout or an error answer is not retried: the script may already have run, and running a
/// toggle twice undoes it.
pub fn activate_with_retry(base_url: &str, token: &str, entity: &str, timeout: Duration) -> Result<()> {
    match try_activate(base_url, token, entity, timeout) {
        Ok(()) => Ok(()),
        Err(first) if first.retry => {
            log::warn!("{entity}: {:#}; retrying once", first.error);
            std::thread::sleep(Duration::from_secs(2));
            activate(base_url, token, entity, timeout)
        }
        Err(first) => Err(first.error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn only_scenes_and_scripts() {
        assert_eq!(service_for("scene.getting_ready_for_bed").unwrap(), ("scene", "turn_on"));
        assert_eq!(service_for("script.reading_time").unwrap(), ("script", "turn_on"));
        for bad in ["light.bedside", "lock.front", "scene.", "scene.Bad Name", "scene", "script.a/../b"] {
            assert!(service_for(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn lists_only_scenes_and_scripts() {
        let states = json!([
            {"entity_id": "light.bedside", "attributes": {"friendly_name": "Bedside"}},
            {"entity_id": "scene.bedtime", "attributes": {"friendly_name": "Getting ready for bed"}},
            {"entity_id": "script.reading_time", "attributes": {}},
            {"entity_id": "scene.a_scene", "attributes": {"friendly_name": "A"}}
        ]);
        let got = scenes_from_states(&states);
        assert_eq!(got.iter().map(|g| g.0.as_str()).collect::<Vec<_>>(), ["scene.a_scene", "scene.bedtime", "script.reading_time"]);
        assert_eq!(got[1].1, "Getting ready for bed");
        assert!(scenes_from_states(&json!({})).is_empty());
    }

    #[test]
    fn endpoint_has_no_double_slash() {
        assert_eq!(endpoint("http://ha:8123/", "scene", "turn_on"), "http://ha:8123/api/services/scene/turn_on");
    }

    fn serve_once(status: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            // Read the whole request (headers, then Content-Length bytes of body) before answering.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = s.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text.to_lowercase().split("content-length:").nth(1).and_then(|v| v.split("\r\n").next()?.trim().parse::<usize>().ok()).unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        (url, h)
    }

    #[test]
    fn posts_the_entity_with_the_token() {
        let (url, h) = serve_once("200 OK");
        activate(&url, "secret-token", "scene.bedtime", Duration::from_secs(5)).unwrap();
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/services/scene/turn_on "), "{req}");
        assert!(req.to_lowercase().contains("authorization: bearer secret-token"));
        assert!(req.contains(r#"{"entity_id":"scene.bedtime"}"#));
    }

    #[test]
    fn only_a_request_that_never_arrived_is_retried() {
        // Nothing is listening: the request cannot have reached Home Assistant, so a second try is safe.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        assert!(try_activate(&url, "t", "script.reading_time", Duration::from_secs(2)).unwrap_err().retry);
        // An error answer means it arrived: no second request, and the answer is what the caller sees.
        let (url, h) = serve_once("500 Internal Server Error");
        let e = activate_with_retry(&url, "t", "script.reading_time", Duration::from_secs(5)).unwrap_err();
        h.join().unwrap();
        assert!(format!("{e:#}").contains("HTTP 500"), "{e:#}");
        assert!(!try_activate("http://127.0.0.1:1", "t", "light.x", Duration::from_secs(1)).unwrap_err().retry, "a bad entity is never retried");
    }

    #[test]
    fn errors_are_readable_and_do_not_leak_the_token() {
        let (url, h) = serve_once("401 Unauthorized");
        let e = activate(&url, "secret-token", "scene.bedtime", Duration::from_secs(5)).unwrap_err();
        h.join().unwrap();
        assert!(format!("{e:#}").contains("rejected the token") && !format!("{e:#}").contains("secret-token"));
        let (url, h) = serve_once("404 Not Found");
        assert!(format!("{:#}", activate(&url, "t", "scene.nope", Duration::from_secs(5)).unwrap_err()).contains("scene.nope"));
        h.join().unwrap();
    }
}
