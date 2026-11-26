#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build a meshkeep .deb from an already compiled binary.
# Usage: packaging/build-deb.sh <deb-arch> <binary> <version> <out-dir>
#   deb-arch: amd64, arm64 or armhf
set -eu

arch=$1
bin=$2
version=$3
out=$4
here=$(cd "$(dirname "$0")" && pwd)

case $arch in
  amd64|arm64|armhf) ;;
  *) echo "build-deb.sh: unknown architecture $arch, use amd64, arm64 or armhf" >&2; exit 2 ;;
esac

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
chmod 0755 "$root"

install -D -m 0755 "$bin" "$root/usr/bin/meshkeep"
install -D -m 0644 "$here/meshkeep.service" "$root/usr/lib/systemd/system/meshkeep.service"
install -D -m 0640 "$here/meshkeep.toml" "$root/etc/meshkeep/meshkeep.toml"
install -D -m 0644 "$here/copyright" "$root/usr/share/doc/meshkeep/copyright"

date=$(date -R -u -d "@${SOURCE_DATE_EPOCH:-$(date +%s)}")
printf 'meshkeep (%s) stable; urgency=medium\n\n  * Release %s.\n\n -- meshkeep maintainers <noreply@packet-net.github.io>  %s\n' \
  "$version" "$version" "$date" | gzip -9n > "$root/usr/share/doc/meshkeep/changelog.Debian.gz"
chmod 0644 "$root/usr/share/doc/meshkeep/changelog.Debian.gz"

size=$(du -sk --apparent-size "$root" | cut -f1)
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: meshkeep
Version: $version
Architecture: $arch
Maintainer: meshkeep maintainers <noreply@packet-net.github.io>
Installed-Size: $size
Depends: adduser
Section: net
Priority: optional
Homepage: https://github.com/packet-net/meshkeep
Description: storing proxy and web client for MeshCore companion radios
 meshkeep keeps a permanent connection to a MeshCore companion radio, stores
 every message it receives, and lets several MeshCore apps and web browsers
 use the radio at once without missing messages.
CONTROL
echo /etc/meshkeep/meshkeep.toml > "$root/DEBIAN/conffiles"

cat > "$root/DEBIAN/postinst" <<'SCRIPT'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
  if ! getent passwd meshkeep >/dev/null; then
    adduser --system --group --home /var/lib/meshkeep --no-create-home \
      --gecos "meshkeep" meshkeep >/dev/null
  fi
  adduser meshkeep dialout >/dev/null 2>&1 || true
  chown root:meshkeep /etc/meshkeep/meshkeep.toml
  chmod 0640 /etc/meshkeep/meshkeep.toml
  if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null || true
    # Restart on upgrade if it was running; a fresh install waits for configuration.
    if systemctl is-active --quiet meshkeep; then
      systemctl restart meshkeep || true
    fi
  fi
fi
exit 0
SCRIPT

cat > "$root/DEBIAN/prerm" <<'SCRIPT'
#!/bin/sh
set -e
if [ "$1" = remove ] && [ -d /run/systemd/system ]; then
  systemctl disable --now meshkeep >/dev/null 2>&1 || true
fi
exit 0
SCRIPT

cat > "$root/DEBIAN/postrm" <<'SCRIPT'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload >/dev/null || true
fi
if [ "$1" = purge ]; then
  rm -rf /var/lib/meshkeep
fi
exit 0
SCRIPT
chmod 0755 "$root/DEBIAN/postinst" "$root/DEBIAN/prerm" "$root/DEBIAN/postrm"

mkdir -p "$out"
deb="$out/meshkeep_${version}_${arch}.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$deb" >/dev/null
echo "$deb"
