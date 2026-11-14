#!/usr/bin/env bash
#
# Build Kubernaut.app and a zip of it.
#
# Usage: scripts/bundle-macos.sh [--sidecars <dir>] [--binary <path>]
#
#   --sidecars <dir>  copy helm, trivy and kustomize from <dir> into the app,
#                     beside the executable, where the app looks for them
#   --binary <path>   use an already built kubernaut-ui instead of building one
#
# The app is ad-hoc signed, which gives it a stable identity but is not a
# Developer ID signature: Gatekeeper still warns on the first run. A signed and
# notarised build needs a paid developer account this project does not have.

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

if [ -z "$binary" ]; then
  cargo build --release -p kubernaut-ui
  binary="target/release/kubernaut-ui"
fi
[ -x "$binary" ] || { echo "no executable at $binary" >&2; exit 1; }

app="dist/Kubernaut.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/kubernaut-ui"
cp crates/kubernaut-ui/assets/icon.icns "$app/Contents/Resources/icon.icns"

if [ -n "$sidecars" ]; then
  for tool in helm trivy kustomize; do
    [ -x "$sidecars/$tool" ] || { echo "missing $sidecars/$tool" >&2; exit 1; }
    cp "$sidecars/$tool" "$app/Contents/MacOS/$tool"
  done
fi

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Kubernaut</string>
  <key>CFBundleDisplayName</key><string>Kubernaut</string>
  <key>CFBundleIdentifier</key><string>dev.kubernaut.app</string>
  <key>CFBundleExecutable</key><string>kubernaut-ui</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

zip="dist/Kubernaut-${version}-macos-$(uname -m).zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "built $app"
echo "built $zip"
