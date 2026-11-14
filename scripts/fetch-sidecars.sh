#!/usr/bin/env bash
#
# Download helm, trivy and kustomize for one platform into a directory, checking
# each archive against the checksum its publisher ships.
#
# The binaries are fetched here rather than committed, so the repository holds
# none. Pinning the digests in this file as well would also guard against a
# compromised distribution host, at the cost of a manual update on every bump.
#
# Usage: scripts/fetch-sidecars.sh <platform> <destination-dir>
#   platform: darwin-arm64 | darwin-amd64 | linux-amd64 | linux-arm64 | windows-amd64
#
# On Windows run it from Git Bash; it needs curl, tar and unzip (or 7z).

set -euo pipefail

HELM_VERSION="${HELM_VERSION:-v3.19.0}"
TRIVY_VERSION="${TRIVY_VERSION:-0.74.0}"
KUSTOMIZE_VERSION="${KUSTOMIZE_VERSION:-v5.8.1}"

usage="usage: fetch-sidecars.sh <darwin-arm64|darwin-amd64|linux-amd64|linux-arm64|windows-amd64> <destination-dir>"
platform="${1:?$usage}"
dest="${2:?$usage}"

# Each tool names its platforms and its archive type differently.
case "$platform" in
  darwin-arm64)  helm_p=darwin-arm64;  trivy_p=macOS-ARM64;   kustomize_p=darwin_arm64;  ext=tar.gz; exe="" ;;
  darwin-amd64)  helm_p=darwin-amd64;  trivy_p=macOS-64bit;   kustomize_p=darwin_amd64;  ext=tar.gz; exe="" ;;
  linux-amd64)   helm_p=linux-amd64;   trivy_p=Linux-64bit;   kustomize_p=linux_amd64;   ext=tar.gz; exe="" ;;
  linux-arm64)   helm_p=linux-arm64;   trivy_p=Linux-ARM64;   kustomize_p=linux_arm64;   ext=tar.gz; exe="" ;;
  windows-amd64) helm_p=windows-amd64; trivy_p=windows-64bit; kustomize_p=windows_amd64; ext=zip;    exe=.exe ;;
  *) echo "unsupported platform: $platform" >&2; exit 1 ;;
esac

mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

sha256() { # <file>
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

# Compare an archive with the first field of the line that names it.
verify() { # <archive> <checksum-file>
  local actual expected
  actual=$(sha256 "$1")
  expected=$(grep " \*\?$1\$" "$2" | awk '{print $1}')
  [ -n "$expected" ] && [ "$actual" = "$expected" ] || { echo "checksum mismatch for $1" >&2; exit 1; }
}

extract() { # <archive> [member...]
  local archive="$1"; shift
  case "$archive" in
    *.tar.gz) tar -xzf "$archive" "$@" ;;
    *.zip)
      if command -v unzip >/dev/null 2>&1; then unzip -q -o "$archive" "$@"
      else 7z x -y -bso0 "$archive" "$@" >/dev/null; fi ;;
  esac
}

archive="helm-${HELM_VERSION}-${helm_p}.${ext}"
curl -fsSLO "https://get.helm.sh/${archive}"
curl -fsSL "https://get.helm.sh/${archive}.sha256sum" -o helm.sha
# helm's checksum file names the archive on its own line; match on the hash.
[ "$(sha256 "$archive")" = "$(awk '{print $1}' helm.sha)" ] \
  || { echo "checksum mismatch for $archive" >&2; exit 1; }
extract "$archive"
install -m 755 "${helm_p}/helm${exe}" "$dest/helm${exe}"

base="https://github.com/aquasecurity/trivy/releases/download/v${TRIVY_VERSION}"
archive="trivy_${TRIVY_VERSION}_${trivy_p}.${ext}"
curl -fsSLO "${base}/${archive}"
curl -fsSL "${base}/trivy_${TRIVY_VERSION}_checksums.txt" -o trivy.sha
verify "$archive" trivy.sha
extract "$archive" "trivy${exe}"
install -m 755 "trivy${exe}" "$dest/trivy${exe}"

base="https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize/${KUSTOMIZE_VERSION}"
archive="kustomize_${KUSTOMIZE_VERSION}_${kustomize_p}.${ext}"
curl -fsSLO "${base}/${archive}"
curl -fsSL "${base}/checksums.txt" -o kustomize.sha
verify "$archive" kustomize.sha
extract "$archive" "kustomize${exe}"
install -m 755 "kustomize${exe}" "$dest/kustomize${exe}"

# A sidecar built for another machine cannot be run here, which is not a failure.
"$dest/helm${exe}" version --short 2>/dev/null || echo "helm: built for ${platform}, not run here"
"$dest/trivy${exe}" --version 2>/dev/null | head -1 || true
"$dest/kustomize${exe}" version 2>/dev/null || true
