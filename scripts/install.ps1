# Install the latest kf-code release binary on Windows.
# Usage: irm https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.ps1 | iex
# Dependencies: PowerShell 5.1+ (ships with Windows). No external modules.
[CmdletBinding()]
param(
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$Repo = 'KirkForge/Kirkforge_Cli'
$InstallDir = Join-Path $env:USERPROFILE '.kf-code\bin'
$ConfigDir  = Join-Path $env:USERPROFILE '.kf-code\config'
$Target = 'x86_64-pc-windows-msvc'

# Fetch the latest release tag.
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
$tag = $release.tag_name
if (-not $tag) { throw "Failed to determine latest release tag." }

$archive = "kf-code-$Target.zip"
$downloadUrl = "https://github.com/$Repo/releases/download/$tag/$archive"
$sumsUrl     = "https://github.com/$Repo/releases/download/$tag/SHA256SUMS.txt"

Write-Host "Downloading kf-code $tag for $Target..."
if ($DryRun) {
    Write-Host "[DryRun] would download $downloadUrl -> verify -> install to $InstallDir"
    return
}

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "kf-code-install-$(Get-Random)") -Force
try {
    $archivePath = Join-Path $tmp $archive
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath

    # SHA256 verification. The release publishes SHA256SUMS.txt listing every
    # archive hash; refuse to install on a missing entry or mismatch.
    $sumsPath = Join-Path $tmp 'SHA256SUMS.txt'
    Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLower()
    $expected = $null
    foreach ($line in Get-Content $sumsPath) {
        $parts = $line -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $archive) {
            $expected = $parts[0].ToLower()
            break
        }
    }
    if (-not $expected) { throw "No checksum entry for $archive in SHA256SUMS.txt — refusing to install." }
    if ($actual -ne $expected) {
        throw "Checksum mismatch for $archive.`n  expected: $expected`n  actual:   $actual"
    }
    Write-Host "Verified checksum for $archive."

    Expand-Archive -Path $archivePath -DestinationPath $tmp -Force

    if (-not (Test-Path $InstallDir)) { New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null }
    $binary = Get-ChildItem -Path $tmp -Recurse -Filter 'kf-code.exe' | Select-Object -First 1
    if (-not $binary) { throw "kf-code.exe not found in archive." }
    Copy-Item -Path $binary.FullName -Destination (Join-Path $InstallDir 'kf-code.exe') -Force

    if (-not (Test-Path $ConfigDir)) { New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null }

    # Add to user PATH if not already present.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath -notlike "*$InstallDir*") {
        $newPath = if ($userPath) { "$InstallDir;$userPath" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added $InstallDir to user PATH (restart your shell for it to take effect)."
    }
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "kf-code installed! Run: kf-code"