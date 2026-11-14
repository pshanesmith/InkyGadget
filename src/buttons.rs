//! The four buttons on the side of the Inky Impression, A at the top.

use crate::app::Event;
use crate::config::Config;
use crate::ha;
use crate::switchlogic::{SwitchEvent, SwitchLogic, Timing};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

    /// 0 for A to 3 for D.
    pub fn index(self) -> usize {
        self as usize
    }

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
            "matter_pairing" => {
                let _ = self.tx.send(Event::PairButton);
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

/// Where the Matter node takes button events from.
pub trait SwitchSink: Send + Sync {
    /// One event of `button`; `down` is whether the button is held once the event has happened.
    fn send(&self, button: Button, event: SwitchEvent, down: bool);
}

/// Everything that happens when a button goes down or up: its action, and (with Matter buttons on) the Switch events.
pub struct Hub {
    actions: Actions,
    logics: [SwitchLogic; 4],
    sink: Option<Arc<dyn SwitchSink>>,
    /// Which buttons report to Matter (the others only run their own action).
    exposed: [bool; 4],
    start: Instant,
}

impl Hub {
    /// `matter` is the timing, the sink and the buttons that report to Matter, or `None` when none do. A button always
    /// runs its own action (`[buttons]`) on a press.
    pub fn new(actions: Actions, matter: Option<(Timing, Arc<dyn SwitchSink>, [bool; 4])>) -> Hub {
        let (timing, sink, exposed) = match matter {
            Some((t, s, e)) => (t, Some(s), e),
            None => (Timing::default(), None, [false; 4]),
        };
        Hub { actions, logics: std::array::from_fn(|_| SwitchLogic::new(timing)), sink, exposed, start: Instant::now() }
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// The button went down (`pressed`) or up now.
    pub fn edge(&mut self, b: Button, pressed: bool) {
        let now = self.now();
        self.edge_at(b, pressed, now);
    }

    pub fn edge_at(&mut self, b: Button, pressed: bool, now: u64) {
        // Close any tap sequence whose time has run out before this edge starts a new one.
        self.tick_at(now);
        if pressed {
            self.actions.press(b);
        }
        if let Some(sink) = self.sink.as_ref().filter(|_| self.exposed[b.index()]) {
            let logic = &mut self.logics[b.index()];
            let events = logic.edge(pressed, now);
            let down = logic.is_down();
            for e in events {
                sink.send(b, e, down);
            }
        }
    }

    /// Time has passed: long presses and the ends of tap sequences are due.
    pub fn tick(&mut self) {
        let now = self.now();
        self.tick_at(now);
    }

    pub fn tick_at(&mut self, now: u64) {
        let Some(sink) = &self.sink else { return };
        for b in [Button::A, Button::B, Button::C, Button::D].into_iter().filter(|b| self.exposed[b.index()]) {
            let logic = &mut self.logics[b.index()];
            let events = logic.tick(now);
            let down = logic.is_down();
            for e in events {
                sink.send(b, e, down);
            }
        }
    }

    /// How long until `tick` has something to do (`None` while nothing is pending).
    pub fn timeout(&self) -> Option<Duration> {
        let now = self.now();
        self.logics.iter().filter_map(SwitchLogic::next_deadline).min().map(|d| Duration::from_millis(d.saturating_sub(now)))
    }
}

/// Block forever reading the GPIO buttons.
#[cfg(all(feature = "hardware", target_os = "linux"))]
pub fn watch_gpio(mut hub: Hub) -> anyhow::Result<()> {
    use anyhow::Context;
    use gpiocdev::line::{Bias, EdgeDetection, EdgeKind};
    let mut b = gpiocdev::Request::builder();
    b.on_chip(first_bcm_chip()).with_consumer("inkyframe-buttons");
    for pin in Button::PINS {
        b.with_line(pin).as_input().with_bias(Bias::PullUp).with_edge_detection(EdgeDetection::BothEdges).with_debounce_period(Duration::from_millis(20));
    }
    let req = b.request().context("cannot claim the button GPIOs (is the user in the gpio group?)")?;
    loop {
        // Wake for an edge, or when a long press or the end of a tap sequence is due.
        let wait = hub.timeout().unwrap_or(Duration::from_secs(3600));
        if req.wait_edge_event(wait)? {
            let ev = req.read_edge_event()?;
            if let Some(button) = Button::from_pin(ev.offset) {
                // Pulled up: a falling edge is a press, a rising edge a release.
                hub.edge(button, ev.kind == EdgeKind::Falling);
            }
        } else {
            hub.tick();
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

/// Development stand-in for the buttons. Type a letter and Enter: `a` taps button A, `a2` double-taps it, `a~` holds it
/// for a long press, `a+` presses and `a-` releases it.
pub fn watch_stdin(mut hub: Hub) {
    use std::io::BufRead;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    // Keep time moving (so timers fire) while a scripted press is "held".
    fn wait(hub: &mut Hub, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            hub.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    loop {
        let line = match rx.recv_timeout(hub.timeout().unwrap_or(Duration::from_secs(3600))) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                hub.tick();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let line = line.trim();
        let mut chars = line.chars();
        let Some(b) = chars.next().and_then(Button::from_char) else {
            if !line.is_empty() {
                eprintln!("type a, b, c or d, then nothing (tap), 2 (double tap), ~ (long hold), + (press) or - (release)");
            }
            continue;
        };
        let tap = |hub: &mut Hub| {
            hub.edge(b, true);
            wait(hub, 60);
            hub.edge(b, false);
        };
        match chars.next() {
            None => tap(&mut hub),
            Some('2') => {
                tap(&mut hub);
                wait(&mut hub, 100);
                tap(&mut hub);
            }
            Some('~') => {
                hub.edge(b, true);
                wait(&mut hub, 800);
                hub.edge(b, false);
            }
            Some('+') => hub.edge(b, true),
            Some('-') => hub.edge(b, false),
            Some(_) => eprintln!("unknown suffix"),
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

    /// Collects what the Matter buttons would send.
    #[derive(Default)]
    struct Collect(std::sync::Mutex<Vec<(Button, SwitchEvent)>>);

    impl SwitchSink for Collect {
        fn send(&self, button: Button, event: SwitchEvent, _down: bool) {
            self.0.lock().unwrap().push((button, event));
        }
    }

    #[test]
    fn only_the_exposed_buttons_report_to_matter_but_every_button_keeps_its_action() {
        let (tx, rx) = channel();
        let sink = Arc::new(Collect::default());
        let cfg = Config::default(); // D is next_photo
        let mut hub = Hub::new(Actions::new(&cfg, tx), Some((Timing::default(), sink.clone(), [true, true, true, false])));
        hub.edge_at(Button::D, true, 0);
        hub.edge_at(Button::D, false, 80);
        hub.tick_at(1000);
        assert!(matches!(rx.try_recv(), Ok(Event::Next)), "next_photo still runs");
        assert!(sink.0.lock().unwrap().is_empty(), "and D sends nothing to Matter");
        hub.edge_at(Button::C, true, 2000);
        hub.edge_at(Button::C, false, 2080);
        hub.tick_at(3000);
        let got: Vec<_> = sink.0.lock().unwrap().iter().map(|(b, e)| (*b, *e)).collect();
        assert_eq!(got, [(Button::C, SwitchEvent::InitialPress), (Button::C, SwitchEvent::ShortRelease), (Button::C, SwitchEvent::MultiPressComplete(1))]);
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
