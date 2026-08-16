# Uninstall kf-code on Windows. Removes the install dir and the PATH entry.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\uninstall.ps1
[CmdletBinding()]
param(
    [switch]$RemoveConfig
)

$ErrorActionPreference = 'Stop'
$InstallRoot = Join-Path $env:USERPROFILE '.kf-code'
$InstallDir  = Join-Path $InstallRoot 'bin'

if (Test-Path $InstallRoot) {
    Remove-Item -Path $InstallRoot -Recurse -Force
    Write-Host "Removed $InstallRoot"
} else {
    Write-Host "No install directory at $InstallRoot (already removed?)."
}

# Remove from user PATH if present.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -and $userPath -like "*$InstallDir*") {
    $entries = $userPath -split ';' | Where-Object { $_ -and $_ -ne $InstallDir }
    [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
    Write-Host "Removed $InstallDir from user PATH (restart your shell to see the change)."
}

if ($RemoveConfig) {
    $ConfigDir = Join-Path $env:USERPROFILE '.config\kf-code'
    if (Test-Path $ConfigDir) {
        Remove-Item -Path $ConfigDir -Recurse -Force
        Write-Host "Removed $ConfigDir"
    }
}

Write-Host ""
Write-Host "kf-code uninstalled."