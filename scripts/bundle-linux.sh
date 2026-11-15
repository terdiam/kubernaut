#!/usr/bin/env bash
#
# Build Kubernaut for Linux and pack it as a tarball.
#
# Usage: scripts/bundle-linux.sh [--sidecars <dir>] [--binary <path>]
#
#   --sidecars <dir>  copy helm, trivy and kustomize from <dir> beside the
#                     executable, where the app looks for them
#   --binary <path>   use an already built kubernaut-ui instead of building one
#
# Two files come out of dist/:
#
#   Kubernaut-<version>-linux-<arch>.tar.gz   unpacks to kubernaut-<version>/ with
#       the executable, the sidecars, an icon and install.sh, which puts it under
#       ~/.local and registers a desktop entry. Nothing needs root, and an
#       install made this way updates itself from inside the app. This is the
#       file the in-app updater downloads.
#   kubernaut_<version>_<deb arch>.deb         the same under /usr/lib/kubernaut,
#       for `apt install ./file.deb`. Root owns that folder, so the app tells the
#       person a new version exists rather than replacing itself.
#
# The .deb needs dpkg-deb; it is skipped, with a note, where that is missing.
#
# Building needs the development packages of the window system and fonts; see
# "Build" in the README for the list.

set -euo pipefail
cd "$(dirname "$0")/.."

sidecars=""
binary=""
while [ $# -gt 0 ]; do
  case "$1" in
    --sidecars) sidecars="${2:?--sidecars needs a directory}"; shift 2 ;;
    --binary) binary="${2:?--binary needs a path}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

version=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
arch=$(uname -m)

if [ -z "$binary" ]; then
  cargo build --release -p kubernaut-ui
  binary="${CARGO_TARGET_DIR:-target}/release/kubernaut-ui"
fi
[ -x "$binary" ] || { echo "no executable at $binary" >&2; exit 1; }

name="kubernaut-${version}"
stage="dist/${name}"
rm -rf "$stage"
mkdir -p "$stage"
cp "$binary" "$stage/kubernaut-ui"
cp crates/kubernaut-ui/assets/512x512.png "$stage/kubernaut.png"
# Marks the folder as an install, and names the version, for the in-app updater.
printf 'dev.kubernaut.app %s\n' "$version" > "$stage/kubernaut.package"

if [ -n "$sidecars" ]; then
  for tool in helm trivy kustomize; do
    [ -x "$sidecars/$tool" ] || { echo "missing $sidecars/$tool" >&2; exit 1; }
    cp "$sidecars/$tool" "$stage/$tool"
  done
fi

cat > "$stage/install.sh" <<'SH'
#!/bin/sh
# Install Kubernaut for the current user: nothing here needs root.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${XDG_DATA_HOME:-$HOME/.local/share}"
target="$prefix/kubernaut"
bin="${HOME}/.local/bin"

rm -rf "$target"
mkdir -p "$target" "$bin" "$prefix/applications"
cp "$here"/kubernaut-ui "$here"/kubernaut.png "$here"/kubernaut.package "$target"/
for tool in helm trivy kustomize; do
  [ -f "$here/$tool" ] && cp "$here/$tool" "$target/$tool"
done
ln -sf "$target/kubernaut-ui" "$bin/kubernaut-ui"

cat > "$prefix/applications/kubernaut.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Kubernaut
Comment=Manage Kubernetes clusters
Exec=$target/kubernaut-ui
Icon=$target/kubernaut.png
Terminal=false
Categories=Development;
StartupWMClass=kubernaut-ui
DESKTOP

echo "Installed to $target"
echo "Run it from your applications menu, or: $bin/kubernaut-ui"
SH
chmod +x "$stage/install.sh"

tarball="dist/Kubernaut-${version}-linux-${arch}.tar.gz"
rm -f "$tarball"
tar -C dist -czf "$tarball" "$name"
echo "built $tarball"

if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "dpkg-deb not found: no .deb built"
  exit 0
fi

case "$arch" in
  x86_64) deb_arch=amd64 ;;
  aarch64) deb_arch=arm64 ;;
  *) echo "no .deb for $arch"; exit 0 ;;
esac

# What the executable links against, as the distribution's own tool works it out.
depends=""
if command -v dpkg-shlibdeps >/dev/null 2>&1; then
  scratch=$(mktemp -d)
  mkdir "$scratch/debian"
  printf 'Source: kubernaut\n\nPackage: kubernaut\nArchitecture: any\n' > "$scratch/debian/control"
  depends=$(cd "$scratch" && dpkg-shlibdeps -O -e "$OLDPWD/$stage/kubernaut-ui" 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p') || depends=""
  rm -rf "$scratch"
fi

root="dist/deb-root"
rm -rf "$root"
mkdir -p "$root/DEBIAN" "$root/usr/lib/kubernaut" "$root/usr/bin" \
  "$root/usr/share/applications" "$root/usr/share/icons/hicolor/512x512/apps"
for file in kubernaut-ui helm trivy kustomize; do
  [ -f "$stage/$file" ] && install -m 755 "$stage/$file" "$root/usr/lib/kubernaut/$file"
done
install -m 644 "$stage/kubernaut.package" "$root/usr/lib/kubernaut/kubernaut.package"
install -m 644 "$stage/kubernaut.png" "$root/usr/share/icons/hicolor/512x512/apps/kubernaut.png"
ln -s ../lib/kubernaut/kubernaut-ui "$root/usr/bin/kubernaut-ui"

cat > "$root/usr/share/applications/kubernaut.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Kubernaut
Comment=Manage Kubernetes clusters
Exec=/usr/bin/kubernaut-ui
Icon=kubernaut
Terminal=false
Categories=Development;
StartupWMClass=kubernaut-ui
DESKTOP

size=$(du -sk "$root/usr" | cut -f1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: kubernaut
Version: ${version}
Architecture: ${deb_arch}
Maintainer: Kubernaut <noreply@users.noreply.github.com>
Section: devel
Priority: optional
Installed-Size: ${size}
Depends: ${depends:-libc6}
Recommends: libvulkan1, mesa-vulkan-drivers, libxkbcommon-x11-0, libwayland-client0
Homepage: https://github.com/terdiam/kubernaut
Description: Kubernetes desktop client
 Browse and manage Kubernetes clusters: workloads, logs, shells, Helm,
 security scans and a live topology of what is running.
CONTROL

deb="dist/kubernaut_${version}_${deb_arch}.deb"
rm -f "$deb"
dpkg-deb --root-owner-group --build "$root" "$deb" >/dev/null
rm -rf "$root"
echo "built $deb"
