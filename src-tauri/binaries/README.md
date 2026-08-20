# Bundled binaries

CI places the verified `helm` binary here before packaging (see
`.github/workflows/release.yml`), and the app looks for it in the bundle's
resource directory before falling back to the user's own `helm` on `PATH`.

This file exists so the directory is present in a source checkout: Tauri's
`resources` glob fails the build when it matches nothing, and a development
build has no sidecar. Binaries themselves are never committed.
