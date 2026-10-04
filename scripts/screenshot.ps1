# Launch the app, wait for its window, screenshot it, and kill it.
# Dev-time visual smoke test. Usage: pwsh -File scripts/screenshot.ps1 [-Exe path] [-Out path]
#   [-Click "x,y;x,y"] [-Keys "End;Up"] [-Wheel -20] [-AfterClickMs 3000]
param(
    [string]$Exe = "target\debug\open-task.exe",
    [string]$Out = "target\screenshot.png",
    [int]$WaitMs = 5000,
    [int]$SettleMs = 2500,
    # Extra command-line arguments for the app, as one string, e.g. "--theme light".
    [string]$AppArgs = "",
    # Left clicks before the shot, "x,y" separated by ";", in the coordinates of the
    # screenshot itself (so a point can be read off an earlier shot).
    [string]$Click = "",
    # Keys to press after the clicks, by name, separated by ";": End, Home, Up,
    # Down, Left, Right, Tab, Enter, Escape, Space, or a single character.
    [string]$Keys = "",
    # Wheel notches over the middle of the window after the keys: negative scrolls
    # down (toward the user), as a real wheel does.
    [int]$Wheel = 0,
    # How long to wait after the last click or key before the shot.
    [int]$AfterClickMs = 1000
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class Native {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);

    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder s, int n);

    // The top-level window of class `cls` owned by process `pid`, or zero. Looking up
    // by class and title alone would find an already-running instance (an installed
    // copy, say) instead of the one this script just launched.
    public static IntPtr FindMainWindow(uint pid, string cls) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint owner;
            GetWindowThreadProcessId(h, out owner);
            if (owner != pid) return true;
            var name = new System.Text.StringBuilder(256);
            GetClassNameW(h, name, name.Capacity);
            if (name.ToString() != cls) return true;
            found = h;
            return false;
        }, IntPtr.Zero);
        return found;
    }
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
        $h = [Native]::FindMainWindow([uint32]$p.Id, "OpenTaskMainWindow")
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

    if ($Click -ne "") {
        foreach ($xy in $Click -split ";") {
            $x, $y = $xy -split "," | ForEach-Object { [int]$_ }
            # Screenshot coordinates start at the visible frame's corner.
            $pt = [Native+POINT]::new()
            $pt.X = $rd.L + $x
            $pt.Y = $rd.T + $y
            [Native]::ScreenToClient($h, [ref]$pt) | Out-Null
            $l = [IntPtr](($pt.Y -shl 16) -bor ($pt.X -band 0xFFFF))
            # WM_MOUSEMOVE, WM_LBUTTONDOWN (MK_LBUTTON), WM_LBUTTONUP.
            [Native]::PostMessageW($h, 0x0200, [IntPtr]::Zero, $l) | Out-Null
            [Native]::PostMessageW($h, 0x0201, [IntPtr]1, $l) | Out-Null
            [Native]::PostMessageW($h, 0x0202, [IntPtr]::Zero, $l) | Out-Null
            Start-Sleep -Milliseconds 200
        }
        Start-Sleep -Milliseconds $AfterClickMs
    }

    if ($Keys -ne "") {
        $vk = @{ End = 0x23; Home = 0x24; Up = 0x26; Down = 0x28; Left = 0x25; Right = 0x27;
                 Tab = 0x09; Enter = 0x0D; Escape = 0x1B; Space = 0x20 }
        foreach ($name in $Keys -split ";") {
            $code = if ($vk.ContainsKey($name)) { $vk[$name] } else { [int][char]$name.ToUpperInvariant() }
            # WM_KEYDOWN, WM_KEYUP; the scan code and repeat count are not needed.
            [Native]::PostMessageW($h, 0x0100, [IntPtr]$code, [IntPtr]0) | Out-Null
            [Native]::PostMessageW($h, 0x0101, [IntPtr]$code, [IntPtr]0xC0000000) | Out-Null
            Start-Sleep -Milliseconds 200
        }
        Start-Sleep -Milliseconds $AfterClickMs
    }

    if ($Wheel -ne 0) {
        # WM_MOUSEWHEEL takes screen coordinates; one notch is 120.
        $cx = [int](($rd.L + $rd.R) / 2); $cy = [int](($rd.T + $rd.B) / 2)
        $lw = [IntPtr](($cy -shl 16) -bor ($cx -band 0xFFFF))
        $step = if ($Wheel -lt 0) { -120 } else { 120 }
        for ($i = 0; $i -lt [Math]::Abs($Wheel); $i++) {
            $ww = [IntPtr](($step -band 0xFFFF) -shl 16)
            [Native]::PostMessageW($h, 0x020A, $ww, $lw) | Out-Null
            Start-Sleep -Milliseconds 30
        }
        Start-Sleep -Milliseconds $AfterClickMs
    }

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
