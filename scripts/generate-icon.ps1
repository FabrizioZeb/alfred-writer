# Generates assets/icon.ico: the AW badge (green fill, slate ring, white "AW" monogram)
# as a proper multi-resolution Windows icon. This is the single source of truth for the
# badge design — build.rs embeds it into the compiled .exe's resource icon, and
# src/theme.rs::badge_rgba() reads the same file back at runtime (via the `ico` crate)
# for the tray icon and in-app window icons, so every consumer shows the identical image
# instead of three hand-maintained approximations of it. Re-run this after any palette or
# badge-design change and keep the two in sync by eye.
#
# Usage: pwsh -File scripts/generate-icon.ps1
#
# Frame encoding: 256px uses PNG (required — the classic ICO directory entry can't even
# express a raw DIB that large), but 16/32/48/64px use raw 32bpp BGRA DIB data instead of
# PNG. That's not optional polish: Windows' own resource compiler (rc.exe) — which
# build.rs invokes via `winres` — rejects PNG-compressed frames below 256px with
# "RC2176: old DIB ... pass it through SDKPAINT". Modern shell icon rendering (Explorer,
# taskbar) accepts either encoding; rc.exe is the one picky consumer here.

Add-Type -AssemblyName System.Drawing

# Green, not magenta — magenta is used elsewhere (Save button, headings) but the badge
# itself uses the palette's sage-derived green so the icon reads as its own accent.
$badgeFill = [System.Drawing.Color]::FromArgb(255, 0x4B, 0x6B, 0x3A)
$slate = [System.Drawing.Color]::FromArgb(255, 0x47, 0x58, 0x5C)

function New-BadgeBitmap {
    param([int]$Size)

    $bmp = New-Object System.Drawing.Bitmap $Size, $Size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $ringWidth = [Math]::Max(1.5, $Size * 0.09)
    $outerMargin = $Size * 0.03
    $outerRect = New-Object System.Drawing.RectangleF $outerMargin, $outerMargin, ($Size - 2 * $outerMargin), ($Size - 2 * $outerMargin)
    $innerRect = New-Object System.Drawing.RectangleF ($outerMargin + $ringWidth), ($outerMargin + $ringWidth), ($Size - 2 * ($outerMargin + $ringWidth)), ($Size - 2 * ($outerMargin + $ringWidth))

    $g.FillEllipse((New-Object System.Drawing.SolidBrush $slate), $outerRect)
    $g.FillEllipse((New-Object System.Drawing.SolidBrush $badgeFill), $innerRect)

    # "AW" at every size, including the 16px tray-icon size — GDI+'s text rasterizer
    # (unlike egui's bundled font, which is missing plain glyphs we needed elsewhere)
    # handles small bold text cleanly, so there's no reason to drop it at small sizes.
    $fontSize = [float]($Size * 0.4)
    $font = New-Object System.Drawing.Font("Segoe UI", $fontSize, [System.Drawing.FontStyle]::Bold)
    $format = New-Object System.Drawing.StringFormat
    $format.Alignment = [System.Drawing.StringAlignment]::Center
    $format.LineAlignment = [System.Drawing.StringAlignment]::Center
    $center = New-Object System.Drawing.PointF ($Size / 2), ($Size / 2 + $Size * 0.01)
    $g.DrawString("AW", $font, [System.Drawing.Brushes]::White, $center, $format)

    $g.Dispose()
    return $bmp
}

function ConvertTo-PngBytes {
    param([System.Drawing.Bitmap]$Bitmap)
    $ms = New-Object System.IO.MemoryStream
    $Bitmap.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    return $ms.ToArray()
}

