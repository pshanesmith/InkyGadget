# InkyGadget

Bedside photo frame: Raspberry Pi Zero W (ARMv6, 32-bit, 426 MB RAM) + Inky Impression 5.7" (UC8159, 600x448), Rust.
See README.md (use) and PLAN.md (decisions, measurements). Sibling project with the same deployment style:
../LEDMatrixGadget.

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

## Publishing
- Once per clone: `git config core.hooksPath .githooks` and `git config user.email ID+NAME@users.noreply.github.com` (the hook
  refuses a personal commit email, secrets, Home Assistant tokens, private keys and absolute paths into a home folder).
- Release: bump `version` in `Cargo.toml` (and `Cargo.lock` by building), commit, `git tag vX.Y.Z && git push origin vX.Y.Z`.
  GitHub Actions cross-builds, tests and publishes `inkyframe-armv6.tar.gz`, its `.sha256` and `install.sh`.
- On the Pi: `curl -fsSL https://github.com/pshanesmith/InkyGadget/releases/latest/download/install.sh | sudo bash`
  (`-s -- --version TAG` pins a release). There is no `--from-source`: the Zero W cannot compile this.

