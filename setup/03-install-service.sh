#!/usr/bin/env bash
# Install the frame into /opt/inkyframe from an unpacked release and run it as a systemd service.
# install.sh downloads and unpacks the release and then runs this; run it again to update.
#
#   sudo ./setup/03-install-service.sh --release DIR --apply
# Options: --release DIR  the unpacked release (bin/inkyframe, setup/, systemd/, config/, tools/, VERSION)
#          --no-start     install and enable, but do not start now
#          --uninstall    stop and remove the service (keeps /opt/inkyframe, /etc/inkyframe, photos and state)
#          --yes          skip confirmation prompts
# Layout:  /opt/inkyframe     bin/, setup/, systemd/, config/, tools/, VERSION  (root-owned)
#          /etc/inkyframe     config.toml, secrets.env (0600), share-password
#          /var/lib/inkyframe state.json and the fitted copies of the photos (cache/)
#          /srv/inkyframe/Images   the shared photo folder
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/common.sh
source "$HERE/lib/common.sh"

main() {
  local DEPLOY=${INKYFRAME_DEPLOY_DIR:-/opt/inkyframe} ETC=${ETC_DIR:-/etc/inkyframe} STATE=${STATE_DIR_PATH:-/var/lib/inkyframe}
  local SHARE=${SHARE_DIR:-/srv/inkyframe/Images} UNIT=/etc/systemd/system/inkyframe.service
  local RELEASE="" START=1 UNINSTALL=0
  ASSUME_YES=0
  while (( $# )); do
    case $1 in
      --apply) DRY_RUN=0 ;;
      --release) RELEASE=$2; shift ;;
      --no-start) START=0 ;;
      --uninstall) UNINSTALL=1 ;;
      --yes) ASSUME_YES=1 ;;
      -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; return 0 ;;
      *) die "unknown option: $1 (try --help)" ;;
    esac
    shift
  done
  require_pi
  require_root

  if (( UNINSTALL )); then
    step "Uninstall"
    run systemctl disable --now inkyframe.service
    run rm -f "$UNIT"
    run systemctl daemon-reload
    info "removed the service; $DEPLOY, $ETC, $STATE and the photos were kept"
    return 0
  fi

  [[ -n $RELEASE ]] || die "--release DIR is required (install.sh prepares it)"
  [[ -x $RELEASE/bin/inkyframe ]] || die "$RELEASE is not an unpacked release (no bin/inkyframe)"

  step "Install the release into $DEPLOY"
  echo "  release: $(cat "$RELEASE/VERSION" 2>/dev/null || echo unknown)   target: $DEPLOY"
  run install -d -m 755 "$DEPLOY"
  local part
  for part in bin setup systemd config tools; do
    [[ -d $RELEASE/$part ]] || continue
    run rm -rf "${DEPLOY:?}/$part"
    run cp -a "$RELEASE/$part" "$DEPLOY/$part"
  done
  run install -m 644 "$RELEASE/VERSION" "$DEPLOY/VERSION"
  run chown -R root:root "$DEPLOY"
  run chmod -R go+rX "$DEPLOY"
  if (( ! DRY_RUN )); then
    "$DEPLOY/bin/inkyframe" --version || die "the installed program does not run (is this a Raspberry Pi OS 32-bit system?)"
  fi

  step "Preflight"
  id inkyframe >/dev/null 2>&1 || (( DRY_RUN )) || die "the service account is missing: run setup/02-inky-setup.sh --apply first"
  [[ -e /dev/spidev0.0 ]] || warn "/dev/spidev0.0 does not exist: SPI is off or the Pi has not been rebooted since setup/02-inky-setup.sh"
  local avail; avail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  (( avail >= 150 )) || warn "only ${avail} MB RAM available now"

  step "Configuration in $ETC"
  run install -d -m 755 "$ETC"
  if [[ ! -f $ETC/config.toml ]]; then
    run install -m 644 "$DEPLOY/config/config.example.toml" "$ETC/config.toml"
    warn "installed the example config: set your location with  sudo $DEPLOY/bin/inkyframe geocode ZIP --config $ETC/config.toml"
  else
    echo "  keeping existing $ETC/config.toml"
  fi
  if [[ ! -f $ETC/secrets.env ]]; then
    run install -m 600 -o root -g root "$DEPLOY/config/secrets.env.example" "$ETC/secrets.env"
    warn "put INKYFRAME_HA_TOKEN in $ETC/secrets.env or the buttons cannot call Home Assistant"
  else
    echo "  keeping existing $ETC/secrets.env"
  fi
  run install -d -m 755 -o inkyframe -g inkyframe "$STATE"
  run install -d -m 2775 -o inkyframe -g inkyframe "$SHARE"

  step "systemd unit"
  write_file "$UNIT" "$(cat "$RELEASE/systemd/inkyframe.service")"
  confirm "This installs a service that drives the e-paper display."
  if (( ! DRY_RUN )); then
    # A config the program refuses would make the service restart every 10 s: stop before touching it.
    "$DEPLOY/bin/inkyframe" check-config --config "$ETC/config.toml" \
      || die "$ETC/config.toml is not valid (see above). Fix it and run this again; the running service was left alone."
  fi
  run systemctl daemon-reload
  run systemctl enable inkyframe.service
  if (( START )); then run systemctl restart inkyframe.service; fi

  if (( ! DRY_RUN && START )); then
    step "Status"
    sleep 5
    if systemctl is-active --quiet inkyframe; then echo "  service is active"; else warn "the service is not active"; fi
    journalctl -u inkyframe.service --no-pager -n 12 | cut -c1-200
  fi
  echo
  if (( DRY_RUN )); then info "dry run complete; nothing changed."; else info "installed. Logs: journalctl -u inkyframe -f"; fi
}

main "$@"; exit $?
