# Releasing

Everything below is one-time setup except the last section.

## 1. Update signing key

The auto-updater refuses any update it cannot verify. Generate the key pair
once:

```bash
npm run tauri signer generate -- -w ~/.kubernaut-updater.key
```

- Put the **public** key in `src-tauri/tauri.conf.json` under
  `plugins.updater.pubkey`.
- Put the **private** key and its password in the repository secrets as
  `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Losing the private key means no existing installation can ever be updated
again — they will reject every future release. Back it up somewhere that is not
this repository.

While `pubkey` is empty the updater rejects everything, which is the intended
state: an update channel nobody can verify is worse than no update channel.

## 2. macOS signing and notarisation

Without these, macOS quarantines the download and refuses to open it.

| Secret | What it is |
| --- | --- |
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | password for that `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | Apple ID used for notarisation |
| `APPLE_PASSWORD` | an app-specific password, not the account password |
| `APPLE_TEAM_ID` | the 10-character team identifier |

Requires a paid Apple Developer account.

## 3. Windows signing

Set `WINDOWS_CERTIFICATE` and `WINDOWS_CERTIFICATE_PASSWORD` and add the
`signCommand` to the bundle configuration. Unsigned Windows builds trigger a
SmartScreen warning; they still run.

## 4. Sidecar binaries

`helm` is fetched and checksum-verified during the build (see
`.github/workflows/release.yml`) rather than committed. Bumping its version
means editing `HELM_VERSION` in that workflow, which keeps the change
reviewable.

Trivy is **not** bundled: its vulnerability database is a separate ~110 MiB
download that has to happen on the user's machine anyway, so shipping the binary
would add 200 MB to the installer without removing that step.

## 5. Cutting a release

```bash
git tag v0.1.0
git push origin v0.1.0
```

The workflow builds macOS (universal), Windows and Linux, and opens a **draft**
release. Check the artefacts, then publish. Publishing is what makes the signed
`latest.json` visible to installed copies.

Users who have not enabled update checks in Settings are never contacted.
