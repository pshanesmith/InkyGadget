# InkyGadget

A photo frame: a Raspberry Pi Zero W and a Pimoroni Inky Impression 5.7" (600x448, 7-colour e-paper),
written in Rust. Drop your photos (JPEG, PNG or iPhone HEIC) into a network folder; the frame shows one a
day with the day and date at the top left and today's high and low at the top right. Three of the four buttons
on the side run Home Assistant scenes or scripts, and the fourth shows the next photo.

- **Photos** arrive over an SMB share (`smb://inky.local/Images`), like the
  [inky-photo-frame](https://github.com/mehdi7129/inky-photo-frame) project. HEIC is decoded in pure Rust
  (no libheif). Each photo is decoded once, in the background, and cached at display size.
- **Rotation**: a new photo at each time in `refresh_times` (default midnight, so the date and forecast are
  current as the day starts), optionally every N minutes. Photos are shuffled with history, so nothing repeats
  until all have been shown. Portrait photos get white (or black) bars at the sides; nothing is cropped.
  Times and the date follow the Pi's time zone (`timedatectl`; set it with `sudo timedatectl set-timezone America/New_York`).
- **Buttons** (A is the top one): A, B and C each call one scene or script that you maintain in Home Assistant
  (any mix of the two); D shows the next photo. Set them in `[buttons]`.
- **Power and packages**: no Python, no desktop, no Bluetooth/audio/HDMI stack; the only extra package is Samba.
  The panel keeps its picture with no power, so the Pi is idle almost all day.

## How it works

One program (`inkyframe`, plain threads, no async runtime) runs as a single systemd service as an unprivileged user
(groups `spi` and `gpio`).

- **Library.** Every 30 seconds it looks at the share. A photo counts once its size and modification time are
  unchanged between two looks, so a half-copied upload is never read (at start-up everything counts). A background
  worker fits each new photo inside 600x448 and stores it as a JPEG in `/var/lib/inkyframe/cache`, keyed by name,
  size and modification time. The originals are never touched; deleting a photo from the share removes its cache
  file. A photo that cannot be decoded is logged and skipped, and a damaged cache file is dropped and decoded again.
- **Refresh.** A photo is picked at random, with the history kept in `state.json` so nothing repeats until all have
  been shown. It is dithered to the panel's measured colours (serpentine Floyd-Steinberg), then the date and
  temperature plates are drawn on top in pure palette colours, so the text stays crisp. The frame goes to the
  panel over SPI, which takes about 32 seconds. Refreshes are at least `display.min_refresh_seconds` apart (the
  panel wears with use), a refresh missed while the Pi was off is made up on boot, and nothing is drawn with a date
  until the clock has been set from the network (the Zero W has no clock of its own).
- **Weather.** Open-Meteo, looked up once per day for the ZIP code you gave to `geocode`. If it cannot be fetched,
  the temperatures are left off rather than shown stale.
- **HEIC.** Decoded in pure Rust ([`heic-rs`](https://github.com/tbraun96/heic-rs)), one tile at a time, straight
  into the downscaler, so a 12 or 24 MP iPhone photo needs about 10 MB of RAM. Photos it cannot stream (a crop, a
  mirror, an unusual colour matrix) use the whole-image decoder, which needs about 220 MB for 24 MP, and
  `max_pixels` (default 25 MP) keeps JPEG, PNG and that fallback within the board's memory. JPEG and PNG use the
  `image` crate; their EXIF rotation is applied after shrinking.
- **Buttons.** GPIO edge events call Home Assistant's REST API (`scene.turn_on` or `script.turn_on`) with the token
  from `secrets.env`. A call is tried again only if it never reached Home Assistant, so a toggle cannot run twice.

## Measured on a Pi Zero W (Raspberry Pi OS 13, 32-bit)

| | |
|---|---|
| Installed packages | 640 on the stock image, 534 after `01-pi-setup.sh` and `02-inky-setup.sh` (Samba adds 11) |
| RAM | 426 MB usable; about 140 MB in use on the whole system with everything running, of which the service itself is about 5 MB |
| Panel refresh | about 32 s |
| HEIC decode, 12 MP iPhone photo | 17 to 25 s once per photo, in the background, about 10 MB peak |
| Decoding a 24 MP HEIC the old whole-image way | about 30 s and 218 MB (what streaming replaced) |

Idle power has not been measured yet.

## Install on the Pi

You need a Pi Zero W (or any 32-bit Raspberry Pi OS board) running **Raspberry Pi OS Lite 32-bit** with SSH
enabled, and the display on the 40-pin header. The first three steps are done once.

```bash
# 1. Slim the OS (removes Bluetooth, camera and desktop leftovers, other models' kernels). Read the dry run first.
sudo /opt/inkyframe/setup/01-pi-setup.sh            # dry run
# 2. SPI, power-saving boot settings, the service account and the photo share. Prints the share password.
sudo /opt/inkyframe/setup/02-inky-setup.sh --apply
sudo reboot
# 3. Install (or update) the program from the latest GitHub release:
curl -fsSL https://github.com/pshanesmith/InkyGadget/releases/latest/download/install.sh | sudo bash
```

Steps 1 and 2 live in the release too, so on a brand-new Pi run the installer first (it installs the files; the
service will start once SPI is on), then the two setup scripts, then reboot. Each script is a dry run unless
given `--apply`, backs up what it changes, and never reboots by itself.

### Configure

```bash
sudo /opt/inkyframe/bin/inkyframe geocode 10001 --config /etc/inkyframe/config.toml   # your ZIP: writes [location]
sudo nano /etc/inkyframe/secrets.env      # INKYFRAME_HA_TOKEN=...  (a dedicated, non-admin Home Assistant user)
sudo nano /etc/inkyframe/config.toml      # [buttons] scene/script ids, refresh_times, units...
sudo systemctl restart inkyframe
```

Add photos by connecting to the share: **macOS** Finder > Go > Connect to Server > `smb://inky.local/Images`;
**iPhone/iPad** Files > Browse > ... > Connect to Server; **Windows** `\\inky.local\Images`. User `inkyframe`,
password in `/etc/inkyframe/share-password` (also on the frame's screen while it is empty). Replace `inky` with
your Pi's hostname. New photos are picked up within a minute; delete a file to remove it.

### Home Assistant

The buttons only call things you define in Home Assistant. Each of A, B and C is one `scene.*` or one `script.*`
entity, in any mix, or empty to switch that button off. For example:

```toml
[buttons]
a = "scene.movie_night"      # a scene: puts a set of devices into saved states
b = "script.goodnight"       # a script: runs several steps, and can use conditions and loops
c = "script.toggle_lamp"     # toggling needs a script, because a scene only sets states
d = "next_photo"             # built in: show the next photo
```

Use a script when a button should do something that depends on the current state (toggle, "only if it is
closed") or should pick its targets by area, floor or label instead of listing every device.
`inkyframe ha-discover` lists the scenes and scripts your Home Assistant has, so you can copy the ids.

Create a dedicated non-admin Home Assistant user and give its long-lived token to the frame. A token carries all
of its user's permissions.

### Operate

```bash
/opt/inkyframe/tools/health-check.sh        # one-screen summary
journalctl -u inkyframe -f                  # live log
sudo /opt/inkyframe/bin/inkyframe hwtest --buttons   # stop the service first; colour bars, then prints button presses
sudo /opt/inkyframe/setup/03-install-service.sh --uninstall
```

## Develop (on a Mac or Linux machine)

Needs Rust 1.85+. Nothing below needs the Pi.

```bash
cargo test                                                         # unit tests
cargo run --release -- preview photo.heic --out x.png --left "Thu Oct 29" --right "H 62° L 41°"
cargo run --release -- run --config dev.toml --emulate frame.png   # whole program; type a/b/c/d + Enter for buttons
tools/cross-build.sh                                               # Pi binary: dist/inkyframe-armv6 (zig, cargo-zigbuild)
tools/package-release.sh                                           # dist/inkyframe-armv6.tar.gz, .sha256, install.sh
```

The hardware driver (`--features hardware`) is Linux-only; `tools/cross-build.sh` turns it on.

## License

GPL-3.0-only. See [NOTICE](NOTICE) for the Pimoroni driver port (MIT) and other credits.

HEVC, the video format inside HEIC photos, is covered by third-party patents. The decoder crate grants a copyright
licence only, and this project grants no patent rights; for a frame you build for yourself that is for you to weigh.
