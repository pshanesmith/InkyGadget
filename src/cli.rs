//! Command-line commands.

use crate::config::Config;
use std::path::Path;
use crate::decode::{prepare_file, Limits};
use crate::overlay::{Overlay, OverlayStyle, PlateStyle, DISPLAY_H, DISPLAY_W};
use crate::render::{render, to_preview_rgb, RenderOptions};
use anyhow::{bail, Context, Result};

pub const USAGE: &str = "\
inkyframe: bedside photo frame for the Inky Impression 5.7\"

  inkyframe run [--config FILE] [--emulate OUT.png]
                                                run the frame (the systemd service); --emulate writes PNGs and reads a/b/c/d from stdin
  inkyframe check-config [--config FILE]        check that the config file loads (exit status 1 and the reason if not)
  inkyframe geocode ZIP [--config FILE]         look up a US ZIP code and store the location
  inkyframe ha-discover [--config FILE]         list Home Assistant scenes and scripts (needs INKYFRAME_HA_TOKEN)
  inkyframe matter qr [--out FILE.png]          the Matter pairing code (QR text, manual code, QR picture)
  inkyframe matter reset                        forget all Matter controllers and make a new pairing code (stop the service first)
  inkyframe hwtest [--white] [--buttons]        colour bars (or a white screen) on the panel, and watch the buttons
  inkyframe hwtest --sensor [--config FILE]     print the SCD40 readings for a minute (stop the service first)
  inkyframe preview PHOTO --out FILE.png        render a photo as the panel would show it
      [--left TEXT] [--right TEXT] [--bottom TEXT] [--bars black|white] [--plate auto|light|dark]
      [--scale N] [--saturation 0..1]
  inkyframe --version
";

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

pub fn preview(args: &[String]) -> Result<()> {
    let photo = args.first().filter(|a| !a.starts_with("--")).context("usage: inkyframe preview PHOTO --out FILE.png")?;
    let out = opt(args, "--out").context("--out FILE.png is required")?;
    let bars = match opt(args, "--bars").unwrap_or("white") {
        "white" => [255, 255, 255],
        "black" => [0, 0, 0],
        other => bail!("--bars must be white or black, not {other:?}"),
    };
    let plate = match opt(args, "--plate").unwrap_or("auto") {
        "auto" => PlateStyle::Auto,
        "light" => PlateStyle::Light,
        "dark" => PlateStyle::Dark,
        other => bail!("--plate must be auto, light or dark, not {other:?}"),
    };
    let ro = RenderOptions {
        saturation: opt(args, "--saturation").map(str::parse).transpose()?.unwrap_or(0.5),
        overlay: OverlayStyle { scale: opt(args, "--scale").map(str::parse).transpose()?.unwrap_or(2), plate, ..Default::default() },
    };
    let overlay = Overlay { left: opt(args, "--left").map(String::from), right: opt(args, "--right").map(String::from), bottom: opt(args, "--bottom").map(String::from) };
    let t = std::time::Instant::now();
    let prepared = prepare_file(Path::new(photo), bars, &Limits::default())?;
    let t_prep = t.elapsed();
    let frame = render(&prepared, &overlay, &ro);
    let rgb = to_preview_rgb(&frame);
    image::RgbImage::from_raw(DISPLAY_W as u32, DISPLAY_H as u32, rgb).context("frame size")?.save(out)?;
    println!("wrote {out} (decode+fit {t_prep:.1?}, total {:.1?})", t.elapsed());
    Ok(())
}

/// `inkyframe check-config`: load and validate the file, and say what it will do.
pub fn check_config(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    let buttons = [("A", &cfg.buttons.a), ("B", &cfg.buttons.b), ("C", &cfg.buttons.c), ("D", &cfg.buttons.d)];
    println!("{} is valid", path.display());
    println!("  new photo at {:?}{}", cfg.photos.refresh_times, if cfg.photos.interval_minutes > 0 { format!(" and every {} min", cfg.photos.interval_minutes) } else { String::new() });
    println!("  date \"{}\", {} at \"{}\"", cfg.overlay.date_format, cfg.overlay.units, if cfg.location.label.is_empty() { "no location set" } else { &cfg.location.label });
    for (name, target) in buttons {
        println!("  button {name}: {}", if target.is_empty() { "off" } else { target });
    }
    let s = &cfg.sensor;
    if s.enabled {
        println!("  sensor: SCD40 on /dev/i2c-{}, {}, temperature offset {} C{}", s.i2c_bus, if s.low_power { "a sample every 30 s" } else { "a sample every 5 s" }, s.temperature_offset_c, if cfg.overlay.show_air && cfg.overlay.enabled { ", shown bottom left" } else { "" });
    } else {
        println!("  sensor: off");
    }
    Ok(())
}

