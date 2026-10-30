#!/usr/bin/env bash
# Slim down the Pi Zero W, cut power use and update it.
#
#   ./setup/01-pi-setup.sh                 dry run (default): shows everything, changes nothing
#   sudo ./setup/01-pi-setup.sh --apply    do it (asks you to type 'yes' first)
#
# cloud-init (first-boot provisioning that Raspberry Pi Imager uses) is removed too, but only if first boot has finished
# and the network settings no longer depend on it (netplan files and the netplan generator are in place).
#
# Options:  --keep-cloud-init   do not remove cloud-init
#           --no-auto-updates   do not set up unattended OS upgrades
#           --yes          skip the confirmation prompt
#           --only a,b     run only these steps   --skip a,b   skip these steps
# Steps:    purge system update autoupdate verify   (the boot settings are in 02-inky-setup.sh)
# Never reboots; reboot yourself afterwards. Safe to run repeatedly.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/common.sh
source "$HERE/lib/common.sh"

BOOT_DIR=${BOOT_DIR:-/boot/firmware}
ALL_STEPS=(purge system update autoupdate verify)
KEEP_CLOUD_INIT=0; ASSUME_YES=0; AUTO_UPDATES=1; ONLY=""; SKIP=""

while (( $# )); do
  case $1 in
    --apply) DRY_RUN=0 ;;
    --keep-cloud-init) KEEP_CLOUD_INIT=1 ;;
    --yes) ASSUME_YES=1 ;;
    --no-auto-updates) AUTO_UPDATES=0 ;;
    --only) ONLY=$2; shift ;;
    --skip) SKIP=$2; shift ;;
    -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
  shift
done

want() {
  local s=$1
  [[ -z $ONLY || ,$ONLY, == *,$s,* ]] && [[ ,$SKIP, != *,$s,* ]]
}

# --- package lists -------------------------------------------------------------
# Only packages that are actually installed are touched. Reasons are for the reader.
PURGE_CORE=(
  bluez bluez-firmware bluez-obexd pi-bluetooth            # Bluetooth is not used (power)
  linux-image-rpi-v7 linux-image-rpi-v7l linux-image-rpi-2712 linux-image-rpi-v8  # kernels for other Pi models; the Zero W runs rpi-v6
  linux-headers-rpi-v6 linux-headers-rpi-v7 linux-headers-rpi-v7l linux-headers-rpi-2712 linux-headers-rpi-v8   # no kernel modules are built
  firmware-atheros firmware-mediatek firmware-realtek firmware-libertas  # USB Wi-Fi dongles; this board uses brcm80211
  rpi-eeprom                                               # Pi 4/5 bootloader updater; the Zero W has no EEPROM
  rpicam-apps-lite v4l-utils                               # camera tools; no camera attached
  mkvtoolnix p7zip-full ntfs-3g libmtp-runtime udisks2   # unused desktop-style utilities
  gdb apt-listchanges pciutils usb-modeswitch wireless-tools manpages-dev
  triggerhappy modemmanager
  rpi-connect-lite                                         # Raspberry Pi Connect remote access; this frame is reached over SSH
  rpi-usb-gadget                                           # USB gadget (Ethernet over USB) mode, off by default
)
# First-boot provisioning (user, SSH, Wi-Fi from Raspberry Pi Imager). It has already run; its leftovers in /var/lib/cloud
# can hold the Wi-Fi password in plain text. Removed only when cloud_init_removable says the network no longer needs it.
PURGE_CLOUD_INIT=(cloud-init rpi-cloud-init-mods)
# apt must never remove any of these (or anything matching), or the run aborts.
PROTECT_RE='^(openssh-.*|ssh|network-manager|wpasupplicant|firmware-brcm80211|raspi-firmware|raspi-config|raspi-utils|raspberrypi-.*|userconf-pi|avahi-daemon|sudo|systemd.*|libsystemd.*|dbus.*|udev|libc6|apt|dpkg|bash|coreutils|login|passwd|initramfs-tools|linux-image-rpi-v6|linux-image-[0-9].*-rpi-v6|python3|python3-minimal|python3-gpiozero|python3-rpi-lgpio|python3-libgpiod|python3-spidev|python3-smbus2|samba.*|libsmbclient.*|libwbclient.*|curl|ca-certificates|cron|iproute2|iputils-ping|nftables|rsync|dhcpcd-base|netplan.*|libnetplan.*|python3-netplan|base-files|netbase|init|kmod|sed|grep|tar|gzip|libnss-.*|libpam.*)$'

DISABLE_UNITS=(e2scrub_all.timer e2scrub_reap.service man-db.timer NetworkManager-wait-online.service hciuart.service bluetooth.service)

