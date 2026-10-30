#!/usr/bin/env bash
# Pack a release: dist/inkyframe-armv6.tar.gz (+ .sha256) and a copy of install.sh.
#   tools/package-release.sh [BINARY]     BINARY defaults to dist/inkyframe-armv6 (from tools/cross-build.sh)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=${1:-dist/inkyframe-armv6}
[[ -x $BIN ]] || { echo "error: $BIN not found; run tools/cross-build.sh first" >&2; exit 1; }
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
NAME=inkyframe-armv6
STAGE=$(mktemp -d); trap 'rm -rf "$STAGE"' EXIT
R=$STAGE/$NAME
mkdir -p "$R/bin" "$R/setup/lib" "$R/systemd" "$R/config" "$R/tools"
install -m 755 "$BIN" "$R/bin/inkyframe"
install -m 755 setup/*.sh "$R/setup/"; install -m 644 setup/lib/common.sh "$R/setup/lib/"
install -m 644 systemd/inkyframe.service "$R/systemd/"
install -m 644 config/config.example.toml config/secrets.env.example "$R/config/"
install -m 755 tools/health-check.sh "$R/tools/"
install -m 644 LICENSE NOTICE README.md "$R/"
echo "$VERSION" > "$R/VERSION"
mkdir -p dist
COPYFILE_DISABLE=1 tar -C "$STAGE" -czf "dist/$NAME.tar.gz" "$NAME"
(cd dist && { sha256sum "$NAME.tar.gz" 2>/dev/null || shasum -a 256 "$NAME.tar.gz"; } > "$NAME.tar.gz.sha256")
cp install.sh dist/install.sh
ls -l dist/
