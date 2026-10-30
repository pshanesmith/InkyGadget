# InkyGadget

Photo frame: Raspberry Pi Zero W (ARMv6, 32-bit, 426 MB RAM) + Inky Impression 5.7" (UC8159, 600x448), Rust.
See README.md (use, how it works, measurements). Sibling project with the same deployment style: ../LEDMatrixGadget.

## Workflow: develop on the Mac, test on the Pi
- Rust toolchain may not be on PATH: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:/opt/homebrew/bin:$PATH"`.
- `cargo test`, and `cargo run --release -- run --config <dev.toml> --emulate out.png` runs the whole program with PNG output.
  `cargo run --release -- preview PHOTO --out x.png` renders one photo as the panel shows it.
- The `hardware` feature (spidev + gpiocdev) is Linux only. Cross-build: `tools/cross-build.sh` (target
  `arm-unknown-linux-gnueabihf.2.31`, ARMv6). It cannot be tested off the Pi; use `inkyframe hwtest` there.
- Pi: host `inky` (your own user, key login), Raspberry Pi OS 13 (Trixie) 32-bit. Deploy copy is `/opt/inkyframe` (root-owned; never edit).
  Config `/etc/inkyframe/`, state `/var/lib/inkyframe`, photos `/srv/inkyframe/Images`.
- Memory is tight (426 MB RAM) but HEIC decoding now streams tile by tile and peaks near 10 MB on the Pi (it was about 220 MB
  for 24 MP before). JPEG/PNG still decode whole (`max_pixels` caps them); do not run other big programs on the Pi.
- `cargo build` on the Mac prints a harmless "rust-objcopy failed" warning (missing llvm-tools); the Pi build is fine.
- Real family photos live outside the repo (`samples/` is gitignored). Never commit photos, tokens or `config.toml`.

## Facts that are easy to get wrong
- Pins (BCM): reset 27, busy 17, DC 22, CS 8 (driven by hand; needs `dtoverlay=spi0-0cs`), buttons A-D = 5, 6, 16, 24 (pull-up, falling edge).
- Pimoroni's driver quantises to a blend of the saturated and desaturated palettes (`display.saturation`, 0.5); previews use the measured colours.
- Overlay is drawn after dithering in palette indices (crisp text). The date format is local time; the clock must be NTP-set
  (`/run/systemd/timesync/synchronized`) or the date is skipped.
- Buttons call `scene.*`/`script.*` through the HA REST API; `next_photo` is built in. Min gap between panel refreshes: `display.min_refresh_seconds`.

## Decisions worth remembering
- **The share is polled** every 30 s, not watched with inotify: one cheap directory walk, no dependency, and the "unchanged
  between two scans" rule handles half-copied files. Samba runs only `smbd` (SMB2+, bound to lo and wlan0, `nmbd` off,
  `_smb._tcp` advertised through avahi); there is no practical pure-Rust SMB server, and it is the only extra package (11).
- **HEIC** is `heic-rs` (pure Rust, MIT/Apache), streamed per tile (`src/heic_stream.rs`, with its own YCbCr to RGB; it falls back
  to `heic_rs::decode` for crops, mirrors and unusual matrices). Rejected: `heic` (AGPL), system libheif (works, since Raspberry
  Pi's armhf repo has ARMv6 builds, but needs more packages and was not needed). Never hold a full-size image on the Pi.
- **The panel** should refresh no more often than every few minutes and at least once a day (Pimoroni); `min_refresh_seconds`
  enforces the floor, also for button D. No deep sleep after power-off (`DSLP`): unmeasurable gain, unverifiable without the panel.
- **systemd unit**: do not add `PrivateDevices`, `DevicePolicy` or `ProtectClock` (each turns the device list into an allow-list that
  excludes `/dev/spidev0.0` and `/dev/gpiochip0`; the service reaches them through the `spi` and `gpio` groups). `MemoryHigh`
  does nothing if the kernel's memory cgroup is off. The unit has no OOM bonus on purpose: it is the biggest process, so it
  should be the one restarted. `TimeoutStopSec=90` lets a stop wait for a refresh to finish.
- **cloud-init** is removed by `01-pi-setup.sh` (`--keep-cloud-init` opts out) only when first boot has finished and the
  network survives without it: Imager's Wi-Fi lives in `/etc/netplan/90-NM-*.yaml`, which NetworkManager rebuilds at every boot
  through `netplan-generator`, and apt may never remove netplan. It also deletes `/var/lib/cloud`, whose user data can hold the
  Wi-Fi password in plain text.
- **Setup scripts** are dry runs unless given `--apply`, back up what they change under `/var/backups/inkyframe-setup`, and never
  reboot. Boot settings (SPI, no Bluetooth/audio/camera/HDMI, `gpu_mem=16`) live only in `02-inky-setup.sh`.
- **Config** is validated at load (`inkyframe check-config`); the installer runs it before restarting the service, so a typo
  cannot leave the frame in a restart loop.

## Publishing
- Once per clone: `git config core.hooksPath .githooks` and `git config user.email ID+NAME@users.noreply.github.com` (the hook
  refuses a personal commit email, secrets, Home Assistant tokens, private keys and absolute paths into a home folder).
- Release: bump `version` in `Cargo.toml` (and `Cargo.lock` by building), commit, `git tag vX.Y.Z && git push origin vX.Y.Z`.
  GitHub Actions cross-builds, tests and publishes `inkyframe-armv6.tar.gz`, its `.sha256` and `install.sh`.
- On the Pi: `curl -fsSL https://github.com/pshanesmith/InkyGadget/releases/latest/download/install.sh | sudo bash`
  (`-s -- --version TAG` pins a release). There is no `--from-source`: the Zero W cannot compile this.

