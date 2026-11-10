<#
  Builds the release app: Gregarioustypes.exe and gt_dialoghook.dll (64-bit),
  then the 32-bit file-dialog helper (gt_dialoghook32.exe + .dll) placed next
  to them in target\release.
  Usage:  powershell -ExecutionPolicy Bypass -File scripts\build.ps1
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
  cargo build --release -p gt-app -p gt-dialoghook
  if ($LASTEXITCODE -ne 0) { throw 'x64 build failed' }
  rustup target add i686-pc-windows-msvc | Out-Null
  cargo build --release -p gt-dialoghook --target i686-pc-windows-msvc --features helper32
  if ($LASTEXITCODE -ne 0) { throw 'x86 helper build failed' }
  $x86 = Join-Path $root 'target\i686-pc-windows-msvc\release'
  $out = Join-Path $root 'target\release'
  Copy-Item (Join-Path $x86 'gt_dialoghook32.exe') $out -Force
  Copy-Item (Join-Path $x86 'gt_dialoghook.dll') (Join-Path $out 'gt_dialoghook32.dll') -Force
  Write-Host 'ok'
} finally { Pop-Location }
