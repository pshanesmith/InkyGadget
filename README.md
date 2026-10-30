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

See [PLAN.md](PLAN.md) for the design decisions and the measurements behind them.

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
