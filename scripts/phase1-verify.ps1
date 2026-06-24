param(
    [Parameter(Mandatory = $true)]
    [string]$Repo,
    [string]$Team = "phase1",
    [string]$Member = "operator",
    [string]$TargetDir = "",
    [string]$SmokeRoot = "",
    [switch]$RunSmoke = $false,
    [switch]$PreflightOnly = $true,
    [switch]$SkipVendor = $false,
    [int]$SmokeTimeoutSecs = 180,
    [int]$AgyTimeoutSecs = 5,
    [int]$MeshTimeoutSecs = 10,
    [int]$HelpTimeoutSecs = 5
)

$ErrorActionPreference = "Stop"

if (-not $TargetDir) {
    if (Test-Path "D:\tmp") {
        $TargetDir = Join-Path "D:\tmp" "ensemble-phase1-target-$PID"
    } else {
        $TargetDir = Join-Path $env:TEMP "ensemble-phase1-target-$PID"
    }
}

if (-not $SmokeRoot) {
    if (Test-Path "D:\tmp") {
        $SmokeRoot = Join-Path "D:\tmp" "ensemble-phase1-smoke-$PID"
    } else {
        $SmokeRoot = Join-Path $env:TEMP "ensemble-phase1-smoke-$PID"
    }
}

function Get-EnsembleCommand {
    param(
        [Parameter(Mandatory)] [string]$Repo,
        [Parameter(Mandatory)] [string]$TargetDir,
        [switch]$AllowMissing
    )

    if (Get-Command ensemble -ErrorAction SilentlyContinue) {
        return "ensemble"
    }

    if (Test-Path (Join-Path $TargetDir "debug/ensemble.exe")) {
        return Join-Path $TargetDir "debug/ensemble.exe"
    }
    if (Test-Path (Join-Path $TargetDir "release/ensemble.exe")) {
        return Join-Path $TargetDir "release/ensemble.exe"
    }
    if (Test-Path (Join-Path $Repo "target/debug/ensemble.exe")) {
        return Join-Path $Repo "target/debug/ensemble.exe"
    }
    if (Test-Path (Join-Path $Repo "target/release/ensemble.exe")) {
        return Join-Path $Repo "target/release/ensemble.exe"
    }

    if ($AllowMissing) {
        return $null
    }
    throw "ensemble executable not found in PATH or common target directories. Set PATH or run cargo build."
}
$EnsembleCmd = Get-EnsembleCommand -Repo $Repo -TargetDir $TargetDir -AllowMissing

function Run-Ensemble {
    param(
        [Parameter(Mandatory = $false, ValueFromRemainingArguments = $true)]
        [string[]]$CommandArgs
    )
    & $EnsembleCmd @CommandArgs
}

function Run-EnsembleBounded {
    param(
        [Parameter(Mandatory = $false)]
        [string[]]$CommandArgs = @(),
        [int]$TimeoutSec = 15,
        [int[]]$AllowedExitCodes = @(0),
        [switch]$AllowTimeout
    )

    $stdout = New-TemporaryFile
    $stderr = New-TemporaryFile
    try {
        if ($CommandArgs -and $CommandArgs.Count -gt 0) {
            $proc = Start-Process -FilePath $EnsembleCmd -ArgumentList $CommandArgs -NoNewWindow -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        } else {
            $proc = Start-Process -FilePath $EnsembleCmd -NoNewWindow -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        }
        if ($null -eq $proc) {
            throw "failed to start process: $EnsembleCmd"
        }
        $done = $proc.WaitForExit($TimeoutSec * 1000)
        if (-not $done) {
            try { $proc.Kill() } catch {}
            if ($AllowTimeout) {
                Write-Host "[timeout] $($CommandArgs -join ' ') after ${TimeoutSec}s" -ForegroundColor Yellow
                return $null
            }
            throw "ensemble command timed out after ${TimeoutSec}s: $($CommandArgs -join ' ')"
        }

        $outText = Get-Content -Path $stdout -Raw
        $errText = Get-Content -Path $stderr -Raw
        if ($outText) { Write-Host $outText.TrimEnd() }
        if ($errText) { Write-Host $errText.TrimEnd() -ForegroundColor DarkYellow }
        $exitCode = $proc.ExitCode
        if ($null -eq $exitCode) {
            $exitCode = $LASTEXITCODE
        }
        if ($null -eq $exitCode) {
            $exitCode = -1
        }
        if (-not ($AllowedExitCodes -contains $exitCode)) {
            throw "ensemble command failed ($exitCode): $($CommandArgs -join ' ')"
        }
        return $exitCode
    } finally {
        Remove-Item -LiteralPath $stdout -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stderr -ErrorAction SilentlyContinue
    }
}

