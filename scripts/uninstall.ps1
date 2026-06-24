#!/usr/bin/env pwsh
# Uninstall ensemble for the current Windows user.
#
# Safe default: remove the installed binary directory and User PATH entry only.
# Repo-local `.ensemble/` state is never deleted by this script.

param(
    [string]$InstallDir = "",
    [switch]$NoPath,
    [switch]$RemoveMcpConfig,
    [string]$Repo = ".",
    [string[]]$Clients = @("codex", "claude", "opencode"),
    [string]$EnsembleExe = "",
    [switch]$ForceCustomInstallDir
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
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

function Split-PathList([string]$PathValue) {
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return @()
    }
    @($PathValue -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Remove-UserPathDir([string]$Dir) {
    $wanted = Normalize-PathForCompare $Dir
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @(Split-PathList $userPath)
    $kept = New-Object System.Collections.Generic.List[string]
    $removed = $false
    foreach ($part in $parts) {
        try {
            if ((Normalize-PathForCompare $part) -ieq $wanted) {
                $removed = $true
                continue
            }
        } catch {
            # Keep malformed legacy PATH entries.
        }
        $kept.Add($part)
    }
    if ($removed) {
        [Environment]::SetEnvironmentVariable("Path", ($kept -join ';'), "User")
        Write-Host "removed User PATH entry: $Dir" -ForegroundColor Green
    } else {
        Write-Host "User PATH did not contain: $Dir" -ForegroundColor DarkGray
    }

    $processParts = @(Split-PathList $env:Path)
    $processKept = @($processParts | Where-Object {
        try {
            (Normalize-PathForCompare $_) -ine $wanted
        } catch {
            $true
        }
    })
    $env:Path = $processKept -join ';'
}

function Stop-EnsembleProcesses([string]$InstallDir) {
    $dir = Normalize-PathForCompare $InstallDir
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

function Normalize-Clients([string[]]$Values) {
    $allowed = @("codex", "claude", "opencode")
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($value in $Values) {
        foreach ($part in ($value -split ",")) {
            $client = $part.Trim()
            if ([string]::IsNullOrWhiteSpace($client)) {
                continue
            }
            if ($allowed -notcontains $client) {
                Fail "unsupported MCP client '$client' (expected one of: $($allowed -join ', '))"
            }
            $out.Add($client)
        }
    }
    @($out | Select-Object -Unique)
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Default-InstallDir
}
$InstallDir = Normalize-PathForCompare $InstallDir
$defaultDir = Normalize-PathForCompare (Default-InstallDir)

if (($InstallDir -ine $defaultDir) -and (-not $ForceCustomInstallDir)) {
    Fail "refusing to remove custom InstallDir without -ForceCustomInstallDir: $InstallDir"
}

if ([string]::IsNullOrWhiteSpace($EnsembleExe)) {
    $candidate = Join-Path $InstallDir "ensemble.exe"
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
        $EnsembleExe = $candidate
    } else {
        $cmd = Get-Command ensemble -ErrorAction SilentlyContinue
        if ($cmd) {
            $EnsembleExe = $cmd.Source
        }
    }
}

if ($RemoveMcpConfig) {
    if ([string]::IsNullOrWhiteSpace($EnsembleExe) -or -not (Test-Path -LiteralPath $EnsembleExe -PathType Leaf)) {
        Fail "-RemoveMcpConfig needs an ensemble.exe; pass -EnsembleExe <path>"
    }
    foreach ($client in (Normalize-Clients $Clients)) {
        Write-Host "removing MCP config for $client" -ForegroundColor Cyan
        & $EnsembleExe mcp uninstall --client $client --repo $Repo
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }
}

if (-not $NoPath) {
    Remove-UserPathDir $InstallDir
} else {
    Write-Host "skipped PATH update (-NoPath)" -ForegroundColor Yellow
}

if (Test-Path -LiteralPath $InstallDir) {
    Stop-EnsembleProcesses $InstallDir
    $full = Normalize-PathForCompare $InstallDir
    if ($full.Length -lt 12 -or [System.IO.Path]::GetPathRoot($full).TrimEnd('\') -ieq $full) {
        Fail "refusing to remove suspicious install directory: $full"
    }
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
    Write-Host "removed install directory: $InstallDir" -ForegroundColor Green
} else {
    Write-Host "install directory already absent: $InstallDir" -ForegroundColor DarkGray
}

Write-Host ""
Write-Host "Open a new terminal and verify:" -ForegroundColor Cyan
Write-Host "  Get-Command ensemble"
Write-Host "It should report that ensemble is not found unless another install is on PATH."
