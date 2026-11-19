<#
.SYNOPSIS
  Generates the NM Mail icons (envelope + lightning bolt, flat Fluent style) into assets/icon.

.DESCRIPTION
  Every ICO frame is rendered from vector geometry at its native size (no downscaling blur).
  Outputs:
    assets/icon/nmmail.ico          app icon, 16-256 px
    assets/icon/nmmail.png          256 px PNG
    assets/icon/nmmail-unread.ico   tray: same icon with a red dot badge
    assets/icon/nmmail-offline.ico  tray: grayscale
  Frames < 64 px are stored as 32-bit DIBs, larger ones as PNG (standard Vista+ layout).

  Usage (Windows PowerShell 5.1 or PowerShell 7 on Windows):
    powershell -ExecutionPolicy Bypass -File scripts/generate-icons.ps1
#>
param(
    [string]$OutDir = (Join-Path $PSScriptRoot '..\assets\icon')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$sizes = @(16, 20, 24, 32, 40, 48, 64, 96, 128, 256)

function New-RoundedRect([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-Color([string]$hex, [int]$alpha = 255) {
    $c = [System.Drawing.ColorTranslator]::FromHtml($hex)
    return [System.Drawing.Color]::FromArgb($alpha, $c.R, $c.G, $c.B)
}

function New-Bolt([single]$s) {
    # Lightning bolt in the bottom-right corner, expressed in a 256 design grid.
    $bx = 146 * $s; $by = 112 * $s; $bw = 104 * $s; $bh = 140 * $s
    $pts = @(
        @(0.62, 0.00), @(0.08, 0.58), @(0.43, 0.58), @(0.30, 1.00),
        @(0.92, 0.38), @(0.56, 0.38), @(0.74, 0.00)
    )
    $arr = New-Object 'System.Drawing.PointF[]' $pts.Count
    for ($i = 0; $i -lt $pts.Count; $i++) {
        $arr[$i] = New-Object System.Drawing.PointF ([single]($bx + $pts[$i][0] * $bw)), ([single]($by + $pts[$i][1] * $bh))
    }
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddPolygon($arr)
    return $p
}

function New-IconBitmap([int]$size, [string]$variant) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = [single]($size / 256.0)
    $small = $size -le 24

    # Envelope body
    $ex = 10 * $s; $ey = 38 * $s; $ew = 222 * $s; $eh = 164 * $s
    $radius = [single]([math]::Max(1.5, 26 * $s))
    $body = New-RoundedRect $ex $ey $ew $eh $radius
    $rect = New-Object System.Drawing.RectangleF $ex, $ey, $ew, $eh
    $bodyBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, (New-Color '#2B9AF3'), (New-Color '#0F6CBD'), ([single]90)
    $g.FillPath($bodyBrush, $body)

    # Flap (lighter V), clipped to the body
    $g.SetClip($body)
    $flap = New-Object System.Drawing.Drawing2D.GraphicsPath
    $fpts = New-Object 'System.Drawing.PointF[]' 3
    $fpts[0] = New-Object System.Drawing.PointF $ex, $ey
    $fpts[1] = New-Object System.Drawing.PointF ([single]($ex + $ew / 2)), ([single]($ey + $eh * 0.60))
    $fpts[2] = New-Object System.Drawing.PointF ([single]($ex + $ew)), $ey
    $flap.AddPolygon($fpts)
    $flapBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, (New-Color '#9FD4FB'), (New-Color '#4BA9F1'), ([single]90)
    $g.FillPath($flapBrush, $flap)
    # Crease line under the flap
    $creaseW = [single]([math]::Max(1.0, 7 * $s))
    $crease = New-Object System.Drawing.Pen (New-Color '#FFFFFF' 235), $creaseW
    $crease.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $crease.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $crease.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $g.DrawLines($crease, $fpts)
    $g.ResetClip()

    # Lightning bolt with a transparent knockout around it
    $bolt = New-Bolt $s
    $knock = New-Object System.Drawing.Pen ([System.Drawing.Color]::Transparent), ([single]([math]::Max(2.0, 22 * $s)))
    $knock.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $g.DrawPath($knock, $bolt)
    $g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::Transparent)), $bolt)
    $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
    $boltRect = $bolt.GetBounds()
    $boltBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $boltRect, (New-Color '#FFD43B'), (New-Color '#FF9F0A'), ([single]90)
    $g.FillPath($boltBrush, $bolt)
    if (-not $small) {
        $edge = New-Object System.Drawing.Pen (New-Color '#E07B00' 160), ([single]([math]::Max(1.0, 3 * $s)))
        $edge.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
        $g.DrawPath($edge, $bolt)
    }

    if ($variant -eq 'unread') {
        $cx = 214 * $s; $cy = 46 * $s; $r = 40 * $s
        $ring = 12 * $s
        $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
        $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::Transparent)), [single]($cx - $r - $ring), [single]($cy - $r - $ring), [single](2 * ($r + $ring)), [single](2 * ($r + $ring)))
        $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
        $g.FillEllipse((New-Object System.Drawing.SolidBrush (New-Color '#E3242B')), [single]($cx - $r), [single]($cy - $r), [single](2 * $r), [single](2 * $r))
    }
    $g.Dispose()

    if ($variant -eq 'offline') {
        for ($y = 0; $y -lt $size; $y++) {
            for ($x = 0; $x -lt $size; $x++) {
                $c = $bmp.GetPixel($x, $y)
                if ($c.A -eq 0) { continue }
                $l = [int]([math]::Round(0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B))
                # Lift the gray a bit so the icon still reads on dark taskbars.
                $l = [math]::Min(255, [int]($l * 0.85 + 40))
                $bmp.SetPixel($x, $y, [System.Drawing.Color]::FromArgb($c.A, $l, $l, $l))
            }
        }
    }
    return $bmp
}

