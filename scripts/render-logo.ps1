# Render the logo's SVG masters (assets/logo/*.svg) into the files the app and the
# installer use, all committed so neither a build nor CI needs an SVG renderer:
#
#   assets/logo/open-task.ico            the app icon: every size Windows asks for,
#                                        each from the drawing made for it
#   assets/logo/open-task-512.png        for places that want a raster (README, web)
#   assets/logo/installer/wizard-N.png   the installer wizard's corner image, one per
#                                        size Inno Setup uses at 100% to 250% scaling
#
# Needs resvg (`cargo install resvg --locked`); found via $env:RESVG, PATH, or
# ~/.cargo/bin. Output is deterministic: running it again changes nothing unless an
# SVG changed.
#
# Usage: pwsh -File scripts/render-logo.ps1
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$logo = Join-Path $root "assets\logo"
$work = Join-Path $root "target\logo-render"
New-Item -ItemType Directory -Force $work, (Join-Path $logo "installer") | Out-Null

$resvg = @(
    $env:RESVG,
    (Get-Command resvg -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source),
    (Join-Path $HOME ".cargo\bin\resvg.exe")
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $resvg) { throw "resvg not found; install it with: cargo install resvg --locked" }

# Which drawing each size comes from. The big one blurs below 48 px, so the small
# sizes have their own (see the comments in each SVG).
function Source([int]$size) {
    if ($size -le 16) { return "open-task-16.svg" }
    if ($size -le 24) { return "open-task-24.svg" }
    if ($size -le 40) { return "open-task-32.svg" }
    return "open-task.svg"
}

function Render([int]$size, [string]$out) {
    $svg = Join-Path $logo (Source $size)
    & $resvg -w $size -h $size $svg $out
    if ($LASTEXITCODE -ne 0) { throw "resvg failed on $svg at $size px" }
}

# The sizes Windows' icon guidance lists: 16 to 40 for small icons at 100% to 250%
# scaling, 32 to 96 for large ones, 256 for Explorer's big views.
$iconSizes = 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256
$images = foreach ($s in $iconSizes) {
    $png = Join-Path $work "icon-$s.png"
    Render $s $png
    [pscustomobject]@{ Size = $s; Bytes = [IO.File]::ReadAllBytes($png) }
}

# An .ico: ICONDIR, one ICONDIRENTRY per image, then the images. Every image is a
# PNG, which Windows reads at any size since Vista. Width and height 0 mean 256.
$ico = Join-Path $logo "open-task.ico"
$stream = [IO.File]::Create($ico)
$w = New-Object IO.BinaryWriter $stream
try {
    $w.Write([uint16]0)
    $w.Write([uint16]1)
    $w.Write([uint16]$images.Count)
    $offset = 6 + 16 * $images.Count
    foreach ($img in $images) {
        $dim = if ($img.Size -ge 256) { 0 } else { $img.Size }
        $w.Write([byte]$dim)
        $w.Write([byte]$dim)
        $w.Write([byte]0)       # colors in the palette: none
        $w.Write([byte]0)       # reserved
        $w.Write([uint16]1)     # planes
        $w.Write([uint16]32)    # bits per pixel
        $w.Write([uint32]$img.Bytes.Length)
        $w.Write([uint32]$offset)
        $offset += $img.Bytes.Length
    }
    foreach ($img in $images) { $w.Write($img.Bytes) }
} finally {
    $w.Dispose()
}
Write-Host "wrote $ico ($($images.Count) sizes, $((Get-Item $ico).Length) bytes)"

Render 512 (Join-Path $logo "open-task-512.png")
Write-Host "wrote open-task-512.png"

# Inno Setup's image area for WizardSmallImageFile at 100%, 125%, ... 250% scaling
# (from its documentation); Setup picks the file that fits best.
foreach ($s in 58, 77, 97, 116, 124, 143, 159) {
    Render $s (Join-Path $logo "installer\wizard-$s.png")
}
Write-Host "wrote the installer's wizard images"
