#!/usr/bin/env pwsh
# Verify that Phantom can invoke ensemble on this machine and force local routing.
#
# This is a bridge check between Phase 1 local CLI acceptance and Phase 2 fleet work:
# Phantom -> shell tool -> ensemble agent --node local -> local AI CLI.

param(
    [string]$Repo = ".",
    [string]$TargetDir = "",
    [ValidateSet("codex", "claude", "opencode", "agy")]
    [string]$Agent = "codex",
    [string]$Prompt = "PONG",
    [switch]$NoBuild,
    [switch]$SkipDirect,
    [switch]$SkipPhantom,
    [switch]$PrintCommand
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Host "FAIL: $Message" -ForegroundColor Red
    exit 1
}

function Require-Tool([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "required tool not found on PATH: $Name"
    }
}

function Resolve-Repo([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
}

function Resolve-EnsembleExe([string]$MaybeTargetDir, [switch]$NoBuild) {
    $candidates = New-Object System.Collections.Generic.List[string]
    if (-not [string]::IsNullOrWhiteSpace($MaybeTargetDir)) {
        $targetFull = [System.IO.Path]::GetFullPath($MaybeTargetDir)
        $candidates.Add((Join-Path $targetFull "release\ensemble.exe"))
        $candidates.Add((Join-Path $targetFull "debug\ensemble.exe"))
    }

    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) {
            return [System.IO.Path]::GetFullPath($candidate)
        }
    }

    $installed = Get-Command ensemble -ErrorAction SilentlyContinue
    if ($installed) {
        return [System.IO.Path]::GetFullPath($installed.Source)
    }

    if ($NoBuild) {
        Fail "ensemble.exe not found in -TargetDir or PATH, and -NoBuild was set"
    }

    Require-Tool "cargo"
    $buildTarget = $MaybeTargetDir
    if ([string]::IsNullOrWhiteSpace($buildTarget)) {
        $buildTarget = Join-Path ([System.IO.Path]::GetTempPath()) "ensemble-phantom-target"
    }
    Write-Host "== building ensemble.exe ==" -ForegroundColor Cyan
    & cargo build --bin ensemble --target-dir $buildTarget
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build failed with exit $LASTEXITCODE"
    }
    $built = Join-Path ([System.IO.Path]::GetFullPath($buildTarget)) "debug\ensemble.exe"
    if (-not (Test-Path -LiteralPath $built)) {
        Fail "built ensemble.exe not found: $built"
    }
    return $built
}

function First-JsonObject([string]$Title, [object[]]$Lines) {
    foreach ($line in $Lines) {
        $text = [string]$line
        $trimmed = $text.Trim()
        if ($trimmed.StartsWith("{")) {
            try {
                return $trimmed | ConvertFrom-Json
            } catch {
                Fail "$Title emitted invalid JSON: $($_.Exception.Message)`n$trimmed"
            }
        }
    }
    Fail "$Title did not emit a JSON object:`n$($Lines -join [Environment]::NewLine)"
}

function Assert-AgentResult([string]$Title, $Result, [string]$ExpectedAgent) {
    if (-not $Result.ok) {
        Fail "$Title returned ok=false: $($Result | ConvertTo-Json -Compress)"
    }
    if ($Result.node -ne "local") {
        Fail "$Title did not force local node: $($Result | ConvertTo-Json -Compress)"
    }
    if ($Result.agent -ne $ExpectedAgent) {
        Fail "$Title returned unexpected agent '$($Result.agent)' (expected '$ExpectedAgent')"
    }
}

function Quote-CmdArg([string]$Value) {
    if ($Value.Contains('"')) {
        Fail "this verifier does not support double quotes inside command arguments: $Value"
    }
    return '"' + $Value + '"'
}

function Assert-ShellTokenSafe([string]$Title, [string]$Value) {
    if ($Value -notmatch '^[A-Za-z0-9_./:@\\=-]+$') {
        Fail "$Title contains characters that cannot be passed through Phantom's simple shell bridge: $Value"
    }
}

$repoFull = Resolve-Repo $Repo
if (-not (Test-Path -LiteralPath $repoFull)) {
    Fail "repo path not found: $repoFull"
}

$ensembleExe = Resolve-EnsembleExe $TargetDir -NoBuild:$NoBuild
Write-Host "ensemble: $ensembleExe"
Write-Host "repo: $repoFull"
Write-Host "agent: $Agent"

if (-not $SkipDirect) {
    Write-Host ""
    Write-Host "== direct ensemble local-agent check ==" -ForegroundColor Cyan
    $direct = & $ensembleExe agent $Agent $Prompt --node local --repo $repoFull --json 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "direct ensemble call failed with exit $LASTEXITCODE`n$($direct -join [Environment]::NewLine)"
    }
    $directJson = First-JsonObject "direct ensemble" $direct
    Assert-AgentResult "direct ensemble" $directJson $Agent
    $directJson | ConvertTo-Json -Compress
}

if (-not $SkipPhantom) {
    Require-Tool "phantom"
    Write-Host ""
    Write-Host "== phantom shell -> ensemble local-agent check ==" -ForegroundColor Cyan

    $tmpRoot = if (Test-Path -LiteralPath "D:\tmp") { "D:\tmp" } else { [System.IO.Path]::GetTempPath() }
    $shimDir = Join-Path $tmpRoot "ensemble-phantom-$PID"
    New-Item -ItemType Directory -Path $shimDir -Force | Out-Null
    $shim = Join-Path $shimDir "ensemble-phantom.cmd"
    try {
        "@echo off`r`n$(Quote-CmdArg $ensembleExe) %*`r`n" | Set-Content -LiteralPath $shim -Encoding ascii
        Assert-ShellTokenSafe "temporary Phantom shim path" $shim
        Assert-ShellTokenSafe "repo path" $repoFull
        Assert-ShellTokenSafe "prompt" $Prompt
        $command = "cmd /c $shim agent $Agent $Prompt --node local --repo $repoFull --json"
        if ($PrintCommand) {
            Write-Host $command
        }
        $argsJson = @{ command = $command } | ConvertTo-Json -Compress
        $phantom = & phantom tool shell --args $argsJson 2>&1
        if ($LASTEXITCODE -ne 0) {
            Fail "phantom tool shell failed with exit $LASTEXITCODE`n$($phantom -join [Environment]::NewLine)"
        }
        $phantomJson = First-JsonObject "phantom tool shell" $phantom
        Assert-AgentResult "phantom tool shell" $phantomJson $Agent
        $phantomJson | ConvertTo-Json -Compress
    } finally {
        if (Test-Path -LiteralPath $shimDir) {
            Remove-Item -LiteralPath $shimDir -Recurse -Force
        }
    }
}

Write-Host ""
Write-Host "Phantom single-machine ensemble invocation passed." -ForegroundColor Green
