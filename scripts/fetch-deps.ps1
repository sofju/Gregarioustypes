<#
  Downloads the prebuilt runtime dependencies into deps/.
  Each download lands in its own folder under deps/_dl/<name>/ (untrusted data),
  then is extracted to deps/<name>/. SHA-256 hashes are pinned in scripts/deps.lock
  (committed with the source) and verified on every run; a new download is added
  to it on first use.
  7z.dll is built from 7-Zip's source without its RAR decoders (needs the Visual
  Studio C++ build tools); see the '7zip-src' step.
  Usage:  powershell -ExecutionPolicy Bypass -File scripts\fetch-deps.ps1 [-Only mpv,pdfium] [-Force]
#>
param([string[]]$Only, [switch]$Force)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent $PSScriptRoot
$deps = Join-Path $root 'deps'
$dl   = Join-Path $deps '_dl'
$lockFile = Join-Path $PSScriptRoot 'deps.lock'
New-Item -ItemType Directory -Force $dl | Out-Null

$items = @(
  @{ name='7zr';       url='https://github.com/ip7z/7zip/releases/download/26.06/7zr.exe';                                   file='7zr.exe' },
  @{ name='7zip';      url='https://github.com/ip7z/7zip/releases/download/26.06/7z2606-x64.exe';                            file='7z2606-x64.exe' },
  @{ name='7zip-src';  url='https://github.com/ip7z/7zip/releases/download/26.06/7z2606-src.7z';                             file='7z2606-src.7z' },
  @{ name='mpv';       url='https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261107/mpv-dev-x86_64-20261107-git-9028187538.7z'; file='mpv-dev-x86_64-20261107-git-9028187538.7z' },
  @{ name='pdfium';    url='https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8138/pdfium-win-x64.tgz'; file='pdfium-win-x64.tgz' },
  @{ name='mediainfo'; url='https://mediaarea.net/download/binary/libmediainfo0/26.10/MediaInfo_DLL_26.10_Windows_x64_WithoutInstaller.zip'; file='MediaInfo_DLL.zip' },
  @{ name='fonts';     url='https://int10h.org/oldschool-pc-fonts/download/oldschool_pc_font_pack_v2.2_win.zip';              file='oldschool_pc_font_pack.zip' },
  @{ name='soundfont'; url='https://github.com/mrbumpy409/GeneralUser-GS/raw/684543d5e5efaef08d02be50dcda8d552478fa60/GeneralUser-GS.sf2'; file='GeneralUser-GS.sf2' },
  @{ name='soundfont-license'; url='https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/684543d5e5efaef08d02be50dcda8d552478fa60/documentation/LICENSE.txt'; file='GeneralUser-GS-LICENSE.txt' },
  @{ name='ghostscript'; url='https://github.com/ArtifexSoftware/ghostpdl-downloads/releases/download/gs10080/gs10080w64.exe'; file='gs10080w64.exe' },
  @{ name='ruffle';    url='https://github.com/ruffle-rs/ruffle/releases/download/nightly-2026-11-08/ruffle-nightly-2026_11_08-web-selfhosted.zip'; file='ruffle-web.zip' },
  @{ name='libreoffice'; url='https://download.documentfoundation.org/libreoffice/stable/26.8.2/win/x86_64/LibreOffice_26.8.2_Win_x86-64.msi'; file='LibreOffice_26.8.2_Win_x86-64.msi' },
  @{ name='three';     url='https://registry.npmjs.org/three/-/three-0.187.0.tgz';                                            file='three-0.187.0.tgz' },
  # Build tool only (installer compiler), installed portable into deps\tools.
  @{ name='innosetup'; url='https://github.com/jrsoftware/issrc/releases/download/is-7_1_1/innosetup-7.1.1-x64.exe';      file='innosetup-7.1.1-x64.exe' },
  @{ name='blender';   url='https://download.blender.org/release/Blender5.2/blender-5.2.3-windows-x64.zip';                 file='blender-5.2.3-windows-x64.zip';
     sha='723f08a4401502561eb763cdeacf8745b31e5f3f154d287d3830d97cf75dc2fb' }
)

$lock = @{}
if (Test-Path $lockFile) {
  Get-Content $lockFile | ForEach-Object { if ($_ -match '^(\S+)\s+([0-9a-f]{64})$') { $lock[$Matches[1]] = $Matches[2] } }
}

