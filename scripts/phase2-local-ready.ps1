#!/usr/bin/env pwsh
# One-machine readiness gate before the operator moves to the real 5-node Phase 2 run.
#
# This script chains existing verifiers; it does not create a new acceptance standard.
# Default path:
#   Phase 1 deterministic acceptance -> Phantom bridge -> Phase 2 Slice A/B-preflight/C-local.
# Clean reinstall is opt-in because it intentionally mutates the user-level install.

param(
    [string]$Repo = ".",
    [string]$TargetDir = "",
    [string]$SmokeRoot = "",
    [string]$FleetManifest = "",
    [string]$FleetNode = "",
    [switch]$CheckFleetManifestNodes,
    [switch]$SkipPhase1,
    [switch]$SkipPhantom,
    [switch]$SkipPhase2,
    [switch]$RunCleanReinstall,
    [int]$AgyTimeoutSecs = 1
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($SkipPhase1 -and $SkipPhantom -and $SkipPhase2 -and -not $RunCleanReinstall) {
    Write-Host "FAIL: all readiness stages were skipped; enable at least one check." -ForegroundColor Red
    exit 1
}

function Fail([string]$Message) {
    Write-Host "FAIL: $Message" -ForegroundColor Red
    exit 1
}

function Step([string]$Title, [scriptblock]$Action) {
    Write-Host ""
    Write-Host "==== $Title ====" -ForegroundColor Cyan
    & $Action
    if ($LASTEXITCODE -ne 0) {
        Fail "$Title failed with exit $LASTEXITCODE"
    }
}

function Resolve-PathForScript([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
}

$repoFull = Resolve-PathForScript $Repo
if (-not (Test-Path -LiteralPath $repoFull -PathType Container)) {
    Fail "repo path not found: $repoFull"
}

if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    if (Test-Path "D:\tmp") {
        $TargetDir = Join-Path "D:\tmp" "ensemble-phase2-local-ready-target"
    } else {
        $TargetDir = Join-Path $env:TEMP "ensemble-phase2-local-ready-target"
    }
}
if ([string]::IsNullOrWhiteSpace($SmokeRoot)) {
    if (Test-Path "D:\tmp") {
        $SmokeRoot = Join-Path "D:\tmp" "ensemble-phase2-local-ready"
    } else {
        $SmokeRoot = Join-Path $env:TEMP "ensemble-phase2-local-ready"
    }
}

$targetFull = Resolve-PathForScript $TargetDir
$smokeRootFull = Resolve-PathForScript $SmokeRoot
$scriptDir = $PSScriptRoot

Write-Host "repo: $repoFull"
Write-Host "target: $targetFull"
Write-Host "smoke-root: $smokeRootFull"

if (-not $SkipPhase1) {
    Step "Phase 1 deterministic acceptance" {
        $acceptance = Join-Path $scriptDir "acceptance-single-machine.ps1"
        pwsh -NoProfile -File $acceptance `
            -SmokeRoot (Join-Path $smokeRootFull "phase1-acceptance") `
            -TargetDir $targetFull `
            -AgyTimeoutSecs $AgyTimeoutSecs
    }
}

if (-not $SkipPhantom) {
    if (Get-Command phantom -ErrorAction SilentlyContinue) {
        Step "Phantom single-machine bridge" {
            $phantomBridge = Join-Path $scriptDir "phantom-single-machine.ps1"
            pwsh -NoProfile -File $phantomBridge `
                -Repo $repoFull `
                -TargetDir $targetFull `
                -NoBuild
        }
    } else {
        Fail "phantom not found on PATH; install Phantom or pass -SkipPhantom explicitly"
    }
}

if (-not $SkipPhase2) {
    Step "Phase 2 local slices A/B-preflight/C-local" {
        $phase2 = Join-Path $scriptDir "phase2-verify.ps1"
        $args = @(
            "-NoProfile", "-File", $phase2,
            "-Repo", $repoFull,
            "-TargetDir", $targetFull,
            "-SkipSliceD",
            "-SliceBPreflightOnly"
        )
        if (-not [string]::IsNullOrWhiteSpace($FleetManifest)) {
            $args += @("-FleetManifest", $FleetManifest)
        }
        if (-not [string]::IsNullOrWhiteSpace($FleetNode)) {
            $args += @("-FleetNode", $FleetNode)
        }
        if ($CheckFleetManifestNodes) {
            $args += "-CheckFleetManifestNodes"
        }
        pwsh @args
    }
}

if ($RunCleanReinstall) {
    Step "Phase 2 clean reinstall slice D" {
        $phase2 = Join-Path $scriptDir "phase2-verify.ps1"
        pwsh -NoProfile -File $phase2 `
            -Repo $repoFull `
            -TargetDir $targetFull `
            -SmokeRoot (Join-Path $smokeRootFull "slice-d") `
            -SkipSliceA `
            -SkipSliceB `
            -SkipSliceC
    }
}

Write-Host ""
Write-Host "Phase 2 local readiness passed." -ForegroundColor Green
