#!/usr/bin/env bash
#
# Finish a release with the macOS build.
#
# CI builds Windows and Linux only (macOS needs a Developer ID certificate the
# workflow does not have), so the macOS half of a release is built here and added
# to the draft CI leaves behind. In order, this:
#
#   1. checks the tag, the version in tauri.conf.json and that CI has finished
#   2. builds and signs the universal app          (skipped with --skip-build)
#   3. checks the signature was made by the key the app trusts
#   4. uploads the .dmg, the updater archive and its .sig to the release
#   5. adds the darwin-* entries to latest.json and uploads it
#   6. with --publish: publishes the release and checks latest.json live
#
# It does not publish unless asked — the workflow makes a draft on purpose, so a
# person looks at the artefacts before the updater can serve them.
#
# Usage:  scripts/release-macos.sh <tag> [--skip-build] [--publish] [--force]
#
#   <tag>         the release tag, e.g. v0.1.12; must match tauri.conf.json
#   --skip-build  reuse the bundle already in target/ instead of rebuilding
#   --publish     publish the release afterwards and verify the live manifest
#   --force       replace macOS assets that are already on the release
#
# Environment:
#   TAURI_SIGNING_PRIVATE_KEY_PATH      updater key (default ~/.kubernaut-updater.key)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  its password; asked for when unset
#   REPO                                owner/name (default: the current repo)
#
# Safe to re-run: assets already uploaded are left alone and latest.json is only
# rewritten when its macOS entries are missing or stale.

set -euo pipefail

TARGET=universal-apple-darwin
ARCHIVE=Kubernaut.app.tar.gz

die() {
  echo "error: $*" >&2
  exit 1
}

step() {
  printf '\n==> %s\n' "$*"
}

TAG=""
SKIP_BUILD=0
PUBLISH=0
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    --publish) PUBLISH=1 ;;
    --force) FORCE=1 ;;
    -h | --help)
      sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*) die "unknown option: $arg" ;;
    *) [ -z "$TAG" ] || die "more than one tag given"; TAG="$arg" ;;
  esac
done

[ -n "$TAG" ] || die "usage: $0 <tag> [--skip-build] [--publish] [--force]"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "tag must look like v1.2.3, got '$TAG'"
VERSION="${TAG#v}"

cd "$(git rev-parse --show-toplevel)"
for tool in gh python3 curl; do
  command -v "$tool" >/dev/null || die "$tool is required"
done
gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
REPO="${REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}"

BUNDLE="target/$TARGET/release/bundle"
DMG="$BUNDLE/dmg/Kubernaut_${VERSION}_universal.dmg"
TARBALL="$BUNDLE/macos/$ARCHIVE"
SIG="$TARBALL.sig"

# ---- 1. preconditions ------------------------------------------------------

step "Checking $TAG"

CONF_VERSION=$(python3 -c 'import json; print(json.load(open("src-tauri/tauri.conf.json"))["version"])')
[ "$CONF_VERSION" = "$VERSION" ] ||
  die "tauri.conf.json says $CONF_VERSION but the tag is $TAG — the bundle would carry the wrong version"

git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "tag $TAG does not exist locally"
[ "$(git rev-parse "$TAG^{commit}")" = "$(git rev-parse HEAD)" ] ||
  die "HEAD is not at $TAG — check out the tagged commit so the build matches the release"
[ -z "$(git status --porcelain --untracked-files=no)" ] || die "the working tree has uncommitted changes"

gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1 ||
  die "no release for $TAG on $REPO — has the release workflow finished?"

# `gh release upload` names an asset after the file, so the manifest to upload
# has to be called latest.json — hence a directory rather than loose temp files.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
MANIFEST="$WORK/current.json"
gh release download "$TAG" --repo "$REPO" -p latest.json -O "$MANIFEST" --clobber ||
  die "the release has no latest.json — the workflow has not finished uploading"

python3 - "$MANIFEST" "$VERSION" <<'EOF' || die "latest.json is not ready (see above)"
import json, sys
manifest, version = json.load(open(sys.argv[1])), sys.argv[2]
if manifest.get("version") != version:
    sys.exit(f"latest.json is for {manifest.get('version')}, not {version}")
platforms = [p for p in manifest["platforms"] if not p.startswith("darwin")]
if not platforms:
    sys.exit("latest.json has no Windows/Linux entries yet — CI is still running")
print(f"latest.json is for {version}, {len(platforms)} non-macOS platforms")
EOF

# ---- 2. build --------------------------------------------------------------

if [ "$SKIP_BUILD" -eq 0 ]; then
  step "Building $TARGET"

  KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.kubernaut-updater.key}"
  [ -r "$KEY_PATH" ] || die "updater key not found at $KEY_PATH"
  if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then
    [ -t 0 ] || die "TAURI_SIGNING_PRIVATE_KEY_PASSWORD is unset and there is no terminal to ask on"
    read -rsp "Updater key password: " TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    echo
  fi

  # Without both targets `tauri build --target universal-apple-darwin` fails
  # late, after the first architecture has already compiled.
  for triple in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target list --installed | grep -qx "$triple" || rustup target add "$triple"
  done

  TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")" \
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$TAURI_SIGNING_PRIVATE_KEY_PASSWORD" \
    npx tauri build --target "$TARGET"
