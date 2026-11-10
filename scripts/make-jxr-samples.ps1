# JPEG XR samples through WPF's WmpBitmapEncoder (built into Windows):
#   hdr-scrgb.jxr : 128-bit float scRGB gradient 0 → 16.0 (0 → 1280 nits)
#   sdr.jxr       : 8-bit RGB
param([string]$Out = (Join-Path $PSScriptRoot "..\samples\images\generated"))
Add-Type -AssemblyName PresentationCore, WindowsBase
New-Item -ItemType Directory -Force $Out | Out-Null

function Save-Jxr($bmp, $path) {
  $enc = New-Object System.Windows.Media.Imaging.WmpBitmapEncoder
  $enc.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($bmp))
  $fs = [IO.File]::Create($path)
  $enc.Save($fs)
  $fs.Close()
  Write-Output ("wrote {0} {1}" -f $path, (Get-Item $path).Length)
}

$w = 512; $h = 256
$buf = New-Object float[] ($w * $h * 4)
for ($y = 0; $y -lt $h; $y++) {
  for ($x = 0; $x -lt $w; $x++) {
    $v = [float]($x / ($w - 1) * 16.0)
    $i = ($y * $w + $x) * 4
    $band = [math]::Floor($y * 4 / $h)
    $buf[$i] = if ($band -eq 0 -or $band -eq 3) { $v } else { 0 }
    $buf[$i + 1] = if ($band -eq 1 -or $band -eq 3) { $v } else { 0 }
    $buf[$i + 2] = if ($band -eq 2 -or $band -eq 3) { $v } else { 0 }
    $buf[$i + 3] = 1.0
  }
}
$hdr = [System.Windows.Media.Imaging.BitmapSource]::Create($w, $h, 96, 96, [System.Windows.Media.PixelFormats]::Rgba128Float, $null, $buf, $w * 16)
Save-Jxr $hdr (Join-Path $Out "hdr-scrgb.jxr")

$b8 = New-Object byte[] ($w * $h * 3)
for ($i = 0; $i -lt $w * $h; $i++) { $x = $i % $w; $v = [int][math]::Floor($x / 2); $b8[$i * 3] = [byte]$v; $b8[$i * 3 + 1] = 128; $b8[$i * 3 + 2] = [byte](255 - $v) }
$sdr = [System.Windows.Media.Imaging.BitmapSource]::Create($w, $h, 96, 96, [System.Windows.Media.PixelFormats]::Rgb24, $null, $b8, $w * 3)
Save-Jxr $sdr (Join-Path $Out "sdr.jxr")
