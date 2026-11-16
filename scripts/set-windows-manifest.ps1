<#
.SYNOPSIS
  Make a Windows executable ask to run as administrator.

.DESCRIPTION
  scripts\set-windows-manifest.ps1 <path to .exe>

  The linked executable already carries GPUI's manifest, which asks for no
  elevation. This replaces it with scripts\kubernaut.manifest, the same manifest
  with the administrator request, using mt.exe from the Windows SDK, and checks
  that the request is in the file afterwards. The shipped executable gets it; a
  development build keeps asking for nothing, so `cargo run` and the tests need
  no elevated terminal.
#>
param([Parameter(Mandatory = $true)][string]$Binary)
$ErrorActionPreference = "Stop"
if (-not (Test-Path $Binary)) { throw "no executable at $Binary" }
$manifest = Join-Path $PSScriptRoot "kubernaut.manifest"

# mt.exe is not on PATH by default; it sits in the newest Windows SDK.
$mt = (Get-Command mt.exe -ErrorAction SilentlyContinue).Source
if (-not $mt) {
  $roots = @("${env:ProgramFiles(x86)}\Windows Kits\10\bin", "${env:ProgramFiles}\Windows Kits\10\bin") | Where-Object { Test-Path $_ }
  $mt = $roots | ForEach-Object { Get-ChildItem -Path $_ -Filter mt.exe -Recurse -ErrorAction SilentlyContinue } |
    Where-Object { $_.FullName -match '\\x64\\' } |
    Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $mt) { throw "mt.exe not found: install the Windows SDK (it comes with the Visual Studio C++ build tools)" }

& $mt -nologo -manifest $manifest "-outputresource:$Binary;#1"
if ($LASTEXITCODE -ne 0) { throw "mt.exe failed with exit code $LASTEXITCODE" }
Write-Host "ok: $Binary now asks to run as administrator"
