//! The four buttons on the side of the Inky Impression, A at the top.

use crate::app::Event;
use crate::config::Config;
use crate::ha;
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    A,
    B,
    C,
    D,
}

impl Button {
    pub fn from_char(c: char) -> Option<Button> {
        match c.to_ascii_lowercase() {
            'a' => Some(Button::A),
            'b' => Some(Button::B),
            'c' => Some(Button::C),
            'd' => Some(Button::D),
            _ => None,
        }
    }

    /// BCM GPIO numbers from Pimoroni's example, top to bottom.
    pub const PINS: [u32; 4] = [5, 6, 16, 24];

    pub fn from_pin(pin: u32) -> Option<Button> {
        [Button::A, Button::B, Button::C, Button::D].get(Self::PINS.iter().position(|&p| p == pin)?).copied()
    }
}

pub struct Actions {
    cfg: Config,
    url: String,
    token: Option<String>,
    tx: Sender<Event>,
}

impl Actions {
    pub fn new(cfg: &Config, tx: Sender<Event>) -> Self {
        let url = std::env::var(ha::URL_ENV).ok().filter(|u| !u.is_empty()).unwrap_or_else(|| cfg.homeassistant.url.clone());
        let token = std::env::var(ha::TOKEN_ENV).ok().filter(|t| !t.is_empty());
        Actions { cfg: cfg.clone(), url, token, tx }
    }

    /// What the button is set to do ("" for nothing).
    pub fn target(&self, b: Button) -> &str {
        match b {
            Button::A => &self.cfg.buttons.a,
            Button::B => &self.cfg.buttons.b,
            Button::C => &self.cfg.buttons.c,
            Button::D => &self.cfg.buttons.d,
        }
    }

    /// Handle one press. Home Assistant calls run on their own thread so a slow network never blocks the buttons
    /// or the display.
    pub fn press(&self, b: Button) {
        let target = self.target(b).to_string();
        log::info!("button {b:?} pressed ({})", if target.is_empty() { "unassigned" } else { &target });
        match target.as_str() {
            "" => {}
            "next_photo" => {
                let _ = self.tx.send(Event::Next);
            }
            entity => {
                let Some(token) = self.token.clone() else {
                    log::error!("{} is not set, so {entity} cannot be called", ha::TOKEN_ENV);
                    return;
                };
                let (url, entity, timeout) = (self.url.clone(), entity.to_string(), Duration::from_secs(self.cfg.homeassistant.timeout_seconds));
                std::thread::spawn(move || match ha::activate_with_retry(&url, &token, &entity, timeout) {
                    Ok(()) => log::info!("{entity}: done"),
                    Err(e) => log::error!("{entity}: {e:#}"),
                });
            }
        }
    }
}

/// Block forever reading the GPIO buttons.
#[cfg(all(feature = "hardware", target_os = "linux"))]
pub fn watch_gpio(actions: Actions) -> anyhow::Result<()> {
    use anyhow::Context;
    use gpiocdev::line::{Bias, EdgeDetection};
    let mut b = gpiocdev::Request::builder();
    b.on_chip(first_bcm_chip()).with_consumer("inkyframe-buttons");
    for pin in Button::PINS {
        b.with_line(pin).as_input().with_bias(Bias::PullUp).with_edge_detection(EdgeDetection::FallingEdge).with_debounce_period(Duration::from_millis(20));
    }
    let req = b.request().context("cannot claim the button GPIOs (is the user in the gpio group?)")?;
    loop {
        let ev = req.read_edge_event()?;
        if let Some(button) = Button::from_pin(ev.offset) {
            actions.press(button);
        }
    }
}

#[cfg(all(feature = "hardware", target_os = "linux"))]
fn first_bcm_chip() -> std::path::PathBuf {
    for p in gpiocdev::chip::chips().unwrap_or_default() {
        if let Ok(info) = gpiocdev::chip::Chip::from_path(&p).and_then(|c| c.info()) {
            if info.label.starts_with("pinctrl-bcm") || info.label.contains("rp1") {
                return p;
            }
        }
    }
    "/dev/gpiochip0".into()
}

/// Development stand-in for the buttons: type a, b, c or d and press Enter.
pub fn watch_stdin(actions: Actions) {
    use std::io::BufRead;
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        match line.trim().chars().next().and_then(Button::from_char) {
            Some(b) => actions.press(b),
            None if line.trim().is_empty() => {}
            None => eprintln!("type a, b, c or d"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn pins_map_top_to_bottom() {
        assert_eq!(Button::from_pin(5), Some(Button::A));
        assert_eq!(Button::from_pin(24), Some(Button::D));
        assert_eq!(Button::from_pin(7), None);
        assert_eq!(Button::from_char('C'), Some(Button::C));
    }

    #[test]
    fn next_photo_sends_an_event_and_blank_does_nothing() {
        let (tx, rx) = channel();
        let a = Actions::new(&Config::default(), tx);
        a.press(Button::A);
        assert!(rx.try_recv().is_err());
        a.press(Button::D);
        assert!(matches!(rx.try_recv(), Ok(Event::Next)));
    }
}
