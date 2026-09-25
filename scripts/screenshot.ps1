# Launch the app, wait for its window, screenshot it, and kill it.
# Dev-time visual smoke test. Usage: pwsh -File scripts/screenshot.ps1 [-Exe path] [-Out path]
param(
    [string]$Exe = "target\debug\open-task.exe",
    [string]$Out = "target\screenshot.png",
    [int]$WaitMs = 5000,
    [int]$SettleMs = 2500,
    # Extra command-line arguments for the app, as one string, e.g. "--theme light".
    [string]$AppArgs = ""
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class Native {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
}
"@
[Native]::SetProcessDPIAware() | Out-Null

$err = [System.IO.Path]::ChangeExtension($Out, ".stderr.txt")
$startArgs = @{ FilePath = $Exe; PassThru = $true; NoNewWindow = $true; RedirectStandardError = $err }
if ($AppArgs -ne "") { $startArgs.ArgumentList = $AppArgs }
$p = Start-Process @startArgs
try {
    $h = [IntPtr]::Zero
    $deadline = (Get-Date).AddMilliseconds($WaitMs)
    while ((Get-Date) -lt $deadline) {
        $h = [Native]::FindWindowW("OpenTaskMainWindow", "open-task")
        if ($h -ne [IntPtr]::Zero) { break }
        if ($p.HasExited) { throw "process exited early with code $($p.ExitCode)" }
        Start-Sleep -Milliseconds 100
    }
    if ($h -eq [IntPtr]::Zero) { throw "window did not appear within $WaitMs ms" }
    Start-Sleep -Milliseconds $SettleMs
    $rw = [Native+RECT]::new()
    [Native]::GetWindowRect($h, [ref]$rw) | Out-Null
    $rd = [Native+RECT]::new()
    # DWMWA_EXTENDED_FRAME_BOUNDS (9): the visible frame, without invisible resize borders.
    [Native]::DwmGetWindowAttribute($h, 9, [ref]$rd, 16) | Out-Null

    # PrintWindow with PW_RENDERFULLCONTENT (2) renders DirectComposition content
    # straight from the window, so it works whatever is on top of it.
    $full = New-Object System.Drawing.Bitmap ($rw.R - $rw.L), ($rw.B - $rw.T)
    $g = [System.Drawing.Graphics]::FromImage($full)
    $hdc = $g.GetHdc()
    [Native]::PrintWindow($h, $hdc, 2) | Out-Null
    $g.ReleaseHdc($hdc)
    $crop = New-Object System.Drawing.Rectangle ($rd.L - $rw.L), ($rd.T - $rw.T), ($rd.R - $rd.L), ($rd.B - $rd.T)
    $bmp = $full.Clone($crop, $full.PixelFormat)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Host "saved $Out ($($bmp.Width)x$($bmp.Height))"
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Start-Sleep -Milliseconds 300
    if (Test-Path $err) { Write-Host "--- stderr (first 20 lines) ---"; Get-Content $err | Select-Object -First 20 }
}
