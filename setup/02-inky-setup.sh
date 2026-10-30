#!/usr/bin/env bash
# Prepare the Pi for the frame: SPI for the display, power-saving boot settings, the service account, the photo
# share (Samba) and Wi-Fi power save.
#
#   ./setup/02-inky-setup.sh                 dry run (default): shows everything, changes nothing
#   sudo ./setup/02-inky-setup.sh --apply    do it (asks you to type 'yes' first)
#
# Options:  --yes             skip the confirmation prompt
#           --no-samba        do not install or configure the photo share
#           --share-password PW   set the share password to PW (at least 3 characters) instead of a random one;
#                             without it, a random 6-character password is created once and kept
#           --only a,b        run only these steps     --skip a,b   skip these steps
# Steps:    boot account samba wifi verify
# Never reboots; reboot yourself afterwards (SPI and the boot settings need it). Safe to run repeatedly.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/common.sh
source "$HERE/lib/common.sh"

BOOT_DIR=${BOOT_DIR:-/boot/firmware}
SVC_USER=inkyframe
SHARE_DIR=${SHARE_DIR:-/srv/inkyframe/Images}
ETC=${ETC_DIR:-/etc/inkyframe}
STATE=${STATE_DIR_PATH:-/var/lib/inkyframe}
ALL_STEPS=(boot account samba wifi verify)
ASSUME_YES=0; SAMBA=1; ONLY=""; SKIP=""; PW_OPT=""

while (( $# )); do
  case $1 in
    --apply) DRY_RUN=0 ;;
    --yes) ASSUME_YES=1 ;;
    --no-samba) SAMBA=0 ;;
    --share-password) PW_OPT=${2:?--share-password needs a value}; shift ;;
    --only) ONLY=$2; shift ;;
    --skip) SKIP=$2; shift ;;
    -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
  shift
done
want() { [[ -z $ONLY || ,$ONLY, == *,$1,* ]] && [[ ,$SKIP, != *,$1,* ]]; }

# SPI on, and no chip-select pin claimed by the kernel (the display driver drives GPIO8 itself). The rest cuts
# power: no Bluetooth, no audio, no camera or HDMI/KMS stack on a headless board.
BOOT_BLOCK='dtparam=spi=on
dtoverlay=spi0-0cs
dtoverlay=disable-bt
dtparam=audio=off
camera_auto_detect=0
display_auto_detect=0
gpu_mem=16
disable_splash=1'
BOOT_CONFLICTS='dtparam=spi=|dtoverlay=spi0-|dtparam=audio=|camera_auto_detect=|display_auto_detect=|dtoverlay=vc4-kms-v3d|dtoverlay=vc4-fkms-v3d|max_framebuffers=|gpu_mem=|dtoverlay=disable-bt|disable_fw_kms_setup=|disable_splash='

step_boot() {
  step "Boot configuration (SPI on; Bluetooth, audio, camera and HDMI overlay off)"
  [[ -f $BOOT_DIR/config.txt ]] || die "$BOOT_DIR/config.txt not found"
  config_txt_update "$BOOT_DIR/config.txt" inky "$BOOT_BLOCK" "$BOOT_CONFLICTS"
  info "takes effect after a reboot"
}

step_account() {
  step "Service account, folders"
  if id "$SVC_USER" >/dev/null 2>&1; then
    echo "  user $SVC_USER exists"
  else
    run useradd --system --home-dir "$STATE" --no-create-home --shell /usr/sbin/nologin "$SVC_USER"
  fi
  local g
  for g in spi gpio; do
    if getent group "$g" >/dev/null; then run usermod -aG "$g" "$SVC_USER"; else warn "group $g does not exist"; fi
  done
  run install -d -m 755 -o root -g root "$ETC"
  run install -d -m 755 -o "$SVC_USER" -g "$SVC_USER" "$STATE"
  run install -d -m 2775 -o "$SVC_USER" -g "$SVC_USER" "$(dirname "$SHARE_DIR")" "$SHARE_DIR"
}