function ConvertTo-DibBytes {
    param([System.Drawing.Bitmap]$Bitmap, [int]$Size)

    $rect = New-Object System.Drawing.Rectangle 0, 0, $Size, $Size
    $locked = $Bitmap.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $locked.Stride
    $raw = New-Object byte[] ($stride * $Size)
    [System.Runtime.InteropServices.Marshal]::Copy($locked.Scan0, $raw, 0, $raw.Length)
    $Bitmap.UnlockBits($locked)

    # GDI+ gives rows top-down in BGRA byte order; DIB rows must be bottom-up.
    $flipped = New-Object byte[] $raw.Length
    for ($row = 0; $row -lt $Size; $row++) {
        [Array]::Copy($raw, $row * $stride, $flipped, ($Size - 1 - $row) * $stride, $stride)
    }

    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    $bw.Write([UInt32]40)               # biSize (BITMAPINFOHEADER)
    $bw.Write([Int32]$Size)             # biWidth
    $bw.Write([Int32]($Size * 2))       # biHeight: XOR image + AND mask, per ICO convention
    $bw.Write([UInt16]1)                # biPlanes
    $bw.Write([UInt16]32)               # biBitCount
    $bw.Write([UInt32]0)                # biCompression: BI_RGB
    $bw.Write([UInt32]($stride * $Size))# biSizeImage
    $bw.Write([Int32]0)                 # biXPelsPerMeter
    $bw.Write([Int32]0)                 # biYPelsPerMeter
    $bw.Write([UInt32]0)                # biClrUsed
    $bw.Write([UInt32]0)                # biClrImportant
    $bw.Write($flipped)                 # XOR (color + alpha) data

    # 1bpp AND mask, rows padded to 4 bytes; all-zero since the alpha channel above
    # already carries real transparency and nothing here needs to be forced opaque/masked.
    $maskRowBytes = [Math]::Ceiling($Size / 32.0) * 4
    $bw.Write((New-Object byte[] ($maskRowBytes * $Size)))
    $bw.Flush()
    return $ms.ToArray()
}

$dibSizes = @(16, 32, 48, 64)
$pngSize = 256
$allSizes = $dibSizes + $pngSize

$frames = [System.Collections.Generic.List[byte[]]]::new()
foreach ($size in $dibSizes) {
    $bmp = New-BadgeBitmap -Size $size
    $frames.Add((ConvertTo-DibBytes -Bitmap $bmp -Size $size))
    $bmp.Dispose()
}
$bmp256 = New-BadgeBitmap -Size $pngSize
$frames.Add((ConvertTo-PngBytes -Bitmap $bmp256))
$bmp256.Dispose()

$headerSize = 6
$dirEntrySize = 16
$offset = $headerSize + ($dirEntrySize * $allSizes.Count)

$out = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $out

$writer.Write([UInt16]0)   # reserved
$writer.Write([UInt16]1)   # type: icon
$writer.Write([UInt16]$allSizes.Count)

for ($i = 0; $i -lt $allSizes.Count; $i++) {
    $size = $allSizes[$i]
    $frameBytes = $frames[$i]
    $wByte = if ($size -ge 256) { 0 } else { $size }
    $hByte = if ($size -ge 256) { 0 } else { $size }
    $writer.Write([byte]$wByte)
    $writer.Write([byte]$hByte)
    $writer.Write([byte]0)    # color palette: none
    $writer.Write([byte]0)    # reserved
    $writer.Write([UInt16]1)  # color planes
    $writer.Write([UInt16]32) # bits per pixel
    $writer.Write([UInt32]$frameBytes.Length)
    $writer.Write([UInt32]$offset)
    $offset += $frameBytes.Length
}

foreach ($frameBytes in $frames) {
    $writer.Write($frameBytes)
}

$writer.Flush()
$repoRoot = Split-Path -Parent $PSScriptRoot
$outPath = Join-Path $repoRoot "assets\icon.ico"
[System.IO.File]::WriteAllBytes($outPath, $out.ToArray())
Write-Output "Wrote $outPath ($($allSizes.Count) sizes: $($allSizes -join ', '); DIB for <256, PNG for 256)"
