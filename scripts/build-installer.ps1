# Build the Windows installer from two already-built binaries.
#
# Usage:
#   pwsh -File scripts/build-installer.ps1 -Version 0.2.1 `
#       -X64Exe path\to\x64\open-task.exe -Arm64Exe path\to\arm64\open-task.exe `
#       [-OutDir target\installer]
#
# Finds ISCC.exe in, in order: $env:ISCC, $env:INNO_SETUP_DIR, the standard install
# locations of Inno Setup 6 and 7, and the repo's portable copies under
# target\tools\innosetup* (see plans/windows-installer.md for how those get there).
# Inno Setup 6.3 or newer is required; the script checks its own version.
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$X64Exe,
    [Parameter(Mandatory = $true)][string]$Arm64Exe,
    [string]$OutDir = "target\installer"
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot

# Keep both lists as arrays: a lone string plus an array would concatenate as text.
[string[]]$explicit = @(
    $env:ISCC,
    $(if ($env:INNO_SETUP_DIR) { Join-Path $env:INNO_SETUP_DIR "ISCC.exe" })
) | Where-Object { $_ -and (Test-Path $_) }
[string[]]$discovered = @(
    "${env:ProgramFiles(x86)}\Inno Setup *\ISCC.exe",
    "$env:ProgramFiles\Inno Setup *\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup *\ISCC.exe",
    (Join-Path $repo "target\tools\innosetup*\ISCC.exe")
) | ForEach-Object { Get-ChildItem -Path $_ -ErrorAction SilentlyContinue } | ForEach-Object { $_.FullName }
$iscc = @($explicit + $discovered) | Select-Object -First 1
if (-not $iscc) { throw "ISCC.exe not found; install Inno Setup 6.3+ or set INNO_SETUP_DIR" }
Write-Host "using $iscc"

foreach ($exe in @($X64Exe, $Arm64Exe)) {
    if (-not (Test-Path $exe)) { throw "binary not found: $exe" }
}
$X64Exe = (Resolve-Path $X64Exe).Path
$Arm64Exe = (Resolve-Path $Arm64Exe).Path
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

# The file version resource must be numeric; a prerelease suffix is display-only.
$numeric = ($Version -split "-")[0]

& $iscc `
    "/DAppVersion=$Version" `
    "/DVersionInfoVersion=$numeric" `
    "/DX64Exe=$X64Exe" `
    "/DArm64Exe=$Arm64Exe" `
    "/DOutputDir=$OutDir" `
    "/Qp" `
    (Join-Path $repo "installer\windows\open-task.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }

$setup = Join-Path $OutDir "open-task-v$Version-windows-setup.exe"
if (-not (Test-Path $setup)) { throw "expected output missing: $setup" }
Write-Host "built $setup ($((Get-Item $setup).Length) bytes)"