pub fn run(cfg: Config, args: &[String]) -> Result<()> {
    use crate::app::{heartbeat, App, Event, SCAN_SECONDS};
    use crate::buttons::Actions;
    use crate::display::{Emulator, Panel};
    use std::sync::mpsc::channel;

    let (tx, rx) = channel();
    let emulate = opt(args, "--emulate");
    let panel: Box<dyn Panel> = match emulate {
        Some(out) => Box::new(Emulator { out: out.into() }),
        None => open_panel()?,
    };
    let actions = Actions::new(&cfg, tx.clone());
    if emulate.is_some() {
        std::thread::spawn(move || crate::buttons::watch_stdin(actions));
    } else {
        spawn_gpio_buttons(actions);
    }
    // Rescan the shared folder now and then (a file counts once it has stopped changing).
    let scan_tx = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(SCAN_SECONDS));
        if scan_tx.send(Event::Scan).is_err() {
            break;
        }
    });
    let sensor_cfg = cfg.sensor.clone();
    let matter_cfg = cfg.clone();
    let mut app = App::new(cfg, panel, rx, tx.clone())?;
    crate::sensor::start(&sensor_cfg, app.air_handle(), app.stop_flag(), emulate.is_some());
    start_matter(&matter_cfg, app.air_handle(), tx.clone());
    // On SIGTERM/SIGINT: tell long waits (clock, weather) to end, then wake the main loop.
    let stop = app.stop_flag();
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT])?;
    std::thread::spawn(move || {
        for _ in signals.forever() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(Event::Stop);
        }
    });
    heartbeat(app.progress.clone());
    log::info!("inkyframe {} running", env!("CARGO_PKG_VERSION"));
    app.run();
    Ok(())
}

