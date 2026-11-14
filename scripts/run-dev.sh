#!/usr/bin/env bash
#
# Build and open the app as a macOS bundle, so the Dock and Launchpad show its icon.
#
# A bare `cargo run` starts the executable without a bundle, and macOS draws a
# generic icon for a process that has none. The icon comes from a bundle's
# Info.plist, so this wraps the debug build in a small one, under its own
# identifier so it does not clash with an installed Kubernaut.
#
# Usage: scripts/run-dev.sh [--release]

set -euo pipefail
cd "$(dirname "$0")/.."

profile=debug
flag=()
if [ "${1:-}" = "--release" ]; then
  profile=release
  flag=(--release)
fi

cargo build -p kubernaut-ui "${flag[@]}"

app="target/dev-app/Kubernaut Dev.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$profile/kubernaut-ui" "$app/Contents/MacOS/kubernaut-ui"
cp crates/kubernaut-ui/assets/icon.icns "$app/Contents/Resources/icon.icns"

cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Kubernaut Dev</string>
  <key>CFBundleDisplayName</key><string>Kubernaut Dev</string>
  <key>CFBundleIdentifier</key><string>dev.kubernaut.app.dev</string>
  <key>CFBundleExecutable</key><string>kubernaut-ui</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

codesign --force --sign - "$app" >/dev/null
open -n "$app"
echo "opened $app"
