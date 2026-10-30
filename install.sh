#!/usr/bin/env bash
# Install the photo frame on a Raspberry Pi from a GitHub release (no git, no compiler needed).
#
#   curl -fsSL https://github.com/pshanesmith/InkyGadget/releases/latest/download/install.sh | sudo bash
#
# Options go after `-s --`, for example:  ... | sudo bash -s -- --version v0.1.0 --yes
# Or download first and read it (it runs as root):  curl -fsSLO <url>/install.sh && less install.sh && sudo bash install.sh
#
# It downloads the newest release (a program built for the Pi Zero W's ARMv6 CPU, which also runs on every other
# 32-bit Raspberry Pi OS board), checks its SHA-256, unpacks it and runs the bundled setup/03-install-service.sh.
# Run it again to update. First time on a new Pi: run setup/02-inky-setup.sh --apply and reboot (see README.md).
#
# Options: --version TAG   install that release instead of the latest
#          --no-start --yes   passed on to setup/03-install-service.sh
#          --dry-run       download, verify and unpack, then show what would be installed
# The repository can be changed with INKYFRAME_REPO=OWNER/NAME (default pshanesmith/InkyGadget), and the download
# location with INKYFRAME_RELEASE_BASE (a URL or file:// directory holding the release files; used by tests).
#
# Everything is inside main(), which runs only after the whole script has been read: a download that is cut
# short can never execute half of it.

die() { echo "error: $*" >&2; exit 1; }
info() { echo "==> $*"; }

main() {
  set -euo pipefail
  local REPO=${INKYFRAME_REPO:-pshanesmith/InkyGadget}
  local ASSET=inkyframe-armv6.tar.gz
  local VERSION=latest DRY=0
  local PASS=()
  while (( $# )); do
    case $1 in
      --version) VERSION=${2:?--version needs a tag}; shift ;;
      --dry-run) DRY=1 ;;
      --no-start|--yes) PASS+=("$1") ;;
      -h|--help) echo "usage: curl -fsSL <release-url>/install.sh | sudo bash -s -- [--version TAG] [--no-start] [--yes] [--dry-run]"; return 0 ;;
      *) die "unknown option: $1" ;;
    esac
    shift
  done

  local BASE
  if [[ -n ${INKYFRAME_RELEASE_BASE:-} ]]; then BASE=$INKYFRAME_RELEASE_BASE
  elif [[ $VERSION == latest ]]; then BASE=https://github.com/$REPO/releases/latest/download
  else BASE=https://github.com/$REPO/releases/download/$VERSION; fi

  if (( ! DRY )); then
    (( EUID == 0 )) || die "run as root: curl -fsSL <url>/install.sh | sudo bash   (or: sudo bash install.sh)"
    case $(uname -m) in armv6l|armv7l|aarch64) ;; *) die "this release is for Raspberry Pi OS (ARM); this system is $(uname -m)" ;; esac
    grep -q "Raspberry Pi" /proc/device-tree/model 2>/dev/null || die "this does not look like a Raspberry Pi"
  fi
  command -v curl >/dev/null || die "curl is needed (sudo apt-get install curl)"

  # When this script is piped into bash, stdin is the script itself. The installer's questions must not read it:
  # use the terminal if there is one, otherwise answer them (--yes) and read nothing.
  local INPUT=/dev/null
  if [[ -t 0 ]]; then INPUT=/dev/stdin
  elif ( : </dev/tty ) 2>/dev/null; then INPUT=/dev/tty
  else PASS+=(--yes); fi

  TMP=$(mktemp -d)   # global on purpose: the EXIT trap runs after main() has returned
  trap 'rm -rf "$TMP"' EXIT

  info "downloading $ASSET ($VERSION) from $BASE"
  curl -fsSL "$BASE/$ASSET" -o "$TMP/$ASSET" || die "download failed: $BASE/$ASSET (is there a release yet?)"
  curl -fsSL "$BASE/$ASSET.sha256" -o "$TMP/$ASSET.sha256" || die "download failed: $BASE/$ASSET.sha256"
  info "checking the checksum"
  if command -v sha256sum >/dev/null; then (cd "$TMP" && sha256sum -c "$ASSET.sha256" >/dev/null) || die "checksum mismatch: the download is incomplete or corrupt (the checksum comes from the same place as the file, so it cannot prove who published it)"
  else (cd "$TMP" && shasum -a 256 -c "$ASSET.sha256" >/dev/null) || die "checksum mismatch: the download is incomplete or corrupt (the checksum comes from the same place as the file, so it cannot prove who published it)"
  fi
  tar -C "$TMP" -xzf "$TMP/$ASSET"
  local REL=$TMP/${ASSET%.tar.gz}
  [[ -x $REL/setup/03-install-service.sh && -f $REL/VERSION ]] || die "the archive does not look like a release"
  info "release $(cat "$REL/VERSION")"

  if (( DRY )); then
    info "dry run: would run $REL/setup/03-install-service.sh --release $REL --apply ${PASS[*]:-}"
    echo "contents:"; (cd "$REL" && find . -type f | sort | sed 's/^/  /')
    return 0
  fi
  "$REL/setup/03-install-service.sh" --release "$REL" --apply ${PASS[@]+"${PASS[@]}"} <"$INPUT"
}

main "$@"