/// The Matter node runs on its own thread; if it fails (no network yet, say) it is started again a minute later.
#[cfg(feature = "matter")]
fn start_matter(cfg: &Config, latest: crate::sensor::Latest, tx: std::sync::mpsc::Sender<crate::app::Event>) {
    if !cfg.matter.enabled {
        return;
    }
    let cfg = cfg.clone();
    std::thread::spawn(move || loop {
        match crate::matter::run(&cfg, latest.clone(), tx.clone()) {
            Ok(()) => {}
            Err(e) => log::error!("Matter node stopped: {e:#}; starting it again in a minute"),
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    });
}

#[cfg(not(feature = "matter"))]
fn start_matter(cfg: &Config, _latest: crate::sensor::Latest, _tx: std::sync::mpsc::Sender<crate::app::Event>) {
    if cfg.matter.enabled {
        log::error!("[matter] is enabled, but this build has no Matter support (build with --features matter)");
    }
}

/// `inkyframe matter qr | reset`: the pairing code of this frame, or forget all controllers and make a new code.
#[cfg(feature = "matter")]
pub fn matter(args: &[String], config_path: &str) -> Result<()> {
    let path = Path::new(config_path);
    let cfg = if path.exists() { Config::load(path)? } else { Config::default() };
    let dir = crate::matter::store_dir(&cfg.photos.state_dir);
    match args.first().map(String::as_str) {
        Some("qr") => {
            let o = crate::matter::Pairing::load_or_create(&dir)?.onboarding(&cfg.matter)?;
            println!("Manual pairing code: {}\nQR text: {}\n", o.manual_code, o.qr_text);
            // Two rows of modules per line of text, with a 4-module white border.
            let dark = |x: i32, y: i32| x >= 0 && y >= 0 && (x as usize) < o.width && (y as usize) < o.width && o.modules[y as usize * o.width + x as usize];
            for y in (-4..o.width as i32 + 4).step_by(2) {
                let line: String = (-4..o.width as i32 + 4)
                    .map(|x| match (dark(x, y), dark(x, y + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    })
                    .collect();
                println!("{line}");
            }
            if let Some(out) = opt(args, "--out") {
                let scale = 8usize;
                let side = (o.width + 8) * scale;
                let img = image::GrayImage::from_fn(side as u32, side as u32, |px, py| {
                    let (x, y) = ((px as usize / scale) as i32 - 4, (py as usize / scale) as i32 - 4);
                    image::Luma([if dark(x, y) { 0 } else { 255 }])
                });
                img.save(out)?;
                println!("wrote {out}");
            }
            Ok(())
        }
        Some("reset") => {
            if dir.exists() {
                std::fs::remove_dir_all(&dir).with_context(|| format!("cannot remove {}", dir.display()))?;
            }
            println!("removed {}: all controllers are forgotten and a new pairing code is made at the next start. Remove the frame from your controller app too.", dir.display());
            Ok(())
        }
        _ => bail!("usage: inkyframe matter qr [--out FILE.png] | inkyframe matter reset (stop the service first)"),
    }
}

#[cfg(not(feature = "matter"))]
pub fn matter(_args: &[String], _config_path: &str) -> Result<()> {
    bail!("this build has no Matter support (build with --features matter)")
}

#[cfg(all(feature = "hardware", target_os = "linux"))]
fn open_panel() -> Result<Box<dyn crate::display::Panel>> {
    Ok(Box::new(crate::display::uc8159::Uc8159::open()?))
}

#[cfg(not(all(feature = "hardware", target_os = "linux")))]
fn open_panel() -> Result<Box<dyn crate::display::Panel>> {
    bail!("this build has no panel driver; use --emulate FILE.png, or build with --features hardware for the Pi")
}

#[cfg(all(feature = "hardware", target_os = "linux"))]
fn spawn_gpio_buttons(actions: crate::buttons::Actions) {
    std::thread::spawn(move || {
        if let Err(e) = crate::buttons::watch_gpio(actions) {
            log::error!("buttons stopped working: {e:#}");
        }
    });
}

#[cfg(not(all(feature = "hardware", target_os = "linux")))]
fn spawn_gpio_buttons(_actions: crate::buttons::Actions) {}

/// `inkyframe geocode ZIP [--config FILE]`: look up the location once and write it into the config.
pub fn geocode(args: &[String], config_path: &str) -> Result<()> {
    let zip = args.first().filter(|a| !a.starts_with("--")).context("usage: inkyframe geocode ZIP [--config FILE]")?;
    let place = crate::geocode::lookup_us_zip(zip)?;
    println!("{zip}: {} ({:.4}, {:.4})", place.label, place.latitude, place.longitude);
    let path = Path::new(config_path);
    if path.exists() {
        let text = std::fs::read_to_string(path)?;
        std::fs::write(path, crate::geocode::set_location(&text, zip, &place))?;
        println!("updated [location] in {config_path}");
    } else {
        println!("{config_path} does not exist; put these values in [location] yourself");
    }
    Ok(())
}

/// `inkyframe ha-discover`: list the scenes and scripts Home Assistant has, to pick the button targets from.
/// Reads INKYFRAME_HA_TOKEN (and INKYFRAME_HA_URL) from the environment; run it with the token loaded, for example
/// `sudo sh -c '. /etc/inkyframe/secrets.env && export INKYFRAME_HA_TOKEN && inkyframe ha-discover'`.
pub fn ha_discover(cfg: &Config) -> Result<()> {
    let token = std::env::var(crate::ha::TOKEN_ENV).ok().filter(|t| !t.is_empty()).with_context(|| format!("{} is not set", crate::ha::TOKEN_ENV))?;
    let url = std::env::var(crate::ha::URL_ENV).ok().filter(|u| !u.is_empty()).unwrap_or_else(|| cfg.homeassistant.url.clone());
    let found = crate::ha::list_scenes_and_scripts(&url, &token, std::time::Duration::from_secs(cfg.homeassistant.timeout_seconds))?;
    if found.is_empty() {
        println!("no scenes or scripts found at {url}");
    }
    for (id, name) in found {
        println!("{id:<50} {name}");
    }
    Ok(())
}

/// `inkyframe hwtest [--buttons]`: colour bars on the panel, then print button presses.
pub fn hwtest(args: &[String], config_path: &str) -> Result<()> {
    if args.iter().any(|a| a == "--sensor") {
        // The sensor on its own; the service must be stopped (two programs would fight over its measurement mode).
        let path = Path::new(config_path);
        let cfg = if path.exists() { Config::load(path)? } else { Config::default() };
        return crate::sensor::test(&cfg.sensor);
    }
    let mut panel = open_panel()?;
    let white = args.iter().any(|a| a == "--white");
    println!("drawing {} (about 30 s) ...", if white { "a plain white screen" } else { "colour bars" });
    if white {
        panel.show(&vec![crate::palette::WHITE; DISPLAY_W * DISPLAY_H])?;
    } else {
        panel.show(&crate::display::colour_bars())?;
    }
    println!("done. If the panel shows 7 colour bars (black, white, green, blue, red, yellow, orange) the wiring is fine.");
    if args.iter().any(|a| a == "--buttons") {
        println!("press the buttons A to D (Ctrl-C to stop)");
        buttons_test()?;
    }
    Ok(())
}

#[cfg(all(feature = "hardware", target_os = "linux"))]
fn buttons_test() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut cfg = Config::default();
    cfg.buttons.d = "next_photo".into();
    let actions = crate::buttons::Actions::new(&cfg, tx);
    std::thread::spawn(move || {
        let _ = crate::buttons::watch_gpio(actions);
    });
    for _ in rx {}
    Ok(())
}

#[cfg(not(all(feature = "hardware", target_os = "linux")))]
fn buttons_test() -> Result<()> {
    bail!("no GPIO in this build")
}

#[cfg(test)]
mod tests {
    use super::check_config;

    #[test]
    fn check_config_accepts_a_good_file_and_names_the_problem_in_a_bad_one() {
        let d = tempfile::tempdir().unwrap();
        let (good, dup, typo) = (d.path().join("good.toml"), d.path().join("dup.toml"), d.path().join("typo.toml"));
        std::fs::write(&good, "[overlay]\nmargin = 2\n").unwrap();
        std::fs::write(&dup, "[overlay]\nmargin = 0\nmargin = 0\n").unwrap();
        std::fs::write(&typo, "[overlay]\ndate_format = \"%Q\"\n").unwrap();
        check_config(&good).unwrap();
        assert!(format!("{:#}", check_config(&dup).unwrap_err()).contains("duplicate key"));
        assert!(format!("{:#}", check_config(&typo).unwrap_err()).contains("date_format"));
        assert!(check_config(&d.path().join("missing.toml")).is_err());
    }
}
