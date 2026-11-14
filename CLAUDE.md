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
- **Air sensor** (`src/sensor.rs`): SCD40 via `sensirion-scd4x`, a port of Adafruit's MIT CircuitPython driver, in the sibling repo
  `pshanesmith/scd4x-rust` (a git dependency on tag v0.1.0; for driver work, temporarily point it at a local path
  `../scd4x-rust`). The sensor keeps measuring across a restart, so `init` always stops it first. The service must not run
  while `hwtest --sensor` does. I2C needs `dtparam=i2c_arm=on`, the `i2c-dev` module and the `i2c` group (all in the setup script
  and unit). Plan: expose it as a Matter Air Quality Sensor (rs-matter), pairing QR on the panel; see the plan in the session
  notes if it is still there, else the README.
- **Matter** (`src/matter.rs`, cargo feature `matter`, on in `tools/cross-build.sh`): rs-matter 0.4.1 (pure Rust, RustCrypto). Its
  cluster code is generated at build time from the Matter IDL, so Air Quality, CO2 / temperature / humidity measurement and
  Identify exist and are implemented here as `ClusterHandler`s. IP-only commissioning (`DiscoveryCapabilities::IP`), Ethernet-style
  root endpoint (the device is commissioned on the network it is already on), CSA test VID/PID 0xFFF1/0x8001 and test DAC.
  mDNS: rs-matter's built-in responder on Linux (does it coexist with avahi-daemon on the Pi? unverified; the alternative is
  rs-matter's `zbus` feature and its avahi backend), Bonjour on macOS (the built-in one cannot join multicast groups there).
  The unit needs `AF_NETLINK`. Verified on the Pi (Zero W, real SCD40): built-in mDNS coexists with avahi-daemon, and pairing
  with Home Assistant works once its Matter Server has `enable_test_net_dcl` on (it refuses the CSA test certificates otherwise).
  Cluster attributes: `with!(required)` leaves out the ones a feature adds (CO2 MeasuredValue!), so name them; a test checks.
  **Pairing screen** (`src/pairing.rs`): drawn in pure palette colours when the node reports no controller (`Event::Paired(false)`)
  and the same code is not already on the panel (`state.pairing_shown`); a photo refresh follows a minute after pairing. Start-up
  waits up to 20 s for the node's first report so the first refresh is the QR code, not welcome-then-QR. The passcode lives in
  `<state>/matter/identity.json` and cli.rs makes it before the node thread starts, so panel and node always agree. A test reads
  the QR code back out of the rendered screen (rqrr). The `matter_pairing` button action reopens the 15-minute window.
- **Matter buttons** (`src/switchlogic.rs`, `src/buttonbus.rs`, `Hub` in `src/buttons.rs`): each panel button is a Generic Switch
  (MS|MSR|MSL|MSM). The pure state machine (timestamps in, Switch events out; sequences from Core spec 1.13) runs on the
  button thread, which watches both GPIO edges; events cross to the Matter thread through a std-only per-button queue
  (`ButtonBus`) that the Switch handler's `run()` awaits and turns into `emit_event` calls (only the handler context can emit).
  Traps already hit: `with!(required)` drops `MultiPressMax` (named explicitly, a test checks); CurrentPosition must be 1 while held;
  the node's ConfigurationVersion is bumped (rs-matter keeps the number) when the layout changes (`layout.json` remembers it);
  Home Assistant only exposes `multi_press_N`/`long_press`/`long_release` when multi-press is on, so a tap is reported after
  `multi_press_ms`. The chain of handlers needs `#![recursion_limit = "256"]`.
- **systemd unit**: do not add `PrivateDevices`, `DevicePolicy` or `ProtectClock` (each turns the device list into an allow-list that
  excludes `/dev/spidev0.0` and `/dev/gpiochip0`; the service reaches them through the `spi` and `gpio` groups). `MemoryHigh`
  does nothing if the kernel's memory cgroup is off. The unit has no OOM bonus on purpose: it is the biggest process, so it
  should be the one restarted. `TimeoutStopSec=90` lets a stop wait for a refresh to finish.
- **cloud-init** is removed by `01-pi-setup.sh` (`--keep-cloud-init` opts out) only when first boot has finished and the
  network survives without it: Imager's Wi-Fi lives in `/etc/netplan/90-NM-*.yaml`, which NetworkManager rebuilds at every boot
  through `netplan-generator`, and apt may never remove netplan. It also deletes `/var/lib/cloud`, whose user data can hold the
  Wi-Fi password in plain text.
- **Wi-Fi drops** happen a few times a day (seen Oct 31 to Nov 2): the 4-way handshake fails (`reason=15`) on every access point at
  once (weak signal, 30-40%, plus the router side), NetworkManager gives up ("no secrets: no agents") and stays down. Pinning one
  BSSID would not help. `02-inky-setup.sh` (wifi step) sets `autoconnect-retries = 0` and a 2-minute watchdog timer
  (`inkyframe-wifi-watchdog`) that runs `nmcli device connect wlan0`; it recovers in 5 to 40 minutes without a reboot.
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

