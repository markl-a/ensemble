#!/usr/bin/env pwsh
# Install ensemble.exe for the current Windows user.
#
# Default install location:
#   %LOCALAPPDATA%\ensemble\bin\ensemble.exe
#
# This script installs only the binary and PATH entry. Vendor MCP config is explicit:
#   ensemble mcp install --client codex|claude|opencode --repo . --team <name>

param(
    [string]$InstallDir = "",
    [string]$SourceExe = "",
    [string]$TargetDir = "",
    [switch]$SkipBuild,
    [switch]$NoPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

function Require-Tool([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "required tool not found on PATH: $Name"
    }
}

function Default-InstallDir() {
    $local = [Environment]::GetFolderPath("LocalApplicationData")
    if ([string]::IsNullOrWhiteSpace($local)) {
        $local = Join-Path $env:USERPROFILE "AppData\Local"
    }
    Join-Path $local "ensemble\bin"
}

function Normalize-PathForCompare([string]$Path) {
    [System.IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
}

function Split-UserPath([string]$PathValue) {
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return @()
    }
    @($PathValue -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Path-ContainsDir([string[]]$Parts, [string]$Dir) {
    $wanted = Normalize-PathForCompare $Dir
    foreach ($part in $Parts) {
        try {
            if ((Normalize-PathForCompare $part) -ieq $wanted) {
                return $true
            }
        } catch {
            # Ignore malformed legacy PATH entries.
        }
    }
    return $false
}

function Add-UserPathDir([string]$Dir) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @(Split-UserPath $userPath)
    if (-not (Path-ContainsDir $parts $Dir)) {
        $newPath = (@($parts) + $Dir) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Host "added User PATH entry: $Dir" -ForegroundColor Green
    } else {
        Write-Host "User PATH already contains: $Dir" -ForegroundColor DarkGray
    }

    $processParts = @(Split-UserPath $env:Path)
    if (-not (Path-ContainsDir $processParts $Dir)) {
        $env:Path = (@($Dir) + $processParts) -join ';'
    }
}

function Stop-EnsembleProcesses([string]$InstallDir) {
    $dir = Normalize-PathForCompare $InstallDir
    if (Test-Path -LiteralPath $dir -PathType Container) {
        $candidates = Get-CimInstance Win32_Process -Filter "Name = 'ensemble.exe'" -ErrorAction SilentlyContinue
        $pids = @()
        foreach ($candidate in $candidates) {
            $exe = $candidate.ExecutablePath
            if (-not $exe) { continue }
            try {
                if ((Normalize-PathForCompare $exe).StartsWith($dir, [System.StringComparison]::OrdinalIgnoreCase)) {
                    $pids += $candidate.ProcessId
                }
            } catch {
                continue
            }
        }
        if ($pids.Count -gt 0) {
            Write-Host "stopping running ensemble process(es) from ${InstallDir}: $($pids -join ', ')" -ForegroundColor Yellow
            foreach ($procId in $pids) {
                try {
                    Stop-Process -Id $procId -Force -ErrorAction Stop
                } catch {
                    Write-Host "could not stop pid $procId ($($_.Exception.Message))" -ForegroundColor DarkYellow
                }
            }
            Start-Sleep -Milliseconds 500
        }
    }
}

$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Default-InstallDir
}
$InstallDir = Normalize-PathForCompare $InstallDir

if ([string]::IsNullOrWhiteSpace($SourceExe)) {
    if ($SkipBuild) {
        $SourceExe = Join-Path $repo "target\release\ensemble.exe"
    } else {
        Require-Tool cargo
        if ([string]::IsNullOrWhiteSpace($TargetDir)) {
            if (Test-Path "D:\tmp") {
                $TargetDir = Join-Path "D:\tmp" "ensemble-install-target-$PID"
            } else {
                $TargetDir = Join-Path $env:TEMP "ensemble-install-target-$PID"
            }
        }
        Write-Host "== building release ensemble.exe ==" -ForegroundColor Cyan
        Push-Location $repo
        try {
            cargo build --release --bin ensemble --target-dir $TargetDir
            if ($LASTEXITCODE -ne 0) {
                exit $LASTEXITCODE
            }
        } finally {
            Pop-Location
        }
        $SourceExe = Join-Path $TargetDir "release\ensemble.exe"
    }
}

$SourceExe = Normalize-PathForCompare $SourceExe
if (-not (Test-Path -LiteralPath $SourceExe -PathType Leaf)) {
    Fail "source ensemble.exe not found: $SourceExe"
}

New-Item -ItemType Directory -Force $InstallDir | Out-Null
$targetExe = Join-Path $InstallDir "ensemble.exe"
$tmpExe = Join-Path $InstallDir ".ensemble.exe.installing"
Copy-Item -LiteralPath $SourceExe -Destination $tmpExe -Force
if (Test-Path -LiteralPath $targetExe -PathType Leaf) {
    Stop-EnsembleProcesses $InstallDir
    try {
        Remove-Item -LiteralPath $targetExe -Force
    } catch {
        Remove-Item -LiteralPath $tmpExe -Force -ErrorAction SilentlyContinue
        Fail "could not replace $targetExe. Close running ensemble/AI CLI sessions that were launched from the installed binary, then run install again. $($_.Exception.Message)"
    }
}
Move-Item -LiteralPath $tmpExe -Destination $targetExe -Force

Write-Host "installed: $targetExe" -ForegroundColor Green

if (-not $NoPath) {
    Add-UserPathDir $InstallDir
} else {
    Write-Host "skipped PATH update (-NoPath)" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "verification:" -ForegroundColor Cyan
& $targetExe doctor
$doctorExit = $LASTEXITCODE
if ($doctorExit -ne 0) {
    Write-Host "ensemble doctor exited $doctorExit; install still completed, but this machine may need CLI login/setup." -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Open a new terminal, then run:" -ForegroundColor Cyan
Write-Host "  ensemble doctor"
Write-Host "  ensemble --repo . --team phase1 --confirm-policy ask codex"