else
  step "Reusing the existing bundle (--skip-build)"
fi

for file in "$DMG" "$TARBALL" "$SIG"; do
  [ -f "$file" ] || die "missing $file — run without --skip-build"
done

# ---- 3. signature ----------------------------------------------------------

step "Checking the signature against the app's public key"

python3 - "$SIG" <<'EOF' || die "the updater signature was not made by the key in tauri.conf.json"
import base64, json, sys

def key_id_of_signature(text):
    # A minisign signature is a comment line, then base64 of: 2 bytes algorithm,
    # 8 bytes key id (little endian), then the signature itself.
    body = base64.b64decode(text.strip()).decode().splitlines()[1]
    return base64.b64decode(body)[2:10][::-1].hex().upper()

conf = json.load(open("src-tauri/tauri.conf.json"))["plugins"]["updater"]["pubkey"]
trusted = base64.b64decode(conf).decode().splitlines()[0].rsplit(": ", 1)[-1].upper()
signed_with = key_id_of_signature(open(sys.argv[1]).read())
if signed_with != trusted:
    sys.exit(f"signed with {signed_with}, but the app trusts {trusted} — installed apps would reject this update")
print(f"signed with {signed_with}, the key the app trusts")
EOF

# ---- 4. upload -------------------------------------------------------------

step "Uploading to $TAG"

existing=$(gh release view "$TAG" --repo "$REPO" --json assets -q '.assets[].name')
to_upload=()
for file in "$DMG" "$TARBALL" "$SIG"; do
  name=$(basename "$file")
  if grep -qxF "$name" <<<"$existing" && [ "$FORCE" -eq 0 ]; then
    echo "already there, leaving alone: $name (--force to replace)"
  else
    to_upload+=("$file")
  fi
done
if [ "${#to_upload[@]}" -gt 0 ]; then
  gh release upload "$TAG" --repo "$REPO" --clobber "${to_upload[@]}"
fi

# ---- 5. latest.json --------------------------------------------------------

step "Adding the macOS entries to latest.json"

# A universal app serves both architectures, and the updater asks for either the
# plain platform key or the -app one depending on how it was installed.
if python3 - "$MANIFEST" "$WORK/latest.json" "$SIG" "https://github.com/$REPO/releases/download/$TAG/$ARCHIVE" <<'EOF'; then
import json, sys

manifest_path, out_path, sig_path, url = sys.argv[1:5]
manifest = json.load(open(manifest_path))
signature = open(sig_path).read().strip()

wanted = {"signature": signature, "url": url}
keys = ["darwin-aarch64", "darwin-aarch64-app", "darwin-x86_64", "darwin-x86_64-app"]
if all(manifest["platforms"].get(key) == wanted for key in keys):
    sys.exit(1)  # nothing to change

for key in keys:
    manifest["platforms"][key] = dict(wanted)
json.dump(manifest, open(out_path, "w"), indent=2)
EOF
  gh release upload "$TAG" --repo "$REPO" --clobber "$WORK/latest.json"
  echo "latest.json updated"
else
  echo "latest.json already has the current macOS entries"
fi

# ---- 6. publish -------------------------------------------------------------

DRAFT=$(gh release view "$TAG" --repo "$REPO" --json isDraft -q .isDraft)
if [ "$PUBLISH" -eq 0 ]; then
  if [ "$DRAFT" = "true" ]; then
    cat <<EOF

Done — $TAG is ready but still a draft. Look it over, then publish with:

  gh release edit $TAG --repo $REPO --draft=false --latest

or re-run with --publish to publish and verify latest.json live.
EOF
  else
    echo
    echo "Done — $TAG is already published; re-run with --publish to verify latest.json live."
  fi
  exit 0
fi

step "Publishing $TAG"
gh release edit "$TAG" --repo "$REPO" --draft=false --latest >/dev/null

step "Checking the live latest.json"
LIVE="$WORK/live.json"

# The release CDN can lag the API for a few seconds.
for _ in 1 2 3 4 5 6; do
  curl -fsSL "https://github.com/$REPO/releases/latest/download/latest.json" -o "$LIVE" || true
  if python3 - "$LIVE" "$VERSION" <<'EOF'; then
import json, sys
manifest = json.load(open(sys.argv[1]))
assert manifest["version"] == sys.argv[2], f"live version is {manifest['version']}"
missing = [k for k in ("darwin-aarch64", "darwin-x86_64") if k not in manifest["platforms"]]
assert not missing, f"missing {missing}"
empty = [k for k, v in manifest["platforms"].items() if not v.get("signature") or not v.get("url")]
assert not empty, f"no signature or url for {empty}"
print(f"live: {manifest['version']}, {len(manifest['platforms'])} platforms, all signed")
EOF
    exit 0
  fi
  sleep 5
done
die "the live latest.json is not serving $VERSION yet — check https://github.com/$REPO/releases/latest"
