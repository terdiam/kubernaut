<#
.SYNOPSIS
  Install what Kubernaut needs from outside itself, if it is missing: kubectl and
  the kubectl oidc-login plugin (kubelogin), which OIDC kubeconfigs call.

.DESCRIPTION
  install-prerequisites.ps1

  Runs as the current user and needs no administrator rights. Everything goes to
  %LOCALAPPDATA%\Kubernaut\bin, which is added to the user PATH. Each download is
  checked against the SHA-256 its publisher posts beside it. A tool that is already
  on PATH is left alone. The setup program runs this as an optional step; it is
  safe to run again at any time. A failure is reported and does not stop the
  install: Kubernaut itself does not need either tool to start.
#>
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = "SilentlyContinue"

$bin = Join-Path $env:LOCALAPPDATA "Kubernaut\bin"
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
$failed = $false

function Test-OnPath([string]$name) {
  [bool](Get-Command $name -ErrorAction SilentlyContinue) -or (Test-Path (Join-Path $bin "$name.exe"))
}

function Get-Checked([string]$url, [string]$expected, [string]$file) {
  Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $file
  $actual = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLower()
  # A .sha256 file is the hash alone or "hash *name"; the hash is the first word.
  $want = ($expected.Trim() -split '\s+')[0].ToLower()
  if ($actual -ne $want) {
    Remove-Item -Force $file
    throw "checksum mismatch for $url"
  }
}

function Add-ToUserPath([string]$dir) {
  $current = [Environment]::GetEnvironmentVariable("Path", "User")
  $entries = if ($current) { $current -split ';' } else { @() }
  if ($entries -notcontains $dir) {
    [Environment]::SetEnvironmentVariable("Path", (($entries + $dir) | Where-Object { $_ }) -join ';', "User")
  }
}

function Install-Kubectl {
  if (Test-OnPath "kubectl") { Write-Host "kubectl: already installed"; return }
  Write-Host "kubectl: downloading"
  $version = (Invoke-RestMethod -UseBasicParsing "https://dl.k8s.io/release/stable.txt").Trim()
  $base = "https://dl.k8s.io/release/$version/bin/windows/$arch/kubectl.exe"
  $hash = Invoke-RestMethod -UseBasicParsing "$base.sha256"
  $tmp = Join-Path $env:TEMP "kubectl-$version.exe"
  Get-Checked $base $hash $tmp
  Move-Item -Force $tmp (Join-Path $bin "kubectl.exe")
  Write-Host "kubectl: installed $version"
}

function Install-OidcLogin {
  # kubectl finds a plugin by its file name: kubectl-oidc_login.exe is `kubectl oidc-login`.
  if ((Test-OnPath "kubectl-oidc_login")) { Write-Host "oidc-login: already installed"; return }
  Write-Host "oidc-login: downloading"
  $base = "https://github.com/int128/kubelogin/releases/latest/download/kubelogin_windows_$arch.zip"
  $hash = Invoke-RestMethod -UseBasicParsing "$base.sha256"
  $zip = Join-Path $env:TEMP "kubelogin.zip"
  Get-Checked $base $hash $zip
  $out = Join-Path $env:TEMP "kubelogin-extract"
  if (Test-Path $out) { Remove-Item -Recurse -Force $out }
  Expand-Archive -Path $zip -DestinationPath $out
  Copy-Item -Force (Join-Path $out "kubelogin.exe") (Join-Path $bin "kubectl-oidc_login.exe")
  Remove-Item -Recurse -Force $out, $zip
  Write-Host "oidc-login: installed"
}

New-Item -ItemType Directory -Force $bin | Out-Null
foreach ($step in "Install-Kubectl", "Install-OidcLogin") {
  try { & $step }
  catch { $failed = $true; Write-Warning "$step failed: $($_.Exception.Message)" }
}
if (Get-ChildItem $bin -Filter *.exe -ErrorAction SilentlyContinue) { Add-ToUserPath $bin }
if ($failed) { exit 1 }
