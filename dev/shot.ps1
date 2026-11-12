param([string]$out, [int]$w = 900, [int]$h = 320)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System.Runtime.InteropServices;
public class Dpi { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
"@
[Dpi]::SetProcessDPIAware() | Out-Null
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$x = $b.X + [int](($b.Width - $w) / 2)
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $b.Y, 0, 0, (New-Object System.Drawing.Size $w, $h))
$bmp.Save($out); "screen $($b.Width)x$($b.Height)"
