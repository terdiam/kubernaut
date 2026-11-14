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
# The tarball unpacks to kubernaut-<version>/ and holds the executable, the
# sidecars, an icon, a desktop entry, and install.sh, which puts it under
# ~/.local and registers the entry with the desktop. Nothing needs root.
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
cp "$here"/kubernaut-ui "$here"/kubernaut.png "$target"/
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
