#!/usr/bin/env bash
# Shared helpers for the setup scripts. Source this file; do not execute it.
#
# Conventions: DRY_RUN=1 (default) prints what would happen and changes nothing.
# Every file change is shown as a diff and backed up before it is overwritten.

DRY_RUN=${DRY_RUN:-1}
BACKUP_ROOT=${BACKUP_ROOT:-/var/backups/inkyframe-setup}
BACKUP_DIR=${BACKUP_DIR:-$BACKUP_ROOT/$(date +%Y%m%d-%H%M%S)}

if [[ -t 1 ]]; then
  C_B=$'\e[1m'; C_Y=$'\e[33m'; C_R=$'\e[31m'; C_G=$'\e[32m'; C_0=$'\e[0m'
else
  C_B=""; C_Y=""; C_R=""; C_G=""; C_0=""
fi

info() { echo "${C_G}==>${C_0} $*"; }
warn() { echo "${C_Y}warning:${C_0} $*" >&2; }
die()  { echo "${C_R}error:${C_0} $*" >&2; exit 1; }
step() { echo; echo "${C_B}### $*${C_0}"; }

# Run a command, or only print it in dry-run mode.
run() {
  if (( DRY_RUN )); then
    echo "  [dry-run] $*"
  else
    echo "  + $*"
    "$@"
  fi
}

require_root() {
  (( DRY_RUN )) && return 0
  (( EUID == 0 )) || die "--apply needs root; run: sudo $0 --apply ..."
}

require_pi() {
  grep -q "Raspberry Pi" /proc/device-tree/model 2>/dev/null \
    || die "this does not look like a Raspberry Pi (/proc/device-tree/model)"
}

installed() {
  dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q '^install ok installed'
}

unit_exists() {
  systemctl list-unit-files --no-legend "$1" 2>/dev/null | grep -q "^$1"
}

# Copy a file into the backup directory (keeps its absolute path).
backup_file() {
  local f=$1
  [[ -e $f ]] || return 0
  mkdir -p "$BACKUP_DIR$(dirname "$f")"
  cp -a "$f" "$BACKUP_DIR$f"
}

# Install $2 (a temp file) as $1, showing a diff first; no-op when identical.
apply_file() {
  local target=$1 new=$2 mode=${3:-644}
  if [[ -f $target ]] && cmp -s "$target" "$new"; then
    echo "  unchanged: $target"
    return 0
  fi
  echo "  change: $target"
  local old=$target
  [[ -f $target ]] || old=/dev/null
  diff -u --label "$target" --label "$target (new)" "$old" "$new" | sed 's/^/    /' || true
  if (( ! DRY_RUN )); then
    backup_file "$target"
    install -D -m "$mode" "$new" "$target"
  fi
}

# Write literal content to a file via apply_file.
write_file() {
  local target=$1 content=$2 mode=${3:-644} tmp
  tmp=$(mktemp)
  printf '%s\n' "$content" > "$tmp"
  apply_file "$target" "$tmp" "$mode"
  rm -f "$tmp"
}

# Idempotently manage a marked block at the end of a Pi config.txt-style file:
#   config_txt_update FILE BLOCK_NAME "block content" "regex of lines to comment out"
# Previous copies of the block are replaced; matching lines outside the block are
# commented out with a "#inkyframe-setup# " prefix (reversible by hand). The block is
# placed in the [all] section so it applies to every board.
config_txt_update() {
  local file=$1 name=$2 content=$3 regex=$4 tmp
  tmp=$(mktemp)
  BLOCK="$content" awk -v begin="# BEGIN inkyframe-setup:$name" -v end="# END inkyframe-setup:$name" -v re="$regex" '
    $0 == begin { skip = 1; next }
    skip && $0 == end { skip = 0; next }
    skip { next }
    /^\[.*\][ \t]*$/ { last = $0 }
    $0 ~ ("^(" re ")") { print "#inkyframe-setup# " $0; next }
    { print }
    END {
      if (last != "[all]") print "[all]"
      print begin; print ENVIRON["BLOCK"]; print end
    }' "$file" > "$tmp"
  apply_file "$file" "$tmp" 755
  rm -f "$tmp"
}

# Save package and service state so changes can be audited or reversed.
snapshot_state() {
  (( DRY_RUN )) && { echo "  [dry-run] would snapshot package and service state to $BACKUP_DIR"; return 0; }
  mkdir -p "$BACKUP_DIR"
  dpkg --get-selections > "$BACKUP_DIR/dpkg-selections.txt"
  apt-mark showmanual > "$BACKUP_DIR/apt-manual.txt"
  systemctl list-unit-files --state=enabled --no-legend > "$BACKUP_DIR/enabled-units.txt"
  info "state snapshot saved in $BACKUP_DIR"
}

confirm() {
  (( DRY_RUN )) && return 0
  (( ASSUME_YES )) && return 0
  local reply
  read -r -p "$1 Type 'yes' to continue: " reply
  [[ $reply == yes ]] || die "aborted"
}

# Idempotently ensure kernel parameters exist on the single line of a cmdline.txt:
#   cmdline_ensure FILE "isolcpus=domain,managed_irq,3" "nohz_full=3" ...
# A token is added only when its key (text before "=") is absent; existing values are
# left alone and reported, so we never silently override the user's settings.
cmdline_ensure() {
  local file=$1; shift
  local line tok key tmp changed=0
  line=$(tr -d '\n' < "$file")
  for tok in "$@"; do
    key=${tok%%=*}
    if [[ " $line " =~ \ ${key//./\.}(=|\ ) ]]; then
      echo "  cmdline already sets $key; leaving it"
    else
      line="$line $tok"; changed=1
    fi
  done
  tmp=$(mktemp)
  printf '%s\n' "$line" > "$tmp"
  apply_file "$file" "$tmp" 755
  rm -f "$tmp"
}
