# InkyGadget: plan for review

A bedside photo frame: Raspberry Pi Zero W + Pimoroni Inky Impression (7-colour e-paper), written in Rust, installed and updated the same way as [LEDMatrixGadget](https://github.com/pshanesmith/LEDMatrixGadget) (release tarball + `install.sh` + `setup/` scripts, systemd, minimal Pi). Photos arrive over an SMB share, as in [inky-photo-frame](https://github.com/mehdi7129/inky-photo-frame). Nothing has been built yet; this is for review.

## 1. What it does

| Feature | Behaviour |
|---|---|
| Photo upload | A Samba share (`\\inkyframe.local\Images`, user `inky`, random password). Drop JPEG/PNG/HEIC files in, delete them to remove. New files show up on the next rotation, or immediately if the library was empty. |
| Rotation | A new photo at each time in `refresh_times` (default `["00:00"]`, i.e. once a day at midnight), shuffled with history so nothing repeats until all have been shown. Optional `interval_minutes` for more frequent rotation (floor 5). Persisted across reboots. |
| Overlay, top left | Day and date, e.g. `Thu Oct 29`, on a small solid plate so it stays legible over dithered photo noise. |
| Overlay, top right | Today's high and low, e.g. `H 62° L 41°`. Hidden (not stale) if there is no fresh forecast. |
| Button A | Calls your "Getting ready for bed" scene/script in Home Assistant. |
| Button B | Calls your "Open bedroom shades" scene/script. |
| Button C | Calls your "Reading time" scene/script. |
| Button D | Next photo (forces a refresh outside the schedule). |
| Welcome screen | Shown while the library is empty: share address, user, password (same idea as the reference project). |

## 2. Hardware facts that shape the design

- **Pi Zero W (original)**: single-core ARMv6 at 1 GHz, 512 MB, 2.4 GHz Wi-Fi, **32-bit only**. Rust target `arm-unknown-linux-gnueabihf` (ARMv6 + hard float). Needs Raspberry Pi OS Lite **32-bit**. The reference project and LEDMatrixGadget both target newer boards, so build and image decoding are the real risks (section 9).
- **Display**: you wrote 5.57". Pimoroni's 7-colour Impression is the **5.7"**, 600x448, UC8159 controller; I'm assuming that is the one (open question 1). Colours: black, white, green, blue, red, yellow, orange. A full refresh takes about 30 s and flashes. Pimoroni advises refreshing no more often than every few minutes, and at least once a day.
- **Wiring** (to confirm against the Pimoroni library source before coding, since I could not fetch it during research): SPI0 CE0, DC GPIO22, RESET GPIO27, BUSY GPIO17, buttons A/B/C/D on GPIO5/6/16/24 (active low, A at the top).
- **No Python and no Pimoroni library on the Pi.** We port the UC8159 init sequence and the palette to Rust (the library is MIT, so credit it in `LICENSE`/`NOTICE`). That removes `numpy`, `pillow`, `spidev`, `smbus2`, `gpiod` and pip from the image. The model is set in config; the HAT EEPROM is not read, so I2C stays off.

## 3. Architecture

One static-ish binary, `inkyframe`, with subcommands, running as a single systemd service. No async runtime (plain threads, mostly asleep; idle RSS should be about 10 MB).

```
inotify watcher  --new/removed file-->  library (cache + shuffle state)
buttons (gpiocdev edge events) --press--> HA actions   (independent of the display, fires immediately)
                               \-D------> controller
timer (interval / day-start) ----------> controller ---> render ---> UC8159 over SPI
weather poller ------------------------> controller (latest high/low)
```

- **Controller** owns the display. Rotation ticks, "next photo" and library changes become events on one channel; presses during the 30 s refresh are coalesced, not queued.
- **Library**: the originals stay untouched in the share. A background worker (nice 19, one image at a time) decodes each one, applies orientation, **fits the whole photo inside 600x448 with bars** (colour configurable, default white) and stores a prepared JPEG in `/var/lib/inkygadget/cache/`, keyed by name + size + mtime. Deleting from the share removes the cache entry. A file is processed only after `CLOSE_WRITE`/`MOVED_TO`, so half-copied uploads are never read. Oversized or undecodable files are logged and skipped, never fatal.
- **Render** (per refresh): load the prepared image, dither to the measured palette (serpentine Floyd-Steinberg, integer arithmetic, a few seconds at most on ARMv6), **then** draw the overlay plates in pure palette colours with a crisp integer-scaled bitmap font. The plate is dark or light depending on the luminance of its corner. Sending the frame to the panel and waiting for BUSY is the only slow part.
- **Scheduling**: refresh at each time in `refresh_times` (default midnight, so the date and that day's high/low are current as the day starts) and optionally every `interval_minutes`. No quiet hours. The weather is fetched shortly before the refresh. The first render waits for NTP sync (no RTC on a Zero W) and shows the welcome screen, or a photo without the date, until then. No time of day is shown anywhere, since the panel refreshes about once a day.
- **Weather**: Open-Meteo by ZIP geocode, as in LEDMatrixGadget (`ureq`, no API key). Reuse that project's `weather.rs`/`geocode.rs` where possible. Units configurable (default Fahrenheit).
- **Home Assistant**: REST with a long-lived token in `/etc/inkygadget/secrets.env` (mode 600). Each button maps to a list of service calls in config (below). The HA host can be a plain-HTTP LAN address. Failures are logged, retried once, never block the display.
- **Crates (kept short)**: `anyhow`, `chrono`, `image` (jpeg, png only), `inotify`, `log`/`env_logger`, `rand`/`rand_chacha`, `serde`/`serde_json`, `signal-hook`, `toml`, `ureq`; Linux-only behind a `hardware` feature: `spidev`, `gpiocdev`. Hand-rolled argument parsing and `sd_notify` (no `clap`) to keep the binary and build small.

### 3a. HEIC support (required)

**How the reference does it:** `pillow-heif` from pip (bundled libheif + HEVC decoder). Nothing in its image code is HEIC-specific; it relies on the plugin being registered.

**Options for Rust on an ARMv6 Zero W**

| | Option | For | Against |
|---|---|---|---|
| A (primary) | Pure-Rust decoder: [`heic-rs`](https://github.com/tbraun96/heic-rs) 0.1.4 (MIT/Apache; `forbid(unsafe_code)`; zero deps without `parallel`) | Static binary, no apt packages, no C cross-compile. Handles iPhone **grid** HEIC (8x6 of 512 px tiles), `irot`/`imir`/`clap`, EXIF, 8 and 10-bit. Fuzzed. Checked against Apple's decoder (about 48 dB PSNR). | Young (0.1.x, few users). Intra-only HEVC, which is fine for stills. **No SIMD**; no 32-bit ARM claim; benchmarks are the author's, on an M3 Max. Does not obviously apply Display P3 to sRGB. HDR gain maps ignored (fine here). |
| A2 | [`heif-oxide`](https://github.com/dan335/heif-oxide) 0.1.0 (MIT/Apache) over `rust_h265` | Claims "tested on real iPhone photos"; does P3 to sRGB; 44/63 Nokia conformance files. | 2 commits, thinner testing; author says ~5x slower than FFmpeg. |
| (avoid) | `heic` by imazen (AGPL or paid commercial) | Best tested (49/49 HEVC vectors), NEON | Licence, AI-generated-code notice; SIMD helps nothing on ARMv6. |
| B (fallback) | System `libheif` via `libheif-rs` (MIT) + `libde265` | Mature, faster C decoder | Adds `libheif1` + plugin packages. Debian's armhf builds target ARMv7, so they may crash with "illegal instruction" on a Zero W unless Raspberry Pi OS ships ARMv6 rebuilds (unconfirmed for Trixie). Cross-linking needs an armhf sysroot. The crate's static `embedded-libheif` mode does not bundle libde265 or the other codecs. |
| C | Convert on the user's side | Trivial | Defeats "drop photos in the share". |

**Plan:** make decoding a trait (`Decoder::decode_to_fit(path, target) -> RgbImage`) so the format is irrelevant to the rest of the app. Ship **A**, with **A2** tried first if A fails on the spike files, and keep **B** behind a cargo feature only if neither is usable. JPEG/PNG stay on the `image` crate.

**Design points that matter more than the crate:**
- **Never hold the full image.** A 12 MP iPhone photo is about 36 MB as RGB, but newer iPhones can save 48 MP HEIC (8064x6048 is about 146 MB RGB plus YUV), which would stress 512 MB. Decode tile by tile and downscale each tile straight into the 600x448 target; this needs the crate (or our own thin container parser) to expose per-tile decoding. That is a spike question, not an assumption.
- **Decode once, in the background**, at nice 19, with `max_pixels` and a time limit; cache the result. Speed is unmeasured: the authors' figures (about a second for 12 MP on a fast desktop with all cores) say nothing about a 1 GHz single-core ARMv6 without SIMD, where I'd expect tens of seconds to minutes per photo. That is acceptable for a one-time background cost (photos appear as they finish; the welcome screen can show "processing n of m"), but a bulk drop of hundreds of photos would take hours, so the spike must measure it.
- **Colour**: iPhone HEIC is usually Display P3. Convert to sRGB ourselves if the decoder doesn't (small effect once quantised to 7 colours, but cheap to do right).
- **Orientation**: apply `irot`/`imir` (HEIC) or EXIF (JPEG) exactly once.
- **Failure is isolated**: an unsupported or corrupt HEIC is logged and skipped, never fatal. Image sequences (`.heics`) and overlays are out of scope.
- **Patents**: HEVC is patent-encumbered; the crates grant copyright licences only. For a private home device that is your call, noted here for completeness.

**Acceptance test (spike 0d):** a set of real files from your phone (a normal photo, a portrait-mode photo, a Live Photo still, a panorama, a 48 MP file if you have one), decoded on the actual Zero W: correct colours and orientation, peak RSS under about 150 MB, time per photo recorded. If A and A2 both fail it, B becomes the plan and the Pi-OS ARMv6 question gets answered first.

### Subcommands
`run` (the service) · `preview` (renders a photo with overlay and palette to PNG on the Mac, no hardware) · `geocode ZIP` · `ha-discover` (lists lights, locks, covers, scenes, scripts, areas and prints a config snippet) · `hwtest` (colour bars, button test) · `credentials` (show or reset the share password).

## 4. Home Assistant buttons

You maintain the scenes/scripts in HA; the Pi only links buttons to them. Each button maps to one entity (a `scene.*` or `script.*`), and the Pi calls `scene.turn_on` or `script.turn_on` on it:

```toml
[buttons]
a = "scene.getting_ready_for_bed"
b = "scene.open_bedroom_shades"
c = "script.reading_time"          # toggle needs a script: a scene can't toggle
d = "next_photo"                   # built-in action, no HA call
```

`ha-discover` lists the scenes and scripts so you can paste the right IDs. One caveat for you when you write them: a scene sets states, it cannot toggle, so "Reading time" is best a script (`light.toggle` on the bedside lights); "open the shades if closed" is safe as a scene or script because opening an open cover does nothing. Presses fire immediately, independent of the display refresh; failures are logged, retried once and never block the display.

**Security note:** a long-lived token carries all of its user's permissions. Create a dedicated, non-admin HA user for the frame.

## 5. Pi setup, minimal power and packages

Same three-script shape as LEDMatrixGadget (dry run by default, `--apply`, idempotent, backs up what it edits, never reboots):

1. `setup/01-pi-setup.sh`: purge unused packages (Bluetooth, USB-Wi-Fi firmware, camera tools, desktop utilities, `gdb`, `triggerhappy`, `modemmanager`, and so on) with the same simulate-first protect-list safety net; remove `cloud-init`/Connect only under `--aggressive`. Disable `man-db`, `e2scrub`, `NetworkManager-wait-online`, Bluetooth units. Turn the ACT LED off. Persistent journal capped at 30 MB. Optional unattended upgrades with the 04:00 reboot rule.
2. `setup/02-inky-setup.sh`: boot config block: `dtoverlay=disable-bt`, `dtparam=audio=off`, `camera_auto_detect=0`, `display_auto_detect=0`, `gpu_mem=16`, no KMS/HDMI overlay for a headless board, `disable_splash=1`, `dtparam=spi=on`, plus `dtoverlay=spi0-0cs` if the chip-select conflict Pimoroni documents applies. Wi-Fi power-save on via a NetworkManager drop-in (costs some latency on the first button press after idle; `--no-wifi-powersave` opts out). CPU governor stays `ondemand`/`powersave`; I'll measure before touching clocks. Creates the `inky` user (groups `spi`, `gpio`) so the service runs unprivileged, installs Samba and writes `smb.conf`.
3. `setup/03-install-service.sh`: installs into `/opt/inkygadget`, creates `/etc/inkygadget` (config, secrets, credentials), `/var/lib/inkygadget`, the share directory, the systemd units, starts the service; `--uninstall`. Called by `install.sh` (`curl ... | sudo bash`, `--version`, `--dry-run`, checksum verified) after downloading a GitHub release built for `arm-unknown-linux-gnueabihf`.

**Samba, the one real package cost.** Debian's `samba` pulls in a few dozen packages (including `python3-samba`), far more than the rest of this project combined. Mitigations: install with `--no-install-recommends`; run only `smbd` (disable `nmbd`, `winbind`, `samba-ad-dc`); advertise `_smb._tcp` through the existing `avahi-daemon` (keeps `inkyframe.local` working). Config: SMB2+ only (no SMB1), `map to guest = never`, one share, bound to `wlan0`, no printing, no usershares, the Samba user mapped to the `inky` Unix user so uploaded files are owned correctly. The first spike counts the exact packages and RAM so you can see the cost before we commit; there is no practical pure-Rust SMB server, so Samba is the way to match the reference project.

Service unit: `Type=notify` with watchdog, `Restart=always`, `OOMScoreAdjust=-500`, `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, writable only in `/var/lib/inkygadget` and the share. No root needed (SPI and GPIO through device groups).

Targets to report after the soak, not promises: installed package count before/after, idle RSS, idle current at the USB supply with a USB meter, behaviour over a week.

## 6. Repository layout

```
Cargo.toml  rust-toolchain  LICENSE (GPL-3.0, matching LEDMatrixGadget)  NOTICE (Pimoroni MIT)
CLAUDE.md README.md DESIGN.md install.sh
src/   main.rs (subcommands) config.rs controller.rs library.rs watcher.rs render/ (palette, dither, overlay, font)
       display/ (uc8159 driver, emulator) buttons.rs ha.rs weather.rs geocode.rs state.rs
config/ config.example.toml  secrets.env.example      setup/ 01 02 03 + lib/common.sh
systemd/ inkyframe.service  tools/ cross-build.sh package-release.sh health-check.sh
tests/  golden images, setup-script tests          .github/workflows/ ci.yml release.yml
```

## 7. Development workflow

Develop and test on the Mac: `cargo test`, and `inkyframe preview photo.jpg --out x.png` shows exactly what the panel would show (palette-quantised, with overlay); golden-image tests catch rendering changes. The hardware backend is Linux-only and cross-built with `zig` + `cargo-zigbuild` for ARMv6; CI builds the release on tag. The SPI/GPIO paths can only be verified on the Pi, so `hwtest` exists for that.

## 8. Milestones

0. **Spikes (before real code)**: (a) cross-build `ureq`+`rustls`+`image` for ARMv6 and run on the Zero W (the `ring` crypto crate is the likely trouble); (b) read the Pimoroni UC8159 driver for the init sequence, palette, pins; (c) count Samba's packages and RAM; (d) **HEIC first**: decode your real HEIC files with `heic-rs` and `heif-oxide` on the Zero W and on the Mac (time, peak RAM, colour, orientation, per-tile API), plus a 12 MP JPEG for comparison; (e) confirm what package repository Raspberry Pi OS Trixie 32-bit uses on a Zero W (ARMv6 rebuilds or Debian's ARMv7 armhf), which decides whether fallback B is even possible.
1. Renderer on the Mac: fit/crop, palette, dither, overlay, preview, goldens.
2. Hardware: UC8159 driver, `hwtest` colour bars, buttons.
3. Library: share watcher, cache, shuffle with history, scheduler (`refresh_times`), welcome screen.
4. Weather and date overlay live.
5. Home Assistant actions and `ha-discover`.
6. Setup scripts, installer, release CI.
7. Soak on the real device, power measurement, docs.

## 9. Risks

- **ARMv6 build**: C-backed crates (`ring`) may not cross-build cleanly. Fallbacks: fetch weather over plain HTTP from a LAN-side source, or use HA's weather entity (no TLS needed at all).
- **Image decode speed and RAM** on a 1 GHz single core: mitigated by decoding once at upload time in the background, size limits, and the cache.
- **HEIC (required)**: see section 3a. The reference project gets it from the Python wheel `pillow-heif` (which bundles libheif); Rust has no equivalent, so this is the largest technical risk and is spiked first.
- **Samba footprint** (section 5) and attack surface on the LAN: hardened config, strong random password, LAN-only.
- **Display lifespan and ghosting**: bounded by the schedule (default once a day, floor of 5 minutes between refreshes enforced in config, which also limits button D).
- **Buttons give no on-screen feedback** (a refresh takes 30 s). HA actions are logged; if the board has a user LED I'll blink it (to verify).

## 10. Decisions (answered)

1. Display: Inky Impression 5.7" (600x448, 7-colour).
2. Button D: next photo.
3. Buttons only link to HA scenes/scripts that you set up.
4. Portrait photos: fit whole photo with bars (no cropping).
5. Schedule: refresh at midnight, about once a day; no quiet hours.
6. OS: Raspberry Pi OS Lite 32-bit.
7. Samba share for uploads, accepted package cost.
8. Fahrenheit, no weather icon, no time display.

Still needed from you at the relevant milestones: HEIC sample files and Zero W SSH access (spikes), HA URL and scene/script entity IDs, ZIP code.

## 11. Spike results so far (Mac only; nothing run on a Zero W yet)

Code is in `spikes/` (`spikes/ref/` holds Pimoroni's MIT-licensed UC8159 driver and `auto.py` for reference; `spikes/armv6/` is a throwaway cross-build test).

| Spike | Result |
|---|---|
| **Pimoroni driver (b)** | Read in full. **Pins confirmed:** RESET GPIO27, BUSY GPIO17, DC GPIO22, CS GPIO8 driven by hand (SPI `no_cs`), SPI0 at 3 MHz. Buttons A/B/C/D = GPIO5/6/16/24, pull-up, falling edge, A at the top. EEPROM variant 14 = 5.7" 600x448. Init sequence (TRES, PSR `(3<<6)\|0x2F, 0x08`, PWR, PLL 0x3C, TSE, CDI, TCON 0x22, DAM, PWS 0xAA, PFS), then per frame DTM1 (two 4-bit pixels per byte) -> PON -> DRF (wait up to 32 s) -> POF. Palette: a "desaturated" and a "saturated" set, blended by `saturation` (default 0.5); the colour index 0-6. Everything needed for a Rust port is in these 459 lines. No Pi 5 only quirks except `dtoverlay=spi0-0cs` guidance. |
| **ARMv6 build (a)** | `cargo zigbuild --target arm-unknown-linux-gnueabihf.2.31` builds `ureq` (rustls, i.e. `ring`), `image` (JPEG/PNG), `heic-rs`, `spidev`, `gpiocdev` and `inotify` in 21 s. Result: 2.2 MB stripped ELF, ARM EABI5, hard-float; the ARM attributes say **ARMv6, VFPv2**. The HTTPS fear (ring on ARMv6) did not materialise at build time. Still unproven at run time on the Zero W. `inotify`/`spidev`/`gpiocdev` are Linux-only, so they sit behind `cfg(target_os = "linux")` for Mac development. |
| **HEIC (d), Mac** | `heic-rs` 0.1.4 (`default-features = false`, single-threaded) decoded real macOS-made HEICs correctly: a 2048x1536 in 62-70 ms, a 4032x3024 (12 MP, 512 px tile grid like an iPhone's) in 207 ms with 91 MB peak RSS. Colours match the source JPEG visually. **Caveats:** the files were made by `sips` from a wallpaper, not iPhone photos; the Mac figures say nothing about a 1 GHz ARMv6 without SIMD (if it is 50-150x slower, a 12 MP photo is roughly 10-30 s, a guess to be measured); 91 MB for 12 MP means a 48 MP file would need about 360 MB, so the streaming design is needed. |
| **Per-tile decoding** | Feasible: `Context`, `Context::grid`, `item_data`, `props` and `hevc::decode_still` are public, so we can decode one 512 px tile at a time, convert it, and area-average it straight into the 600x448 target without ever holding the full image. Not yet written. |
| **Samba (c)** | Debian Trixie `samba` depends on about 20 packages (libs such as `libcups2t64`, `libldap2`, `libgnutls30t64`, `liburing2`, `samba-common`, `samba-common-bin`, `samba-libs`). Its Recommends (`python3-samba`, `samba-ad-dc`, `attr`) are skipped with `--no-install-recommends`. Exact package count and RAM still need `apt-get -s` on the Pi. |

### Not yet done (need your input or the Zero W)
- Real iPhone HEIC files (portrait mode, Live Photo, 48 MP) decoded on the Zero W: actual speed and peak RAM.
- Confirm the package repository Trixie 32-bit uses on a Zero W (ARMv6 rebuilds or Debian ARMv7).
- Run the ARMv6 spike binary on the Zero W (proves HTTPS and the device nodes work there).
- `apt-get -s install --no-install-recommends samba` on the Pi for the exact package count.

### 11a. On-device results (Zero W Rev 1.1, Raspbian 13 Trixie, kernel 6.18 `rpi-v6`)

| Item | Result |
|---|---|
| Package repository | `raspbian.raspberrypi.com` armhf (ARMv6 rebuilds) plus `archive.raspberrypi.com`. So libheif (fallback B) would work if ever needed. |
| Baseline | 640 packages, 426 MB RAM, 425 MB zram swap, 12 GB free, governor `ondemand`, Wi-Fi power save already on, avahi-daemon and python3 present, 35 enabled unit files. |
| Boot config as shipped | `vc4-kms-v3d`, audio on, camera/display auto-detect on, `max_framebuffers=2`, **SPI off** (no `/dev/spidev*`). All to be changed by `02-inky-setup.sh`. |
| GPIO | One chip: `/dev/gpiochip0` (`20200000.gpio`, `pinctrl-bcm2835`, 54 lines); `gpiochip4` is a symlink to it. Group `gpio` can access it. |
| Spike binary | Runs on the Zero W. **HTTPS to Open-Meteo returns 200** (rustls works on ARMv6). |
| **HEIC decode, `heic-rs`** | 2048x1536 in **3.9 s**; 4032x3024 (12 MP) in **12.5 s**, peak RSS **74 MB**. Much better than the 10-30 s guess and fine for background processing. Test files are `sips`-made, not iPhone HEICs; real iPhone files are still to be tried (bitrate and 48 MP). The decode ran without the `parallel` feature, on the single core. |
| **Samba (simulated install, no recommends)** | **11 new packages**: `samba`, `samba-common`, `samba-common-bin`, `samba-libs`, `libtdb1`, `libtevent0t64`, `libldb2`, `libcups2t64`, `libavahi-client3`, `libncurses6`, `liburing2`. Far smaller than feared. Disk size not captured. |

Remaining before coding: real iPhone HEIC files (portrait, Live Photo, 48 MP), and the Samba disk and RAM figures once installed.

### 11b. Real iPhone HEIC results (3 files from your phone)

| File | Size | Mac (M-series, 1 thread) | **Zero W** | Peak RSS on Zero W |
|---|---|---|---|---|
| IMG_0362.HEIC (portrait, 5712x4284 = 24 MP, 2.3 MB) | rotated correctly to 4284x5712 | 0.40 s | **29.6 s** | **218 MB** |
| IMG_1678.HEIC (portrait, 12 MP, 2.5 MB) | rotated to 3024x4032 | 0.22 s | **13.7 s** | 110 MB |
| IMG_1817.HEIC (landscape, 12 MP, 3.2 MB) | 4032x3024 | 0.25 s | **13.8 s** | 76 MB |

- All three decoded without errors on the first try, orientation (`irot`) was applied, and the colours matched the photos. No unsupported-tool errors on real iPhone HEVC streams, including the 24 MP file.
- **HEIC is feasible with the pure-Rust decoder; fallback B (libheif) is not needed for these files.**
- Speed is fine for a background, decode-once design: about 14 s per 12 MP and 30 s per 24 MP photo, so 100 photos is roughly 25 to 50 minutes the first time, with the frame still working meanwhile.
- **Memory is the remaining constraint.** The 24 MP file peaked at 218 MB on a 426 MB board that already uses about 145 MB, before Samba. A 48 MP file would need about 4x that, so the per-tile streaming decode stays in the plan, with a `max_pixels` cap as a safety net (files over the cap are skipped and logged, never crash the service). The decode must not run while another large allocation is live (one image at a time).
- IMG_0362 is a **Live Photo** still, so that case is covered: the HEIC holds only the still (the motion clip is a separate `.MOV`, which the frame ignores).
- Not yet tested: a 48 MP file and a portrait-mode photo with depth data. Test on the Pi when available.

## 12. Build status

Built and tested on the Mac (49 unit tests, clippy clean, whole program runs with `--emulate`):
renderer (box-filter scaling, Floyd-Steinberg dither to the blended palette, overlay plates drawn after dithering),
JPEG/PNG/HEIC decoding with a per-photo cache, shuffle with history, schedule (midnight by default, missed-refresh
catch-up after a power loss), Open-Meteo weather, ZIP geocoding, Home Assistant scene/script calls, welcome screen,
UC8159 driver and GPIO buttons (compile for ARMv6, **not yet run on the panel**), systemd unit, `install.sh`,
`setup/01..03`, release packaging and CI workflows (untested until the first tag).

Changes from the plan: the share folder is polled every 30 s (a file counts once it stops changing) instead of using
inotify, which removes a dependency and the half-copied-file problem; the service account and share login are both
`inkyframe`; boot settings live only in `02-inky-setup.sh`.

Dry runs of `01-pi-setup.sh` and `02-inky-setup.sh` on the Zero W look right (01 would remove 124 packages; 02 adds SPI,
the service account, Samba with 11 packages, and Wi-Fi power save). **Not yet applied.**

Next: apply the setup scripts on the Pi and reboot, `inkyframe hwtest` (colour bars, buttons), install the release,
copy photos through the share, check the first real refresh, measure idle power and packages after.


## 13. Code review and hardening (30 Oct 2026)

An independent Opus review read the whole tree. Everything it found was fixed or consciously left out:

- **Config typo can no longer crash-loop the service**: `overlay.date_format` is validated at load, and date formatting cannot panic.
- **A panic in a decoder** is now one failed photo (`catch_unwind` per photo, a drop guard clears the worker flag), not a worker that never runs again.
- **Cache files are written crash-safely** (`fsutil::write_atomic`: flush the file, rename, flush the directory); a cache file that still will not load is deleted so it is decoded again, and another photo is tried at once.
- **Stopping during a refresh**: signals set a stop flag that ends the clock and weather waits early; `TimeoutStopSec=90`; opening the panel pulses RESET, so a refresh cut short by a kill leaves nothing powered.
- **Memory**: HEIC now streams (see below); `max_pixels` 25 MP and also a decoded-bytes limit (16-bit PNGs); JPEG orientation is applied after shrinking; no negative `OOMScoreAdjust` (it protected the process that causes memory pressure), `MemoryHigh=300M` instead.
- **Scheduling**: no 1 Hz spin when an interval is shorter than `min_refresh_seconds`; a refresh time inside a DST gap is moved to just after it; a failed panel update is retried after 2 minutes (5 tries) with the schedule untouched; the date is left off while the clock is not set.
- **Library**: one folder walk and one cache listing per scan; "settled" is only advanced by `rescan`; the worker records failures itself so a bad photo is never decoded twice.
- **Home Assistant**: retried only when the request never reached the server (a timeout or error answer is not retried, so a toggle cannot run twice).
- **Scripts**: the purge confirmation comes after the list of packages; the checksum message no longer claims to detect tampering; the time zone is shown by the verification step.
- **Hardening**: more `Protect*`/`Restrict*` options, `ReadOnlyPaths` for the photos, empty capability set.
- **Smaller and faster**: scaler bins are precomputed (no per-pixel 64-bit divide on ARMv6), unused dependencies removed, fat LTO, one codegen unit, heartbeat every 60 s.
- **CI** also lints the `hardware` feature.

### HEIC streaming (measured on the Pi Zero W)

`heic_stream` decodes a HEIC one tile at a time through the public `heic-rs` API and adds each tile to the downscaler, with its own fixed-point YCbCr to RGB conversion (BT.601/709/2020/240M, limited and full range). Plain rotations become an EXIF-style orientation applied after shrinking; crops, mirrors, other matrices and uneven tiles fall back to the whole-image decoder. It agrees with the whole-image decoder on real photos (mean difference under 1.5 of 255).

| Photo (from the share) | File size | Peak RSS now | Time |
|---|---|---|---|
| IMG_1608.HEIC | 2.4 MB | 10.4 MB | 17.0 s |
| IMG_2351.HEIC | 2.6 MB | 10.6 MB | 18.8 s |
| IMG_1025.heic | 3.7 MB | 11.8 MB | 24.8 s |
| IMG_2246.heic | 1.8 MB | 9.8 MB | 17.3 s |

The whole-image decode measured 218 MB for a 24 MP photo (section 11b); the time is about the same as before.

A 48 MP HEIC would now only need CPU time, not memory, if `max_pixels` is raised above its 25 MP default; keep the default unless every photo is a streamable HEIC, because a JPEG, a PNG and the whole-image fallback decode (crops, mirrors, unusual colour matrices) still need 3 to 4 bytes a pixel.

### Not done, on purpose

- **Panel deep sleep after power-off** (`DSLP`): the gain is unmeasurable (the panel draws almost nothing after POF) and it cannot be verified without the hardware.
- **inotify** instead of a 30 s scan: the scan is now one directory walk; the Samba share is on a local SD card, so the cost is negligible.
