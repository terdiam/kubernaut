#!/usr/bin/env bash
#
# Download helm, trivy and kustomize for one platform into a directory, checking
# each archive against the checksum its publisher ships.
#
# The binaries are fetched here rather than committed, so the repository holds
# none. Pinning the digests in this file as well would also guard against a
# compromised distribution host, at the cost of a manual update on every bump.
#
# Usage: scripts/fetch-sidecars.sh <darwin-arm64|darwin-amd64> <destination-dir>

set -euo pipefail

HELM_VERSION="${HELM_VERSION:-v3.19.0}"
TRIVY_VERSION="${TRIVY_VERSION:-0.74.0}"
KUSTOMIZE_VERSION="${KUSTOMIZE_VERSION:-v5.8.1}"

platform="${1:?usage: fetch-sidecars.sh <darwin-arm64|darwin-amd64> <destination-dir>}"
dest="${2:?usage: fetch-sidecars.sh <darwin-arm64|darwin-amd64> <destination-dir>}"

case "$platform" in
  darwin-arm64) helm_p=darwin-arm64; trivy_p=macOS-ARM64; kustomize_p=darwin_arm64 ;;
  darwin-amd64) helm_p=darwin-amd64; trivy_p=macOS-64bit; kustomize_p=darwin_amd64 ;;
  *) echo "unsupported platform: $platform" >&2; exit 1 ;;
esac

mkdir -p "$dest"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

# Compare an archive with the first field of the line that names it.
verify() { # <archive> <checksum-file>
  local actual expected
  actual=$(shasum -a 256 "$1" | awk '{print $1}')
  expected=$(grep " $1\$" "$2" | awk '{print $1}')
  [ -n "$expected" ] && [ "$actual" = "$expected" ] || { echo "checksum mismatch for $1" >&2; exit 1; }
}

archive="helm-${HELM_VERSION}-${helm_p}.tar.gz"
curl -fsSLO "https://get.helm.sh/${archive}"
curl -fsSL "https://get.helm.sh/${archive}.sha256sum" -o helm.sha
# helm's checksum file names the archive on its own line; match on the hash.
[ "$(shasum -a 256 "$archive" | awk '{print $1}')" = "$(awk '{print $1}' helm.sha)" ] \
  || { echo "checksum mismatch for $archive" >&2; exit 1; }
tar -xzf "$archive"
install -m 755 "${helm_p}/helm" "$dest/helm"

base="https://github.com/aquasecurity/trivy/releases/download/v${TRIVY_VERSION}"
archive="trivy_${TRIVY_VERSION}_${trivy_p}.tar.gz"
curl -fsSLO "${base}/${archive}"
curl -fsSL "${base}/trivy_${TRIVY_VERSION}_checksums.txt" -o trivy.sha
verify "$archive" trivy.sha
tar -xzf "$archive" trivy
install -m 755 trivy "$dest/trivy"

base="https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize/${KUSTOMIZE_VERSION}"
archive="kustomize_${KUSTOMIZE_VERSION}_${kustomize_p}.tar.gz"
curl -fsSLO "${base}/${archive}"
curl -fsSL "${base}/checksums.txt" -o kustomize.sha
verify "$archive" kustomize.sha
tar -xzf "$archive" kustomize
install -m 755 kustomize "$dest/kustomize"

"$dest/helm" version --short
"$dest/trivy" --version | head -1
"$dest/kustomize" version