# The share password: random, kept in /etc/inkyframe/share-password (the welcome screen shows it until the first
# photo is added), and set as the Samba password of the service account.
step_samba() {
  step "Photo share (Samba, SMB2+ only, one share)"
  if (( ! SAMBA )); then echo "  skipped (--no-samba)"; return 0; fi
  if ! installed samba; then
    run env DEBIAN_FRONTEND=noninteractive apt-get -y install --no-install-recommends samba
  else
    echo "  samba already installed"
  fi
  write_file /etc/samba/smb.conf "# Managed by setup/02-inky-setup.sh
[global]
   server role = standalone server
   server string = InkyGadget
   workgroup = WORKGROUP
   security = user
   map to guest = Never
   server min protocol = SMB2_10
   smb ports = 445
   disable netbios = yes
   dns proxy = no
   interfaces = lo wlan0
   bind interfaces only = yes
   load printers = no
   printing = bsd
   printcap name = /dev/null
   disable spoolss = yes
   usershare allow guests = no
   logging = systemd
   log level = 1

[Images]
   comment = Photos for the frame
   path = $SHARE_DIR
   browseable = yes
   read only = no
   valid users = $SVC_USER
   force user = $SVC_USER
   force group = $SVC_USER
   create mask = 0664
   directory mask = 0775
   veto files = /._*/.DS_Store/Thumbs.db/
   delete veto files = yes"
  write_file /etc/avahi/services/inkyframe-smb.service '<?xml version="1.0" standalone="no"?>
<!DOCTYPE service-group SYSTEM "avahi-service.dtd">
<service-group>
  <name replace-wildcards="yes">InkyGadget photos on %h</name>
  <service>
    <type>_smb._tcp</type>
    <port>445</port>
  </service>
</service-group>'

  # Low risk (a home LAN, one folder of photos), so the password is short. --share-password sets your own.
  local pw=""
  if [[ -n $PW_OPT ]]; then
    [[ ${#PW_OPT} -ge 3 ]] || die "--share-password must be at least 3 characters"
    pw=$PW_OPT
  elif [[ -f $ETC/share-password ]]; then
    echo "  keeping existing $ETC/share-password"
    (( DRY_RUN )) || pw=$(cat "$ETC/share-password")
  elif (( ! DRY_RUN )); then
    # `|| true`: with pipefail, head closing the pipe makes tr die of SIGPIPE, which would abort the script.
    pw=$(LC_ALL=C tr -dc 'abcdefghjkmnpqrstuvwxyz23456789' </dev/urandom | head -c 6 || true)
    [[ ${#pw} -eq 6 ]] || die "could not generate the share password"
  fi
  if (( DRY_RUN )); then
    echo "  [dry-run] would set the share password in $ETC/share-password and for the Samba user $SVC_USER"
  elif [[ -n $pw ]]; then
    install -d -m 755 "$ETC"
    install -m 640 -o root -g "$SVC_USER" /dev/null "$ETC/share-password"
    printf '%s\n' "$pw" > "$ETC/share-password"
    printf '%s\n%s\n' "$pw" "$pw" | smbpasswd -s -a "$SVC_USER" >/dev/null
    info "share password: $pw   (also in $ETC/share-password; it is shown on the frame's screen until the first photo)"
  fi

  # Only the file server is needed: no NetBIOS name service, no domain controller, no winbind.
  local u
  for u in nmbd.service winbind.service samba-ad-dc.service; do
    if unit_exists "$u"; then run systemctl disable --now "$u"; fi
  done
  run systemctl enable smbd.service
  run systemctl restart smbd.service
  if unit_exists avahi-daemon.service; then run systemctl restart avahi-daemon.service; fi
}

step_wifi() {
  step "Wi-Fi power save"
  write_file /etc/NetworkManager/conf.d/inkyframe-wifi-powersave.conf '[connection]
wifi.powersave = 3'
  echo "  (the first packet after a long idle can be a little slower; remove this file to turn power save off)"
}

step_verify() {
  step "Verification"
  echo "  SPI device: $(ls /dev/spidev0.0 2>/dev/null || echo 'not yet (reboot after the boot step)')"
  echo "  user: $(id "$SVC_USER" 2>&1 | head -1)"
  echo "  share dir: $(ls -ld "$SHARE_DIR" 2>&1 | head -1)"
  echo "  installed packages: $(dpkg -l | grep -c '^ii')"
  if (( SAMBA )) && installed samba; then
    echo "  smbd: $(systemctl is-active smbd 2>&1 | head -1) ($(systemctl is-enabled smbd 2>&1 | head -1))"
  fi
  (( DRY_RUN )) || info "reboot to apply the boot changes:  sudo reboot"
}

require_pi
require_root
if (( DRY_RUN )); then
  info "DRY RUN: nothing will be changed. Re-run with --apply (as root) to execute."
else
  mkdir -p "$BACKUP_DIR"; exec > >(tee -a /var/log/inkyframe-setup.log) 2>&1
  info "backups go to $BACKUP_DIR; log: /var/log/inkyframe-setup.log"
  confirm "This edits $BOOT_DIR/config.txt, adds a service account and installs the photo share."
  snapshot_state
fi
for s in "${ALL_STEPS[@]}"; do
  if want "$s"; then "step_$s"; fi
done
echo
if (( DRY_RUN )); then info "dry run complete; nothing changed."; else info "done. Reboot when ready: sudo reboot"; fi
