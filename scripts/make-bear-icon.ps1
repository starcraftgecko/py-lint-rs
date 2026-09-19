Add-Type -AssemblyName System.Drawing

$size = 256
$bmp = New-Object System.Drawing.Bitmap $size, $size
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

$brown = [System.Drawing.Color]::FromArgb(255, 121, 85, 61)
$darkBrown = [System.Drawing.Color]::FromArgb(255, 92, 64, 46)
$tan = [System.Drawing.Color]::FromArgb(255, 210, 170, 130)
$black = [System.Drawing.Color]::FromArgb(255, 30, 24, 20)

$brownBrush = New-Object System.Drawing.SolidBrush $brown
$darkBrownBrush = New-Object System.Drawing.SolidBrush $darkBrown
$tanBrush = New-Object System.Drawing.SolidBrush $tan
$blackBrush = New-Object System.Drawing.SolidBrush $black

# Ears
$g.FillEllipse($brownBrush, 30, 20, 70, 70)
$g.FillEllipse($brownBrush, 156, 20, 70, 70)
$g.FillEllipse($darkBrownBrush, 50, 40, 30, 30)
$g.FillEllipse($darkBrownBrush, 176, 40, 30, 30)

# Head
$g.FillEllipse($brownBrush, 28, 60, 200, 180)

# Snout
$g.FillEllipse($tanBrush, 78, 150, 100, 80)

# Nose
$g.FillEllipse($blackBrush, 108, 175, 40, 30)

# Eyes
$g.FillEllipse($blackBrush, 78, 120, 24, 24)
$g.FillEllipse($blackBrush, 154, 120, 24, 24)

$g.Dispose()

$iconPath = "C:\Users\joshu\code\py-lint-rs\scripts\bear.ico"
$hIcon = $bmp.GetHicon()
$icon = [System.Drawing.Icon]::FromHandle($hIcon)
$fs = New-Object System.IO.FileStream $iconPath, ([System.IO.FileMode]::Create)
$icon.Save($fs)
$fs.Close()
$icon.Dispose()
$bmp.Dispose()

Write-Output "Icon saved to $iconPath"