function Get-Dep($it) {
  $dir = Join-Path $dl $it.name
  $out = Join-Path $dir $it.file
  if ($Force -and (Test-Path $dir)) { Remove-Item -Recurse -Force $dir }
  if (-not (Test-Path $out)) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    Write-Host "Downloading $($it.name) ..."
    & curl.exe -L --fail --retry 3 -s -o $out $it.url
    if ($LASTEXITCODE -ne 0) { throw "download failed: $($it.url)" }
  }
  $hash = (Get-FileHash -Algorithm SHA256 $out).Hash.ToLower()
  # Publisher checksum when there is one, else trust on first use.
  if ($it.sha -and $it.sha -ne $hash) { throw "hash mismatch for $($it.file): published $($it.sha) got $hash" }
  if ($lock.ContainsKey($it.file)) {
    if ($lock[$it.file] -ne $hash) { throw "hash mismatch for $($it.file): expected $($lock[$it.file]) got $hash" }
  } else { $lock[$it.file] = $hash }
  return $out
}

$sevenZr = Join-Path $deps 'tools\7zr.exe'
$sevenZ  = Join-Path $deps '7zip\7z.exe'

foreach ($it in $items) {
  if ($Only -and ($Only -notcontains $it.name)) { continue }
  $file = Get-Dep $it
  $dest = Join-Path $deps $it.name
  switch ($it.name) {
    '7zr'       { New-Item -ItemType Directory -Force (Split-Path $sevenZr) | Out-Null; Copy-Item $file $sevenZr -Force }
    # The official 7-Zip: 7z.exe runs as a separate process for files inside RAR archives.
    '7zip'      { if (-not (Test-Path $sevenZ)) { & $sevenZr x $file "-o$dest" -y | Out-Null } }
    # 7z.dll for the app's own process, built without CPP/7zip/Compress/Rar* (the
    # only files under the unRAR licence restriction, which isn't GPL-compatible):
    # pure LGPL. RAR archives still list; their files come out through 7z.exe.
    '7zip-src' {
      $out = Join-Path $deps '7zip-lgpl'
      if (-not (Test-Path (Join-Path $out '7z.dll'))) {
        $src = Join-Path $dl '7zip-src\src'
        if (Test-Path $src) { Remove-Item -Recurse -Force $src }
        & $sevenZr x $file "-o$src" -y | Out-Null
        if ($LASTEXITCODE -ne 0) { throw '7-Zip source extraction failed' }
        $mak = Join-Path $src 'CPP\7zip\Bundles\Format7zF\Arc.mak'
        $lines = Get-Content $mak
        $kept = @($lines | Where-Object { $_ -notmatch '^\s*\$O[\\/](Rar\dDecoder|Rar3Vm|RarCodecsRegister)\.obj' })
        if ($lines.Count - $kept.Count -ne 6) { throw "Arc.mak: expected 6 RAR decoder objects, found $($lines.Count - $kept.Count)" }
        (Get-Item $mak).IsReadOnly = $false
        Set-Content -Encoding ascii $mak $kept
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (-not $vs) { throw 'Visual Studio C++ build tools not found (needed to build 7z.dll)' }
        $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
        $bundle = Join-Path $src 'CPP\7zip\Bundles\Format7zF'
        & cmd.exe /c "call `"$vcvars`" >nul && cd /d `"$bundle`" && nmake /nologo PLATFORM=x64"
        if ($LASTEXITCODE -ne 0) { throw '7z.dll build failed' }
        New-Item -ItemType Directory -Force $out | Out-Null
        Copy-Item (Join-Path $bundle 'x64\7z.dll') (Join-Path $out '7z.dll') -Force
        Copy-Item (Join-Path $src 'DOC\License.txt') (Join-Path $out 'License.txt') -Force
        Set-Content -Encoding ascii (Join-Path $out 'README.txt') @(
          '7z.dll built from the 7-Zip 26.06 source (https://github.com/ip7z/7zip/releases/tag/26.06)',
          'by scripts\fetch-deps.ps1 in https://github.com/sofju/Gregarioustypes.',
          'The only change: the RAR decoders (CPP/7zip/Compress/Rar*) are left out of the build,',
          'so this library contains no code under the unRAR licence restriction: it is GNU LGPL',
          '(with the BSD-licensed parts listed in License.txt).')
      }
    }
    'mpv'       { if (-not (Test-Path (Join-Path $dest 'libmpv-2.dll'))) { & $sevenZ x $file "-o$dest" -y | Out-Null } }
    'pdfium'    { if (-not (Test-Path (Join-Path $dest 'bin\pdfium.dll'))) { New-Item -ItemType Directory -Force $dest | Out-Null; & tar.exe -xzf $file -C $dest } }
    'mediainfo' { if (-not (Test-Path (Join-Path $dest 'MediaInfo.dll'))) { New-Item -ItemType Directory -Force $dest | Out-Null; & tar.exe -xf $file -C $dest } }
    'fonts'     { if (-not (Test-Path $dest)) { New-Item -ItemType Directory -Force $dest | Out-Null; & tar.exe -xf $file -C $dest } }
    'soundfont' { New-Item -ItemType Directory -Force $dest | Out-Null; Copy-Item $file (Join-Path $dest $it.file) -Force }
    'soundfont-license' { $sf = Join-Path $deps 'soundfont'; New-Item -ItemType Directory -Force $sf | Out-Null; Copy-Item $file (Join-Path $sf 'LICENSE.txt') -Force }
    # NSIS installer: 7-Zip unpacks it without running it.
    'ghostscript' { if (-not (Test-Path (Join-Path $dest 'bin\gswin64c.exe'))) { & $sevenZ x $file "-o$dest" -y | Out-Null; Remove-Item -Recurse -Force (Join-Path $dest '$PLUGINSDIR') -ErrorAction SilentlyContinue } }
    'ruffle'    { if (-not (Test-Path (Join-Path $dest 'ruffle.js'))) { New-Item -ItemType Directory -Force $dest | Out-Null; & tar.exe -xf $file -C $dest } }
    # npm package: the module build and the example add-ons (loaders, controls, decoders).
    'three' {
      if (-not (Test-Path (Join-Path $dest 'build\three.module.js'))) {
        New-Item -ItemType Directory -Force $dest | Out-Null
        & tar.exe -xzf $file -C $dest --strip-components=1 package/build package/examples/jsm package/LICENSE
      }
    }
    # Portable mode: no uninstaller, no Start menu entries, no file associations.
    'innosetup' {
      $dir = Join-Path $deps 'tools\innosetup'
      if (-not (Test-Path (Join-Path $dir 'ISCC.exe'))) {
        $p = Start-Process $file -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', '/PORTABLE=1', '/NOICONS', "/DIR=`"$dir`"") -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "Inno Setup install failed: $($p.ExitCode)" }
      }
    }
    # Portable zip: one folder, no installer, no registry.
    'blender' {
      if (-not (Test-Path (Join-Path $dest 'blender.exe'))) {
        $stage = Join-Path $dl 'blender\x'
        if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
        & $sevenZ x $file "-o$stage" -y | Out-Null
        $inner = Get-ChildItem $stage -Recurse -Filter blender.exe | Select-Object -First 1
        if (-not $inner) { throw 'blender.exe not found after extraction' }
        Move-Item (Split-Path -Parent $inner.FullName) $dest
        Remove-Item -Recurse -Force $stage
      }
    }
    # Administrative install: unpacks the MSI's files into a folder, no system changes.
    'libreoffice' {
      if (-not (Test-Path (Join-Path $dest 'program\soffice.exe'))) {
        $stage = Join-Path $dl 'libreoffice\admin'
        $p = Start-Process msiexec.exe -ArgumentList @('/a', "`"$file`"", '/qn', "TARGETDIR=`"$stage`"") -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "msiexec /a failed: $($p.ExitCode)" }
        $inner = Get-ChildItem $stage -Recurse -Filter soffice.exe | Select-Object -First 1
        if (-not $inner) { throw 'soffice.exe not found after extraction' }
        Move-Item (Split-Path -Parent (Split-Path -Parent $inner.FullName)) $dest
        if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
      }
      # The admin image copies the MSI itself; the fonts meant for C:\Windows\Fonts
      # go to LibreOffice's private font folder instead.
      Get-ChildItem $dest -Filter *.msi | Remove-Item -Force
      $fonts = Join-Path $dest 'Fonts'
      if (Test-Path $fonts) {
        $priv = Join-Path $dest 'share\fonts\truetype'
        New-Item -ItemType Directory -Force $priv | Out-Null
        Get-ChildItem $fonts -File | Move-Item -Destination $priv -Force
        Remove-Item -Recurse -Force $fonts
      }
      # VC++ runtime meant for System32: deployed app-locally next to soffice.exe.
      $sys64 = Join-Path $dest 'System64'
      if (Test-Path $sys64) {
        Get-ChildItem $sys64 -Filter *.dll | ForEach-Object {
          $t = Join-Path $dest "program\$($_.Name)"
          if (-not (Test-Path $t)) { Copy-Item $_.FullName $t }
        }
        Remove-Item -Recurse -Force $sys64
      }
      $sys32 = Join-Path $dest 'System'
      if (Test-Path $sys32) { Remove-Item -Recurse -Force $sys32 }
    }
  }
  Write-Host "ok  $($it.name)"
}

# Only the files listed above (a full run also drops entries of replaced downloads).
$known = $items | ForEach-Object { $_.file }
if (-not $Only) { foreach ($k in @($lock.Keys)) { if ($known -notcontains $k) { $lock.Remove($k) } } }
$lock.GetEnumerator() | Sort-Object Name | ForEach-Object { "{0} {1}" -f $_.Name, $_.Value } | Set-Content -Encoding ascii $lockFile
Write-Host "done"
