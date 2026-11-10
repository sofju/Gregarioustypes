<#
  Builds the installer: target\installer\Gregarioustypes-<version>-setup.exe.
  Needs Inno Setup 7 (ISCC.exe): deps\tools\innosetup (scripts\fetch-deps.ps1
  -Only innosetup installs it there, portable), or an installed copy; and Python 3
  for scripts\collect-licenses.py (the licence texts shipped in {app}\licenses).
  Usage:  powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-NoBuild]
#>
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $NoBuild) { & (Join-Path $PSScriptRoot 'build.ps1') }

$iscc = @(
  (Join-Path $root 'deps\tools\innosetup\ISCC.exe'),
  "$env:LOCALAPPDATA\Programs\Inno Setup 7\ISCC.exe",
  "$env:ProgramFiles\Inno Setup 7\ISCC.exe",
  "${env:ProgramFiles(x86)}\Inno Setup 7\ISCC.exe",
  "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
  "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup (ISCC.exe) not found: run scripts\fetch-deps.ps1 -Only innosetup' }

# Licence texts of the app and of everything it bundles.
& python (Join-Path $PSScriptRoot 'collect-licenses.py')
if ($LASTEXITCODE -ne 0) { throw 'collect-licenses.py failed' }

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$icon = Get-ChildItem (Join-Path $root 'target\release\build') -Recurse -Filter *.ico -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
$isccArgs = @("/DAppVersion=$version", "/DRoot=$root")
if ($icon) { $isccArgs += "/DIcon=$($icon.FullName)" }
& $iscc @isccArgs (Join-Path $root 'installer\Gregarioustypes.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
Get-ChildItem (Join-Path $root 'target\installer') -Filter *.exe | Select-Object Name, @{n='MB';e={[math]::Round($_.Length/1MB)}}
