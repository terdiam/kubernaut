<#
.SYNOPSIS
  Fail if a Windows executable needs the Visual C++ runtime DLLs, or, with
  -RequireAdministrator, lacks the run-as-administrator manifest.

.DESCRIPTION
  scripts\check-windows-crt.ps1 <path to .exe>

  A clean Windows install has no VCRUNTIME140.dll or MSVCP140.dll, so an
  executable that imports them does not start there. The build links the C
  runtime in statically (.cargo/config.toml); this catches that going missing.
#>
param(
  [Parameter(Mandatory = $true)][string]$Binary,
  # Also require the run-as-administrator manifest (see set-windows-manifest.ps1).
  [switch]$RequireAdministrator
)
$ErrorActionPreference = "Stop"
if (-not (Test-Path $Binary)) { throw "no executable at $Binary" }

# DLL names sit in the import table as plain ASCII.
$text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($Binary))
$found = [regex]::Matches($text, '(?i)\b(vcruntime\d+(_\d+)?|msvcp\d+(_\d+)?|vcomp\d+|concrt\d+)\.dll') |
  ForEach-Object { $_.Value.ToLower() } | Sort-Object -Unique
if ($found) {
  throw "$Binary needs the Visual C++ runtime: $($found -join ', '). Link it statically (.cargo/config.toml)."
}
Write-Host "ok: $Binary does not import the Visual C++ runtime DLLs"

# The manifest is plain text in the resource section. The shipped executable asks
# to run as administrator; a development build does not, so this is opt-in.
if ($RequireAdministrator) {
  if ($text -notmatch 'requireAdministrator') {
    throw "$Binary has no requireAdministrator manifest. Run scripts/set-windows-manifest.ps1 on it."
  }
  Write-Host "ok: $Binary asks to run as administrator"
}