function Step([string]$Title, [scriptblock]$Action) {
    Write-Host ""
    Write-Host "==== $Title ====" -ForegroundColor Cyan
    & $Action
}

Step "1) Build and format smoke (fmt + core tests)" {
    cargo fmt --check
    cargo test --target-dir $TargetDir control
    cargo test --target-dir $TargetDir team_
    cargo test --target-dir $TargetDir launcher
}
$EnsembleCmd = Get-EnsembleCommand -Repo $Repo -TargetDir $TargetDir
if (-not $EnsembleCmd) {
    throw "ensemble executable not found after build. Set PATH or re-run cargo build."
}

Step "2) Local control flow check (team/watch)" {
    Run-Ensemble team status --repo $Repo --team $Team
    Run-Ensemble team inbox --repo $Repo --team $Team --since 0
    Run-Ensemble watch $Member --repo $Repo --since 0
}

Step "3) MCP and service command check" {
    Run-EnsembleBounded -CommandArgs @() -TimeoutSec $HelpTimeoutSecs -AllowedExitCodes @(0, 2)
    Run-EnsembleBounded -CommandArgs @("doctor") -TimeoutSec $HelpTimeoutSecs
    Run-EnsembleBounded -CommandArgs @("nodes") -TimeoutSec $HelpTimeoutSecs -AllowTimeout
    Run-EnsembleBounded -CommandArgs @("mesh") -TimeoutSec $MeshTimeoutSecs -AllowTimeout
}

Step "4) Controlled launch preflight" {
    if (-not $SkipVendor) {
        if (Get-Command codex -ErrorAction SilentlyContinue) {
            Run-Ensemble --repo $Repo --team $Team --print-config codex --print-prompt "preflight"
        } else {
            Write-Host "codex not found, skip"
        }

        if (Get-Command claude -ErrorAction SilentlyContinue) {
            Run-Ensemble --repo $Repo --team $Team --print-config claude --continue --print-prompt "preflight"
        } else {
            Write-Host "claude not found, skip"
        }

        if (Get-Command opencode -ErrorAction SilentlyContinue) {
            Run-Ensemble --repo $Repo --team $Team --print-config opencode --print-prompt "preflight"
        } else {
            Write-Host "opencode not found, skip"
        }

        if (Get-Command agy -ErrorAction SilentlyContinue) {
            Run-Ensemble --repo $Repo --team $Team --print-config agy --prompt "preflight read board"
        } else {
            Write-Host "agy not found, skip"
        }
    } else {
        Write-Host "SkipVendor enabled, skip vendor checks"
    }
}

if ($RunSmoke) {
    $SmokeScript = Join-Path $PSScriptRoot "smoke.ps1"

    if ($PreflightOnly) {
        Step "5) Single machine smoke preflight" {
            pwsh -NoProfile -File $SmokeScript -NoBuild -PreflightOnly -SmokeRoot $SmokeRoot -TargetDir $TargetDir -TimeoutSecs $SmokeTimeoutSecs -AgyTimeoutSecs $AgyTimeoutSecs
        }
    } else {
        Step "5) Single machine smoke full" {
            pwsh -NoProfile -File $SmokeScript -NoBuild -SmokeRoot $SmokeRoot -TargetDir $TargetDir -TimeoutSecs $SmokeTimeoutSecs -AgyTimeoutSecs $AgyTimeoutSecs
        }
    }
}

Step "6) Single machine acceptance (recommended)" {
    $AcceptanceScript = Join-Path $PSScriptRoot "acceptance-single-machine.ps1"
    $ReleaseExe = Join-Path $TargetDir "release\ensemble.exe"
    if (Test-Path $ReleaseExe) {
        pwsh -NoProfile -File $AcceptanceScript -NoBuild -TargetDir $TargetDir -AgyTimeoutSecs $AgyTimeoutSecs
    } else {
        pwsh -NoProfile -File $AcceptanceScript -TargetDir $TargetDir -AgyTimeoutSecs $AgyTimeoutSecs
    }
}

Write-Host ""
Write-Host "Phase 1 verify script finished" -ForegroundColor Green
