param()
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$extension = Join-Path $root "extension\icons"
$client = Join-Path $root "crates\app-gpui\assets"
New-Item -ItemType Directory -Force -Path $extension, $client | Out-Null

Add-Type -AssemblyName System.Drawing

function Pt([double]$x, [double]$y) {
    return [System.Drawing.PointF]::new(
        [single](($x - 45.0) * 1.193),
        [single](($y - 45.0) * 1.193)
    )
}

function New-MarkBitmap([int]$size) {
    $bitmap = [System.Drawing.Bitmap]::new(
        $size,
        $size,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.Clear([System.Drawing.Color]::Transparent)
        $scale = $size / 512.0
        $graphics.ScaleTransform($scale, $scale)

        $bg = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $radius = 112.0
        $diameter = $radius * 2.0
        $bg.AddArc(0, 0, $diameter, $diameter, 180, 90)
        $bg.AddArc(512 - $diameter, 0, $diameter, $diameter, 270, 90)
        $bg.AddArc(512 - $diameter, 512 - $diameter, $diameter, $diameter, 0, 90)
        $bg.AddArc(0, 512 - $diameter, $diameter, $diameter, 90, 90)
        $bg.CloseFigure()
        $brush = [System.Drawing.SolidBrush]::new(
            [System.Drawing.ColorTranslator]::FromHtml("#0B0C0E")
        )
        $graphics.FillPath($brush, $bg)
        $brush.Dispose()
        $bg.Dispose()

        $mark = [System.Drawing.Drawing2D.GraphicsPath]::new()
        $mark.StartFigure()
        $mark.AddBezier((Pt 352 104), (Pt 227 172), (Pt 141 268), (Pt 110 406))
        $mark.AddBezier((Pt 110 406), (Pt 207 359), (Pt 289 297), (Pt 351 207))
        $mark.AddBezier((Pt 351 207), (Pt 374 172), (Pt 393 136), (Pt 407 98))
        $mark.AddBezier((Pt 407 98), (Pt 369 138), (Pt 326 172), (Pt 278 198))
        $mark.AddBezier((Pt 278 198), (Pt 300 173), (Pt 327 138), (Pt 352 104))
        $mark.CloseFigure()
        $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
        $graphics.FillPath($white, $mark)
        $white.Dispose()
        $mark.Dispose()
    }
    finally {
        $graphics.Dispose()
    }
    return $bitmap
}

function Save-Png([int]$size, [string]$path) {
    $bitmap = New-MarkBitmap $size
    try {
        $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $bitmap.Dispose()
    }
}

$sizes = @(16, 32, 48, 64, 128, 256)
foreach ($size in $sizes) {
    Save-Png $size (Join-Path $extension "icon$size.png")
}

$icoFrames = @()
foreach ($size in $sizes) {
    $path = Join-Path $extension "icon$size.png"
    $icoFrames += [pscustomobject]@{
        Size = $size
        Bytes = [System.IO.File]::ReadAllBytes($path)
    }
}

$icoPath = Join-Path $client "volant-icon.ico"
$stream = [System.IO.File]::Create($icoPath)
$writer = [System.IO.BinaryWriter]::new($stream)
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$icoFrames.Count)
    $offset = 6 + (16 * $icoFrames.Count)
    foreach ($frame in $icoFrames) {
        $dimension = if ($frame.Size -ge 256) { 0 } else { $frame.Size }
        $writer.Write([byte]$dimension)
        $writer.Write([byte]$dimension)
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$frame.Bytes.Length)
        $writer.Write([uint32]$offset)
        $offset += $frame.Bytes.Length
    }
    foreach ($frame in $icoFrames) {
        $writer.Write($frame.Bytes)
    }
}
finally {
    $writer.Dispose()
    $stream.Dispose()
}

Copy-Item (Join-Path $root "volant-icon.svg") (Join-Path $extension "volant.svg") -Force
Copy-Item (Join-Path $root "volant-icon.svg") (Join-Path $client "volant-icon.svg") -Force
Write-Host "Brand assets written to $extension and $client"
