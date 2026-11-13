param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$WaitMs = 2500,
    # Extra command line passed to the exe as one string.
    [string]$ExeArgs = ""
)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W {
    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr h);
}
"@

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path $Exe).Path
$psi.Arguments = $ExeArgs
$psi.UseShellExecute = $false
$p = [System.Diagnostics.Process]::Start($psi)
try {
    Start-Sleep -Milliseconds $WaitMs
    $p.Refresh()
    $h = $p.MainWindowHandle
    if ($h -ne [IntPtr]::Zero) {
        # HWND_TOPMOST(-1), SWP_NOSIZE(0x1) | SWP_SHOWWINDOW(0x40), move to 0,0
        [void][W]::SetWindowPos($h, [IntPtr](-1), 0, 0, 0, 0, 0x41)
        [void][W]::SetForegroundWindow($h)
        Start-Sleep -Milliseconds 600
    } else {
        Write-Host "main window not found"
    }

    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.X, $b.Y, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Write-Host "saved $Out"
} finally {
    if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit(3000) | Out-Null }
}