# --- cloud-init ------------------------------------------------------------------
# Wi-Fi set up in Raspberry Pi Imager is written by cloud-init into /etc/netplan, and NetworkManager rebuilds its
# connections from those files at every boot (netplan-generator). Removing cloud-init keeps that working, but only if
# those pieces exist. Returns 1 with CLOUD_REASON set when it is not safe.
cloud_init_removable() {
  CLOUD_REASON=""
  if [[ ! -e /var/lib/cloud/instance/boot-finished ]]; then
    CLOUD_REASON="first boot has not finished (no /var/lib/cloud/instance/boot-finished)"; return 1
  fi
  local nm_profiles=0 netplan_files=0
  compgen -G "/etc/NetworkManager/system-connections/*" >/dev/null && nm_profiles=1
  compgen -G "/etc/netplan/*.yaml" >/dev/null && netplan_files=1
  if (( netplan_files )) && ! { installed netplan-generator || installed netplan.io; }; then
    CLOUD_REASON="/etc/netplan has settings but the netplan generator is not installed"; return 1
  fi
  if (( ! nm_profiles && ! netplan_files )); then
    CLOUD_REASON="no saved network settings found outside cloud-init (/etc/netplan, NetworkManager)"; return 1
  fi
  return 0
}

# --- steps -----------------------------------------------------------------------
step_purge() {
  step "Remove unneeded packages"
  local list=("${PURGE_CORE[@]}") present=() p removed=() blocked=() sim
  # apt never auto-removes versioned kernel packages, so name the Pi 5 kernel and all
  # kernel headers explicitly. The v6 kernel image (this board's, and the running one) is kept.
  while read -r p; do list+=("$p"); done < <(dpkg-query -W -f='${Package}\n' \
      'linux-image-*-rpi-v7' 'linux-image-*-rpi-v7l' 'linux-image-*-rpi-v8' 'linux-image-*-rpi-2712' 'linux-headers-*-rpi-*' 'linux-base-*-rpi-2712' 2>/dev/null || true)
  local cloud_purge=0
  if (( ! KEEP_CLOUD_INIT )) && installed cloud-init; then
    if cloud_init_removable; then
      list+=("${PURGE_CLOUD_INIT[@]}"); cloud_purge=1
    else
      warn "keeping cloud-init: $CLOUD_REASON"
    fi
  fi
  for p in "${list[@]}"; do if installed "$p"; then present+=("$p"); fi; done
  if (( ${#present[@]} == 0 )); then info "nothing to remove"; return 0; fi
  info "requested (installed): ${present[*]}"

  sim=$(apt-get -s -o Debug::NoLocking=1 purge --auto-remove "${present[@]}" 2>&1) \
    || { echo "$sim"; die "apt could not simulate the removal"; }
  mapfile -t removed < <(awk '/^(Remv|Purg) /{print $2}' <<<"$sim")
  for p in "${removed[@]}"; do if [[ $p =~ $PROTECT_RE ]]; then blocked+=("$p"); fi; done
  if (( ${#blocked[@]} )); then
    die "removal would take out protected package(s): ${blocked[*]}. Nothing was changed."
  fi
  info "apt will remove ${#removed[@]} packages in total:"
  printf '%s ' "${removed[@]}" | fold -s -w 100 | sed 's/^/    /'; echo
  # The first prompt came before this list existed: ask again now that it is on screen.
  confirm "The packages listed above will be removed."

  run env DEBIAN_FRONTEND=noninteractive apt-get -y purge --auto-remove "${present[@]}"
  local rc; rc=$(dpkg -l | awk '/^rc/{print $2}' | tr '\n' ' ')
  if [[ -n $rc ]]; then run dpkg --purge $rc; fi
  if (( cloud_purge )); then
    # What cloud-init saved on first boot, including the Imager's user data (it can hold the Wi-Fi password).
    run rm -rf /var/lib/cloud /var/log/cloud-init.log /var/log/cloud-init-output.log
  fi
}

step_system() {
  step "Services, LED, journal size"
  local u
  for u in "${DISABLE_UNITS[@]}"; do
    if unit_exists "$u"; then run systemctl disable --now "$u"; else echo "  skip (not installed): $u"; fi
  done

  write_file /etc/systemd/system/led-off.service '[Unit]
Description=Turn off the ACT LED (saves a little power, no blinking in a living space)

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c "echo none > /sys/class/leds/ACT/trigger; echo 0 > /sys/class/leds/ACT/brightness"

[Install]
WantedBy=multi-user.target'
  if [[ -d /sys/class/leds/ACT ]]; then
    run systemctl daemon-reload
    run systemctl enable --now led-off.service
  fi

  # Raspberry Pi OS ships 40-rpi-volatile-storage.conf (Storage=volatile), so logs vanish on every
  # reboot. A drop-in whose name sorts later wins. Persistent logs are essential for diagnosing
  # hangs and power loss on an always-on display; the size cap limits SD-card wear.
  write_file /etc/systemd/journald.conf.d/inkyframe.conf '[Journal]
Storage=persistent
SystemMaxUse=30M
RuntimeMaxUse=15M'

  run systemctl restart systemd-journald
}

step_update() {
  step "Update packages"
  if (( DRY_RUN )); then
    echo "  [dry-run] apt-get update  (skipped; simulating with the current package lists)"
    apt-get -s -o Debug::NoLocking=1 full-upgrade | awk '/^Inst /{n++} END{print "  would upgrade/install " n+0 " packages"}'
    return 0
  fi
  export DEBIAN_FRONTEND=noninteractive
  run apt-get update
  run apt-get -y -o Dpkg::Options::=--force-confold full-upgrade
  run apt-get clean
}

step_autoupdate() {
  step "Automatic OS updates (unattended-upgrades)"
  if (( ! AUTO_UPDATES )); then echo "  skipped (--no-auto-updates)"; return 0; fi

  if ! installed unattended-upgrades; then
    run env DEBIAN_FRONTEND=noninteractive apt-get -y install unattended-upgrades
  else
    echo "  unattended-upgrades already installed"
  fi

  # Which repositories may upgrade themselves: Debian stable, its security and stable-updates suites, and the
  # Raspberry Pi repository (kernel, firmware, raspi tools). Never a new Debian release. The '#clear' line
  # replaces the stock list instead of adding to it.
  write_file /etc/apt/apt.conf.d/52inkyframe-unattended-upgrades '// Managed by setup/01-pi-setup.sh
#clear Unattended-Upgrade::Origins-Pattern;
Unattended-Upgrade::Origins-Pattern {
    "origin=Debian,codename=${distro_codename},label=Debian";
    "origin=Debian,codename=${distro_codename}-security,label=Debian-Security";
    "origin=Debian,codename=${distro_codename}-updates,label=Debian";
    "origin=Raspberry Pi Foundation,codename=${distro_codename},label=Raspberry Pi Foundation";
};

// Keep our edited config files; clean up after upgrades.
Dpkg::Options { "--force-confdef"; "--force-confold"; };
Unattended-Upgrade::MinimalSteps "true";
Unattended-Upgrade::Remove-Unused-Kernel-Packages "true";
Unattended-Upgrade::Remove-New-Unused-Dependencies "true";
Unattended-Upgrade::Remove-Unused-Dependencies "true";
Unattended-Upgrade::SyslogEnable "true";

// A kernel or firmware update needs a reboot: do it at 04:00, but never while someone is logged in
// (an open SSH session postpones it to the next night). The display is dark for about a minute.
Unattended-Upgrade::Automatic-Reboot "true";
Unattended-Upgrade::Automatic-Reboot-WithUsers "false";
Unattended-Upgrade::Automatic-Reboot-Time "04:00";'

  # Without these, the daily apt timers do nothing at all.
  write_file /etc/apt/apt.conf.d/20auto-upgrades 'APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";'

  # Upgrade at about 03:00 so the 04:00 reboot window follows it (the default is 06:00 plus up to an hour).
  write_file /etc/systemd/system/apt-daily-upgrade.timer.d/inkyframe.conf '[Timer]
OnCalendar=
OnCalendar=*-*-* 03:00
RandomizedDelaySec=20m'
  run systemctl daemon-reload
  run systemctl enable --now apt-daily.timer apt-daily-upgrade.timer


  if (( ! DRY_RUN )); then
    echo "  checking the configuration:"
    unattended-upgrade --dry-run -d 2>&1 | grep -E 'Allowed origins|Packages that will be upgraded|No packages found|ERROR' | sed 's/^/    /' | cut -c1-200 || true
  fi
}

step_verify() {
  step "Verification"
  local n; n=$(dpkg -l | grep -c '^ii')
  echo "  installed packages: $n (640 on the stock Zero W image)"
  echo "  disk free on /: $(df -h --output=avail / | tail -1 | tr -d ' ')"
  echo "  bluetooth service: $(systemctl is-enabled bluetooth.service 2>&1 | head -1)"
  local tz; tz=$(timedatectl show -p Timezone --value 2>/dev/null || true)
  echo "  time zone: ${tz:-unknown} (the midnight refresh and the date follow it; change with: sudo timedatectl set-timezone America/New_York)"
  if (( ! DRY_RUN )) && [[ -f /var/run/reboot-required ]]; then
    info "reboot to apply boot changes:  sudo reboot"
  fi
}

# --- main --------------------------------------------------------------------------
require_pi
require_root
if (( DRY_RUN )); then
  info "DRY RUN: nothing will be changed. Re-run with --apply (as root) to execute."
else
  mkdir -p "$BACKUP_DIR"; exec > >(tee -a /var/log/inkyframe-setup.log) 2>&1
  info "backups and state snapshot go to $BACKUP_DIR; log: /var/log/inkyframe-setup.log"
  confirm "This removes packages, changes services and the journal size."
  snapshot_state
fi

for s in "${ALL_STEPS[@]}"; do
  if want "$s"; then "step_$s"; fi
done
echo
if (( DRY_RUN )); then info "dry run complete; nothing changed."; else info "done. Reboot when ready: sudo reboot"; fi
