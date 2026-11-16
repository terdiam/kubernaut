<#
.SYNOPSIS
  Build Kubernaut for Windows and pack it as a zip.

.DESCRIPTION
  scripts\bundle-windows.ps1 [-Sidecars <dir>] [-Binary <path>] [-Installer]

  -Sidecars  copy helm.exe, trivy.exe and kustomize.exe from <dir> beside the
             executable, where the app looks for them
  -Binary    use an already built kubernaut-ui.exe instead of building one
  -Installer also build the setup program with Inno Setup (ISCC.exe), which has
             to be installed; without this switch only the zip is built

  Two files come out of dist\:

    Kubernaut-<version>-windows-x86_64.zip        unpacks to Kubernaut\ with the
      executable and the sidecars. Run kubernaut-ui.exe from there. This is the
      file the in-app updater downloads.
    Kubernaut-<version>-windows-x86_64-setup.exe  installs for the current user
      under %LOCALAPPDATA%\Programs\Kubernaut, adds a Start menu entry and an
      uninstaller. Nothing needs administrator rights.

  The build is not code-signed, so SmartScreen warns on the first run:
  More info, then Run anyway.
#>
param(
  [string]$Sidecars = "",
  [string]$Binary = "",
  [switch]$Installer
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
& (Join-Path $PSScriptRoot "check-windows-crt.ps1") -Binary $Binary

$stage = "dist\Kubernaut"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $Binary (Join-Path $stage "kubernaut-ui.exe")
# The shipped copy asks to run as administrator so the self-update can replace its
# own files; the build in target\ is left as it is.
& (Join-Path $PSScriptRoot "set-windows-manifest.ps1") -Binary (Join-Path $stage "kubernaut-ui.exe")
& (Join-Path $PSScriptRoot "check-windows-crt.ps1") -Binary (Join-Path $stage "kubernaut-ui.exe") -RequireAdministrator
# Marks the folder as an install, and names the version, for the in-app updater.
"dev.kubernaut.app $version" | Set-Content -Encoding ascii (Join-Path $stage "kubernaut.package")

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

if ($Installer) {
  $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
  if (-not $iscc) {
    $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
      Where-Object { Test-Path $_ } | Select-Object -First 1
  }
  if (-not $iscc) { throw "Inno Setup is not installed (ISCC.exe not found)" }
  & $iscc "/DAppVersion=$version" "scripts\windows-installer.iss"
  if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }
  Write-Host "built dist\Kubernaut-$version-windows-x86_64-setup.exe"
}
