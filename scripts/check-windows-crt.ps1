<#
.SYNOPSIS
  Fail if a Windows executable needs the Visual C++ runtime DLLs.

.DESCRIPTION
  scripts\check-windows-crt.ps1 <path to .exe>

  A clean Windows install has no VCRUNTIME140.dll or MSVCP140.dll, so an
  executable that imports them does not start there. The build links the C
  runtime in statically (.cargo/config.toml); this catches that going missing.
#>
param([Parameter(Mandatory = $true)][string]$Binary)
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