function Get-PngBytes($bmp) {
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    return , $ms.ToArray()
}

function Get-DibBytes($bmp) {
    $w = $bmp.Width; $h = $bmp.Height
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    # BITMAPINFOHEADER (height doubled: XOR + AND masks)
    $bw.Write([int]40); $bw.Write([int]$w); $bw.Write([int]($h * 2)); $bw.Write([int16]1); $bw.Write([int16]32)
    $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0)
    for ($y = $h - 1; $y -ge 0; $y--) {
        for ($x = 0; $x -lt $w; $x++) {
            $c = $bmp.GetPixel($x, $y)
            $bw.Write([byte]$c.B); $bw.Write([byte]$c.G); $bw.Write([byte]$c.R); $bw.Write([byte]$c.A)
        }
    }
    $stride = [int]([math]::Floor(($w + 31) / 32) * 4)
    for ($y = $h - 1; $y -ge 0; $y--) {
        $row = New-Object byte[] $stride
        for ($x = 0; $x -lt $w; $x++) {
            if ($bmp.GetPixel($x, $y).A -eq 0) { $row[[math]::Floor($x / 8)] = $row[[math]::Floor($x / 8)] -bor (0x80 -shr ($x % 8)) }
        }
        $bw.Write($row)
    }
    $bw.Flush()
    return , $ms.ToArray()
}

function Write-Ico([string]$path, [string]$variant) {
    $frames = @()
    foreach ($size in $sizes) {
        $bmp = New-IconBitmap $size $variant
        if ($size -ge 64) { $bytes = Get-PngBytes $bmp } else { $bytes = Get-DibBytes $bmp }
        $frames += , @($size, $bytes)
        $bmp.Dispose()
    }
    $fs = [System.IO.File]::Create($path)
    $bw = New-Object System.IO.BinaryWriter $fs
    $bw.Write([int16]0); $bw.Write([int16]1); $bw.Write([int16]$frames.Count)
    $offset = 6 + 16 * $frames.Count
    foreach ($f in $frames) {
        $sz = $f[0]; $data = $f[1]
        $dim = if ($sz -ge 256) { 0 } else { $sz }
        $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
        $bw.Write([int16]1); $bw.Write([int16]32); $bw.Write([int]$data.Length); $bw.Write([int]$offset)
        $offset += $data.Length
    }
    foreach ($f in $frames) { $bw.Write([byte[]]$f[1]) }
    $bw.Flush(); $fs.Close()
    Write-Host "wrote $path"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
Write-Ico (Join-Path $OutDir 'nmmail.ico') 'normal'
Write-Ico (Join-Path $OutDir 'nmmail-unread.ico') 'unread'
Write-Ico (Join-Path $OutDir 'nmmail-offline.ico') 'offline'
$png = New-IconBitmap 256 'normal'
$png.Save((Join-Path $OutDir 'nmmail.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$png.Dispose()
Write-Host "wrote $(Join-Path $OutDir 'nmmail.png')"
