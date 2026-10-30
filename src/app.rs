//! The running frame: one loop that owns the panel and decides when to refresh it.

use crate::config::{parse_time, Config};
use crate::decode::Limits;
use crate::display::Panel;
use crate::library::{process_item, Item, Library};
use crate::overlay::{Overlay, OverlayStyle, PlateStyle};
use crate::render::{render, RenderOptions};
use crate::schedule;
use crate::state::{pick, State};
use crate::weather::{self, Forecast};
use crate::welcome;
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use std::fmt::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum Event {
    /// Time to look at the shared folder.
    Scan,
    /// Show the next photo now (button D).
    Next,
    /// A photo finished decoding (`true`) or could not be decoded (`false`).
    Processed(String, bool),
    Stop,
}

pub const SCAN_SECONDS: u64 = 30;
const SHARE_USER: &str = "inkyframe";
/// Two looks at the folder closer together than this do not count as "unchanged between scans".
const MIN_SCAN_GAP: Duration = Duration::from_secs(SCAN_SECONDS - 5);
/// How many cached photos to try in one refresh before giving up until the next gap.
const LOAD_TRIES: usize = 3;
/// Wait after a failed panel update, and how many failures in a row before waiting for the next scheduled time.
const PANEL_RETRY: Duration = Duration::from_secs(120);
const PANEL_RETRIES: u32 = 5;
/// How often the watchdog is told the main loop is alive (the unit allows 900 s).
const HEARTBEAT_SECONDS: u64 = 60;

pub struct App {
    cfg: Config,
    panel: Box<dyn Panel>,
    lib: Library,
    state: State,
    rx: Receiver<Event>,
    tx: Sender<Event>,
    worker_busy: Arc<AtomicBool>,
    weather: Option<(NaiveDate, Forecast)>,
    times: Vec<u32>,
    /// A refresh that is wanted but held back by `min_refresh_seconds`.
    wanted: Option<&'static str>,
    /// After a failed panel update: no new attempt before this.
    hold_until: Option<Instant>,
    panel_failures: u32,
    /// When the folder was last looked at: scans queued up behind a long refresh must not run back to back, or a file
    /// still being copied could look unchanged between them.
    last_scan: Option<Instant>,
    /// Set by the signal handler: long waits end early so the service stops promptly.
    stop: Arc<AtomicBool>,
    pub progress: Arc<std::sync::atomic::AtomicU64>,
}

fn epoch_to_local(s: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(s, 0).single()
}

/// systemd-timesyncd drops this file once the clock has been set from the network.
fn clock_is_set() -> bool {
    !cfg!(target_os = "linux") || Path::new("/run/systemd/timesync/synchronized").exists()
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The date text, or `None` if the format cannot be applied (chrono reports that as an error, and `to_string` would panic).
fn format_date(now: DateTime<Local>, format: &str) -> Option<String> {
    let mut s = String::new();
    write!(s, "{}", now.format(format)).ok()?;
    Some(s)
}

/// Clears the worker flag when the worker thread ends, however it ends.
struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Run one decode. A panic inside a decoder is a failure of that photo, not the end of all decoding.
fn guarded<F: FnOnce() -> anyhow::Result<()>>(f: F) -> anyhow::Result<()> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let text = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            Err(anyhow::anyhow!("the decoder panicked: {text}"))
        }
    }
}

