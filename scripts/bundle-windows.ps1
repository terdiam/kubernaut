<#
.SYNOPSIS
  Build Kubernaut for Windows and pack it as a zip.

.DESCRIPTION
  scripts\bundle-windows.ps1 [-Sidecars <dir>] [-Binary <path>]

  -Sidecars  copy helm.exe, trivy.exe and kustomize.exe from <dir> beside the
             executable, where the app looks for them
  -Binary    use an already built kubernaut-ui.exe instead of building one

  The zip unpacks to Kubernaut\ and holds the executable and the sidecars.
  Run kubernaut-ui.exe from there; nothing is installed and nothing needs
  administrator rights. The build is not code-signed, so SmartScreen warns on
  the first run: More info, then Run anyway.
#>
param(
  [string]$Sidecars = "",
  [string]$Binary = ""
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $version) { throw "no version in Cargo.toml" }

if (-not $Binary) {
  cargo build --release -p kubernaut-ui
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
  $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
  $Binary = Join-Path $targetDir "release\kubernaut-ui.exe"
}
if (-not (Test-Path $Binary)) { throw "no executable at $Binary" }

$stage = "dist\Kubernaut"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $Binary (Join-Path $stage "kubernaut-ui.exe")

if ($Sidecars) {
  foreach ($tool in "helm", "trivy", "kustomize") {
    $source = Join-Path $Sidecars "$tool.exe"
    if (-not (Test-Path $source)) { throw "missing $source" }
    Copy-Item $source (Join-Path $stage "$tool.exe")
  }
}

$zip = "dist\Kubernaut-$version-windows-x86_64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "built $zip"