impl App {
    pub fn new(cfg: Config, panel: Box<dyn Panel>, rx: Receiver<Event>, tx: Sender<Event>) -> anyhow::Result<Self> {
        let limits = Limits { max_pixels: cfg.photos.max_pixels, ..Limits::default() };
        let bars = cfg.bar_rgb();
        let lib = Library::new(&cfg.photos.dir, &cfg.photos.state_dir, bars, limits);
        let state = State::load(&cfg.photos.state_dir);
        let times = cfg.photos.refresh_times.iter().map(|t| parse_time(t)).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(App {
            cfg,
            panel,
            lib,
            state,
            rx,
            tx,
            worker_busy: Arc::new(AtomicBool::new(false)),
            weather: None,
            times,
            wanted: None,
            hold_until: None,
            panel_failures: 0,
            last_scan: None,
            stop: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Default::default()),
        })
    }

    /// Flag that ends long waits early; the signal handler sets it before it sends `Event::Stop`.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Sleep, but wake early when asked to stop. `false` means stop.
    fn nap(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if self.stopping() {
                return false;
            }
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(Duration::from_millis(200)));
        }
    }

    fn last(&self) -> Option<DateTime<Local>> {
        self.state.last_refresh.and_then(epoch_to_local)
    }

    fn render_options(&self) -> RenderOptions {
        let plate = match self.cfg.overlay.plate.as_str() {
            "light" => PlateStyle::Light,
            "dark" => PlateStyle::Dark,
            _ => PlateStyle::Auto,
        };
        RenderOptions { saturation: self.cfg.display.saturation, overlay: OverlayStyle { scale: self.cfg.overlay.scale, margin: self.cfg.overlay.margin, plate } }
    }

    fn min_gap_ok(&self, now: DateTime<Local>) -> bool {
        self.gap_ok(now, self.cfg.display.min_refresh_seconds)
    }

    fn gap_ok(&self, now: DateTime<Local>, secs: u64) -> bool {
        self.last().is_none_or(|l| (now - l).num_seconds() >= secs as i64)
    }

    fn held(&self) -> bool {
        self.hold_until.is_some_and(|t| Instant::now() < t)
    }

    /// Look at the folder once: start decoding anything new, forget cached copies of deleted photos.
    fn scan(&mut self) {
        if self.last_scan.is_some_and(|t| t.elapsed() < MIN_SCAN_GAP) {
            return;
        }
        self.last_scan = Some(Instant::now());
        let snap = self.lib.rescan();
        if !snap.pending.is_empty() && !self.worker_busy.swap(true, Ordering::SeqCst) {
            self.start_worker(snap.pending);
        }
        if self.state.welcome && !snap.ready.is_empty() {
            self.wanted = Some("first photo");
        }
        if !self.worker_busy.load(Ordering::SeqCst) {
            self.lib.remove_stale(&snap.stale);
        }
    }

    fn start_worker(&self, items: Vec<Item>) {
        let (tx, busy, failed) = (self.tx.clone(), self.worker_busy.clone(), self.lib.failed_handle());
        let (cache, bars, limits) = (self.lib.cache_dir().to_path_buf(), self.lib.bars(), self.lib.limits());
        std::thread::spawn(move || {
            let _busy = BusyGuard(busy);
            for item in items {
                // A scan that raced with the end of the previous worker may hand over a photo that is already done.
                let done = cache.join(format!("{}.jpg", item.key)).is_file()
                    || failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&item.key);
                if done {
                    continue;
                }
                let t = Instant::now();
                let ok = match guarded(|| process_item(&cache, &item, bars, &limits)) {
                    Ok(()) => {
                        log::info!("prepared {} in {:.1?}", item.rel, t.elapsed());
                        true
                    }
                    Err(e) => {
                        log::error!("skipping {}: {e:#}", item.rel);
                        // Recorded here, before the flag clears, so the next scan cannot start on it again.
                        failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(item.key.clone());
                        false
                    }
                };
                let _ = tx.send(Event::Processed(item.key, ok));
            }
        });
    }

    fn forecast(&mut self, today: NaiveDate) -> Option<Forecast> {
        if let Some((d, f)) = &self.weather {
            if *d == today {
                return Some(f.clone());
            }
        }
        let fahrenheit = self.cfg.overlay.units == "fahrenheit";
        for (n, wait) in [0u64, 5, 20].into_iter().enumerate() {
            if !self.nap(Duration::from_secs(wait)) {
                return None;
            }
            match weather::fetch(&self.cfg.location, fahrenheit, Duration::from_secs(15)) {
                Ok(f) => {
                    self.weather = Some((today, f.clone()));
                    return Some(f);
                }
                Err(e) => log::warn!("weather attempt {}: {e:#}", n + 1),
            }
        }
        None
    }

    fn overlay(&mut self, now: DateTime<Local>) -> Overlay {
        if !self.cfg.overlay.enabled {
            return Overlay::default();
        }
        let right = self.forecast(now.date_naive()).map(|f| weather::overlay_text(&f));
        // Without a clock set from the network the date would be wrong, so it is left off.
        let left = if clock_is_set() { format_date(now, &self.cfg.overlay.date_format) } else { None };
        Overlay { left, right }
    }

    /// Show a new photo (or the welcome screen if there is none).
    fn refresh(&mut self, why: &str) {
        self.wait_for_clock();
        if self.stopping() {
            return;
        }
        let now = Local::now();
        let mut keys = self.lib.ready_keys();
        log::info!("refreshing ({why}); {} photos ready", keys.len());
        let had_photos = !keys.is_empty();
        let current = self.state.current.clone();
        let mut chosen = None;
        for _ in 0..LOAD_TRIES {
            let Some(key) = pick(&keys, &mut self.state.played, current.as_deref(), &mut rand::rng()) else { break };
            match self.lib.load(&key) {
                Ok(p) => {
                    let overlay = self.overlay(now);
                    chosen = Some((render(&p, &overlay, &self.render_options()), key));
                    break;
                }
                Err(e) => {
                    // A cache file damaged by a power cut: drop it so the next scan decodes the photo again, and
                    // try another one now.
                    log::error!("{e:#}; it will be decoded again");
                    self.lib.discard(&key);
                    self.state.played.retain(|k| *k != key);
                    keys.retain(|k| *k != key);
                }
            }
        }
        if chosen.is_none() && had_photos {
            // Every try failed: keep what is on the panel, and try again after the minimum gap.
            self.state.last_refresh = Some(now.timestamp());
            self.wanted = Some("retry");
            return;
        }
        let (frame, shown) = match chosen {
            Some((frame, key)) => (frame, Some(key)),
            None => {
                let host = read_trimmed("/etc/hostname").unwrap_or_else(|| "inky".into());
                let pw = read_trimmed("/etc/inkyframe/share-password");
                (welcome::render(&host, SHARE_USER, pw.as_deref()), None)
            }
        };
        if shown.is_none() && self.state.welcome {
            log::info!("welcome screen already showing");
            self.state.last_refresh = Some(now.timestamp());
            return;
        }
        match self.panel.show(&frame) {
            Ok(()) => {
                self.panel_failures = 0;
                self.hold_until = None;
                self.state.welcome = shown.is_none();
                self.state.current = shown;
                self.state.last_refresh = Some(Local::now().timestamp());
            }
            Err(e) => {
                self.panel_failures += 1;
                log::error!("panel update failed ({} in a row): {e:#}", self.panel_failures);
                if let Some(k) = &shown {
                    self.state.played.retain(|p| p != k);
                }
                if self.panel_failures >= PANEL_RETRIES {
                    log::error!("giving up until the next scheduled refresh");
                    self.panel_failures = 0;
                    self.state.last_refresh = Some(Local::now().timestamp());
                } else {
                    // The panel still shows the old picture and the schedule is untouched: try again soon.
                    self.wanted = Some("retry");
                    self.hold_until = Some(Instant::now() + PANEL_RETRY);
                    return;
                }
            }
        }
        if let Err(e) = self.state.save(&self.cfg.photos.state_dir) {
            log::error!("cannot save state: {e:#}");
        }
    }

    fn wait_for_clock(&self) {
        let start = Instant::now();
        while !clock_is_set() && start.elapsed() < Duration::from_secs(120) {
            log::info!("waiting for the clock to be set");
            if !self.nap(Duration::from_secs(5)) {
                return;
            }
        }
    }

    fn beat(&self) {
        self.progress.store(chrono::Utc::now().timestamp() as u64, Ordering::Relaxed);
    }

    /// When the loop next has something to do, apart from events: never earlier than the minimum gap allows (or the
    /// loop would wake every second while a refresh is due but held back), and never earlier than a failed panel
    /// update's hold.
    fn next_wake(&self, now: DateTime<Local>, due: bool) -> DateTime<Local> {
        let min = self.cfg.display.min_refresh_seconds;
        let ready_after = |secs: u64| self.last().map_or(now, |l| l + ChronoDuration::seconds(secs as i64));
        let min_ready = ready_after(min);
        let mut wake = if due { min_ready } else { schedule::next_due(now, &self.times, self.cfg.photos.interval_minutes, self.last()).max(min_ready) };
        if let Some(w) = self.wanted {
            wake = wake.min(ready_after(if w == "first photo" { 60.min(min) } else { min }));
        }
        if let Some(h) = self.hold_until {
            let left = h.saturating_duration_since(Instant::now());
            wake = wake.max(now + ChronoDuration::from_std(left).unwrap_or_else(|_| ChronoDuration::zero()));
        }
        wake
    }

    pub fn run(&mut self) {
        self.scan();
        crate::sdnotify::notify("READY=1");
        loop {
            self.beat();
            if self.stopping() {
                break;
            }
            let now = Local::now();
            let held = self.held();
            // Replacing the welcome screen with the first photo is not worth a long wait: a minute is enough to spare the panel.
            let wanted_gap_ok = match self.wanted {
                Some("first photo") => self.gap_ok(now, 60.min(self.cfg.display.min_refresh_seconds)),
                Some(_) => self.min_gap_ok(now),
                None => false,
            };
            if !held && wanted_gap_ok {
                if let Some(why) = self.wanted.take() {
                    self.refresh(why);
                    continue;
                }
            }
            let due = schedule::missed(now, &self.times, self.cfg.photos.interval_minutes, self.last());
            if !held && due && self.min_gap_ok(now) {
                self.refresh("scheduled");
                continue;
            }
            let wake = self.next_wake(now, due);
            let wait = (wake - now).to_std().unwrap_or(Duration::from_secs(1)).clamp(Duration::from_secs(1), Duration::from_secs(60));
            match self.rx.recv_timeout(wait) {
                Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Event::Scan) => self.scan(),
                Ok(Event::Next) => {
                    if self.min_gap_ok(Local::now()) {
                        self.wanted = Some("button");
                    } else {
                        log::info!("next photo ignored: the panel was refreshed less than {} s ago", self.cfg.display.min_refresh_seconds);
                    }
                }
                Ok(Event::Processed(_, ok)) => {
                    if ok && self.state.welcome {
                        self.wanted = Some("first photo");
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        log::info!("stopping");
    }
}

/// Pings systemd's watchdog while the main loop is making progress (it records a timestamp every pass; a
/// refresh takes about a minute at most, so ten minutes without progress means it is stuck).
pub fn heartbeat(progress: Arc<std::sync::atomic::AtomicU64>) {
    std::thread::spawn(move || loop {
        let last = progress.load(Ordering::Relaxed) as i64;
        if last != 0 && chrono::Utc::now().timestamp() - last < 600 {
            crate::sdnotify::notify("WATCHDOG=1");
        }
        std::thread::sleep(Duration::from_secs(HEARTBEAT_SECONDS));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::Mutex;

    /// Records frames; the first `fail` updates return an error.
    struct MockPanel {
        shown: Arc<Mutex<usize>>,
        fail: Arc<Mutex<u32>>,
    }

    impl Panel for MockPanel {
        fn show(&mut self, _frame: &[u8]) -> anyhow::Result<()> {
            let mut f = self.fail.lock().unwrap();
            if *f > 0 {
                *f -= 1;
                anyhow::bail!("spi error");
            }
            *self.shown.lock().unwrap() += 1;
            Ok(())
        }
    }

    struct Rig {
        app: App,
        shown: Arc<Mutex<usize>>,
        fail: Arc<Mutex<u32>>,
        _photos: tempfile::TempDir,
        _state: tempfile::TempDir,
    }

    fn png(path: &Path, shade: u8) {
        image::RgbImage::from_pixel(400, 300, image::Rgb([shade, 30, 30])).save(path).unwrap();
    }

    /// An app with `n` photos, all decoded and cached; the weather is off, so nothing touches the network.
    fn rig(n: u8) -> Rig {
        let (photos, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for i in 0..n {
            png(&photos.path().join(format!("p{i}.png")), 100 + i * 20);
        }
        let mut cfg = Config::default();
        cfg.photos.dir = photos.path().into();
        cfg.photos.state_dir = state.path().into();
        cfg.overlay.enabled = false;
        let (tx, rx) = channel();
        let (shown, fail) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(0)));
        let panel = Box::new(MockPanel { shown: shown.clone(), fail: fail.clone() });
        let mut app = App::new(cfg, panel, rx, tx).unwrap();
        for item in app.lib.rescan().pending {
            app.lib.process(&item).unwrap();
        }
        Rig { app, shown, fail, _photos: photos, _state: state }
    }

    fn wait_until_idle(app: &App) {
        for _ in 0..200 {
            if !app.worker_busy.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("worker did not finish");
    }

    #[test]
    fn a_refresh_shows_a_photo_and_records_it() {
        let mut r = rig(2);
        r.app.refresh("test");
        assert_eq!(*r.shown.lock().unwrap(), 1);
        assert!(r.app.state.current.is_some() && !r.app.state.welcome);
        assert!(r.app.state.last_refresh.is_some());
    }

    #[test]
    fn a_damaged_cache_copy_is_dropped_and_another_photo_is_shown() {
        let mut r = rig(2);
        let keys = r.app.lib.ready_keys();
        let (bad, good) = (keys[0].clone(), keys[1].clone());
        std::fs::write(r.app.lib.cache_dir().join(format!("{bad}.jpg")), b"").unwrap(); // a power cut left it empty
        r.app.state.played = vec![good.clone()]; // so the damaged one is picked first
        r.app.refresh("test");
        assert_eq!(*r.shown.lock().unwrap(), 1, "a photo was still shown");
        assert_eq!(r.app.state.current.as_deref(), Some(good.as_str()));
        assert!(!r.app.lib.is_ready(&bad), "the damaged copy was removed, so the next scan decodes it again");
        assert_eq!(r.app.lib.rescan().pending.len(), 1);
    }

    #[test]
    fn when_every_copy_is_damaged_the_panel_is_left_alone_and_a_retry_is_queued() {
        let mut r = rig(1);
        let key = r.app.lib.ready_keys().remove(0);
        std::fs::write(r.app.lib.cache_dir().join(format!("{key}.jpg")), b"").unwrap();
        r.app.refresh("test");
        assert_eq!(*r.shown.lock().unwrap(), 0, "no welcome screen over a photo that is only being decoded again");
        assert_eq!(r.app.wanted, Some("retry"));
        assert!(r.app.state.last_refresh.is_some(), "the minimum gap paces the retry");
    }

    #[test]
    fn a_failed_panel_update_keeps_the_schedule_and_retries_soon() {
        let mut r = rig(1);
        *r.fail.lock().unwrap() = 1;
        r.app.refresh("test");
        assert_eq!(r.app.state.last_refresh, None, "still due");
        assert_eq!(r.app.state.current, None);
        assert!(r.app.state.played.is_empty(), "the photo is not marked as seen");
        assert_eq!(r.app.wanted, Some("retry"));
        assert!(r.app.held());
        r.app.hold_until = None;
        r.app.wanted = None;
        r.app.refresh("retry");
        assert_eq!(*r.shown.lock().unwrap(), 1);
        assert!(r.app.state.last_refresh.is_some());
    }

    #[test]
    fn repeated_panel_failures_give_up_until_the_next_scheduled_time() {
        let mut r = rig(1);
        *r.fail.lock().unwrap() = 100;
        for _ in 0..PANEL_RETRIES {
            r.app.hold_until = None;
            r.app.refresh("test");
        }
        assert!(r.app.state.last_refresh.is_some(), "no more immediate retries");
        assert!(!r.app.held());
    }

    #[test]
    fn a_panic_in_a_decoder_is_a_failed_photo() {
        let e = guarded(|| panic!("boom")).unwrap_err();
        assert!(format!("{e}").contains("boom"), "{e}");
        assert!(guarded(|| Ok(())).is_ok());
        let busy = Arc::new(AtomicBool::new(true));
        let b = busy.clone();
        assert!(std::thread::spawn(move || {
            let _g = BusyGuard(b);
            panic!("worker died");
        })
        .join()
        .is_err());
        assert!(!busy.load(Ordering::SeqCst), "the flag is cleared even if the worker thread dies");
    }

    #[test]
    fn a_photo_that_cannot_be_decoded_is_recorded_before_the_worker_is_free() {
        let mut r = rig(1);
        std::fs::write(r._photos.path().join("bad.jpg"), b"not a jpeg").unwrap();
        r.app.lib.rescan(); // first sighting
        let pending = r.app.lib.rescan().pending;
        assert_eq!(pending.len(), 1);
        r.app.worker_busy.store(true, Ordering::SeqCst);
        r.app.start_worker(pending);
        wait_until_idle(&r.app);
        assert!(r.app.lib.rescan().pending.is_empty(), "not handed to a second worker");
    }

    #[test]
    fn the_loop_does_not_spin_while_a_refresh_is_due_but_held_back() {
        let mut r = rig(1);
        r.app.cfg.photos.interval_minutes = 5;
        r.app.cfg.display.min_refresh_seconds = 600;
        let now = Local::now();
        r.app.state.last_refresh = Some((now - ChronoDuration::seconds(400)).timestamp());
        let due = schedule::missed(now, &r.app.times, 5, r.app.last());
        assert!(due && !r.app.min_gap_ok(now));
        let wait = r.app.next_wake(now, due) - now;
        assert!(wait > ChronoDuration::seconds(150) && wait <= ChronoDuration::seconds(200), "{wait}");
    }

    #[test]
    fn a_failed_panel_update_holds_the_loop_back() {
        let mut r = rig(1);
        r.app.hold_until = Some(Instant::now() + Duration::from_secs(100));
        r.app.wanted = Some("retry");
        let now = Local::now();
        assert!(r.app.next_wake(now, false) - now >= ChronoDuration::seconds(99));
    }

    #[test]
    fn scans_queued_up_behind_a_long_refresh_do_not_run_back_to_back() {
        let mut r = rig(0);
        r.app.scan(); // startup
        png(&r._photos.path().join("new.png"), 90);
        let long_ago = || Some(Instant::now() - Duration::from_secs(40));
        // Three scans queued up one after another: only the first one looks at the folder.
        r.app.last_scan = long_ago();
        r.app.scan(); // first sighting of the new file
        r.app.scan();
        r.app.scan();
        wait_until_idle(&r.app);
        assert!(r.app.lib.ready_keys().is_empty(), "back-to-back scans must not make a file look unchanged");
        r.app.last_scan = long_ago();
        r.app.scan(); // second sighting, a full interval later: settled, decoded
        wait_until_idle(&r.app);
        assert_eq!(r.app.lib.ready_keys().len(), 1);
    }

    #[test]
    fn a_date_format_chrono_cannot_apply_is_dropped_not_a_panic() {
        let now = Local::now();
        assert!(format_date(now, "%A %B %-d").is_some());
        assert_eq!(format_date(now, "%Q"), None);
        assert_eq!(format_date(now, "%"), None);
    }

    #[test]
    fn stopping_ends_a_nap_early() {
        let r = rig(0);
        let flag = r.app.stop_flag();
        let t = Instant::now();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        assert!(!r.app.nap(Duration::from_secs(30)));
        h.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
