#!/usr/bin/env pwsh
# ensemble single-machine smoke:
# build ensemble.exe, create a throwaway git repo, and drive one governed multi-AI run locally.
#
# Default path:
#   codex implements -> claude reviews -> test gate -> auto-merge -> watch transcript check
#
# Team preflight:
#   team board say/inbox/status -> launcher dry-run for codex/claude/opencode
#   -> agy prompt preview -> bounded agy wrapper result/flake -> artifact recording
#
# Examples:
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1 -Reviewers claude,agy -TimeoutSecs 240
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1 -Task "Create RESULT.txt with SINGLE_MACHINE_MULTI_AI_OK" -ProbeAgents
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1 -PreflightOnly
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1 -Reviewers codex,claude -AllowEscalatedRun

param(
    [string]$Task = "Create or overwrite RESULT.txt with exactly one line: SINGLE_MACHINE_MULTI_AI_OK. Do not modify any other file.",
    [ValidateSet("codex", "claude", "opencode", "agy")]
    [string]$Implementer = "codex",
    [string[]]$Reviewers = @("claude"),
    [int]$TimeoutSecs = 180,
    [int]$AgyTimeoutSecs = 30,
    [string]$Watch = "single-machine-smoke",
    [string]$Team = "single-machine-smoke",
    [string]$SmokeRoot = "",
    [string]$TargetDir = "",
    [switch]$NoBuild,
    [switch]$ProbeAgents,
    [switch]$NoMerge,
    [switch]$PreflightOnly,
    [switch]$SkipLauncherDryRun,
    [switch]$SkipAgyWrapper,
    [switch]$SkipSupervisor,
    [switch]$AllowEscalatedRun
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

function Run-Step([string]$Title, [scriptblock]$Body) {
    Write-Host "== $Title ==" -ForegroundColor Cyan
    & $Body
    if ($LASTEXITCODE -ne 0) {
        Fail "$Title failed with exit $LASTEXITCODE"
    }
}

function Require-Tool([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "required tool not found on PATH: $Name"
    }
}

function Write-AsciiFile([string]$Path, [string]$Content) {
    $Content | Set-Content -LiteralPath $Path -Encoding ascii
}

function Read-TextFile([string]$Path) {
    if (Test-Path $Path) {
        return Get-Content -LiteralPath $Path -Raw
    }
    return ""
}

function Write-Utf8File([string]$Path, [string]$Content) {
    $Content | Set-Content -LiteralPath $Path -Encoding utf8
}

function Invoke-EnsembleCapture(
    [string]$Title,
    [string[]]$CommandArgs,
    [string]$ArtifactName,
    [switch]$AllowFailure
) {
    Write-Host "== $Title ==" -ForegroundColor Cyan
    $stdoutPath = Join-Path $ArtifactDir $ArtifactName
    $stderrPath = "$stdoutPath.stderr"
    & $exe @CommandArgs > $stdoutPath 2> $stderrPath
    $code = $LASTEXITCODE
    $stdout = Read-TextFile $stdoutPath
    $stderr = Read-TextFile $stderrPath
    if (-not [string]::IsNullOrWhiteSpace($stdout)) {
        Write-Host $stdout.TrimEnd()
    }
    if (-not [string]::IsNullOrWhiteSpace($stderr)) {
        Write-Host $stderr.TrimEnd() -ForegroundColor DarkYellow
    }
    if (($code -ne 0) -and (-not $AllowFailure)) {
        Fail "$Title failed with exit $code. See $stdoutPath and $stderrPath"
    }
    return [pscustomobject]@{
        Code = $code
        Stdout = $stdout
        Stderr = $stderr
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
    }
}

function Convert-JsonOrFail([string]$Title, [string]$JsonText) {
    try {
        return $JsonText | ConvertFrom-Json
    } catch {
        Fail "$Title did not return parseable JSON: $($_.Exception.Message)`n$JsonText"
    }
}

function Assert-Contains([string]$Title, [string]$Text, [string]$Needle) {
    if (-not $Text.Contains($Needle)) {
        Fail "$Title did not contain expected text: $Needle"
    }
}

function Contains-RunOutcome([string]$Text, [string]$Kind) {
    return $Text -match [regex]::Escape($Kind)
}

function Invoke-WithProcessEnv([string]$Name, [string]$Value, [scriptblock]$Body) {
    $old = [Environment]::GetEnvironmentVariable($Name, "Process")
    [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
    try {
        & $Body
    } finally {
        [Environment]::SetEnvironmentVariable($Name, $old, "Process")
    }
}

function Normalize-AgentList([string[]]$Values, [string]$ParamName) {
    $allowed = @("codex", "claude", "opencode", "agy")
    $items = New-Object System.Collections.Generic.List[string]
    foreach ($value in $Values) {
        foreach ($part in ($value -split ",")) {
            $agent = $part.Trim()
            if ([string]::IsNullOrWhiteSpace($agent)) {
                continue
            }
            if ($allowed -notcontains $agent) {
                Fail "$ParamName contains unsupported agent '$agent' (expected one of: $($allowed -join ', '))"
            }
            $items.Add($agent)
        }
    }
    return @($items)
}

function Toml-AgentTimeouts([string[]]$Agents, [int]$Timeout) {
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($agent in $Agents) {
        $out.Add("")
        $out.Add("[agents.$agent]")
        $out.Add("timeout = $Timeout")
    }
    $out -join "`n"
}

$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    if ($NoBuild) {
        $TargetDir = Join-Path $repo "target"
    } elseif (Test-Path "D:\tmp") {
        $TargetDir = Join-Path "D:\tmp" "ensemble-smoke-target-$PID"
    } else {
        $TargetDir = Join-Path $env:TEMP "ensemble-smoke-target-$PID"
    }
}
$exe = Join-Path $TargetDir "release\ensemble.exe"
$Reviewers = @(Normalize-AgentList $Reviewers "Reviewers")
$allAgents = @($Implementer) + @($Reviewers)
$uniqueReviewers = @($Reviewers | Select-Object -Unique)
$uniqueAgents = @($allAgents | Select-Object -Unique)

if ($Reviewers.Count -lt 1) {
    Fail "at least one reviewer is required"
}
if ($uniqueReviewers.Count -ne $Reviewers.Count) {
    Fail "reviewers must be unique"
}
if ($Reviewers -contains $Implementer) {
    Fail "implementer and reviewers must be different agents for a meaningful multi-AI smoke"
}
if ($TimeoutSecs -lt 10) {
    Fail "TimeoutSecs must be >= 10"
}
if ($AgyTimeoutSecs -lt 1) {
    Fail "AgyTimeoutSecs must be >= 1"
}

Require-Tool git
if (-not $NoBuild) {
    Require-Tool cargo
}
if (-not $PreflightOnly) {
    foreach ($agent in $uniqueAgents) {
        Require-Tool $agent
    }
}

if (-not $NoBuild) {
    Run-Step "building release ensemble.exe" {
        Push-Location $repo
        try {
            $built = $false
            for ($i = 1; $i -le 3; $i++) {
                $env:CARGO_TARGET_DIR = $TargetDir
                cargo build --release --bin ensemble
                if ($LASTEXITCODE -eq 0) {
                    $built = $true
                    break
                }
                Write-Host "  build try $i failed; retrying in 4s" -ForegroundColor Yellow
                Start-Sleep -Seconds 4
            }
            if (-not $built) {
                exit 1
            }
        } finally {
            Pop-Location
        }
    }
}

if (-not (Test-Path $exe)) {
    Fail "ensemble.exe not found at $exe; run without -NoBuild first"
}

if ([string]::IsNullOrWhiteSpace($SmokeRoot)) {
    $SmokeRoot = Join-Path $env:USERPROFILE "ensemble-smoke-single-machine"
}
if (Test-Path $SmokeRoot) {
    Remove-Item -LiteralPath $SmokeRoot -Recurse -Force
}
New-Item -ItemType Directory -Force $SmokeRoot | Out-Null
$ArtifactDir = Join-Path $SmokeRoot "smoke-artifacts"
New-Item -ItemType Directory -Force $ArtifactDir | Out-Null

Write-Host "  exe: $exe" -ForegroundColor DarkGray
Write-Host "  target: $TargetDir" -ForegroundColor DarkGray
Write-Host "  repo: $SmokeRoot" -ForegroundColor DarkGray
Write-Host "  crew: $Implementer -> $($Reviewers -join ', ')" -ForegroundColor DarkGray
Write-Host "  team: $Team" -ForegroundColor DarkGray
Write-Host "  watch: $Watch" -ForegroundColor DarkGray
Write-Host "  artifacts: $ArtifactDir" -ForegroundColor DarkGray
Write-Host ""

Push-Location $SmokeRoot
try {
    Run-Step "initializing smoke git repo" {
        git init -q
    }
    Run-Step "configuring smoke git repo" {
        git config user.email "single-machine-smoke@example.invalid"
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        git config user.name "ensemble smoke"
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        git config core.autocrlf false
    }

    Write-AsciiFile ".gitignore" @"
.ensemble/
crew.toml
smoke-artifacts/
"@
    Write-AsciiFile "README.md" "# ensemble single-machine smoke`n"
    Run-Step "committing smoke seed" {
        git add .gitignore README.md
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        git commit -q -m init
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        git branch -M main
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }

    $pipeline = @("implement")
    for ($i = 0; $i -lt $Reviewers.Count; $i++) {
        $pipeline += "review$($i + 1)"
    }
    $pipelineToml = ($pipeline | ForEach-Object { '"' + $_ + '"' }) -join ", "

    $roles = New-Object System.Collections.Generic.List[string]
    $roles.Add("[roles.implement]")
    $roles.Add("agent = `"$Implementer`"")
    for ($i = 0; $i -lt $Reviewers.Count; $i++) {
        $role = "review$($i + 1)"
        $agent = $Reviewers[$i]
        $roles.Add("")
        $roles.Add("[roles.$role]")
        $roles.Add("agent = `"$agent`"")
        $roles.Add("blind = true")
    }

    $crewToml = @"
pipeline = [$pipelineToml]

[gate]
min_approvals = $($Reviewers.Count)
max_rounds = 2
on_flake = "exclude"
stall_limit = 2
max_task_secs = $([Math]::Max($TimeoutSecs * ($Reviewers.Count + 2), 120))

[test]
command = "findstr /C:SINGLE_MACHINE_MULTI_AI_OK RESULT.txt"

$($roles -join "`n")
$(Toml-AgentTimeouts -Agents $uniqueAgents -Timeout $TimeoutSecs)
"@
    Write-AsciiFile "crew.toml" $crewToml

    Invoke-EnsembleCapture "doctor preflight" @("doctor") "doctor.txt" | Out-Null

    Invoke-EnsembleCapture `
        "team board say" `
        @("team", "say", "operator: smoke started", "--repo", ".", "--team", $Team, "--from", "operator") `
        "team-say.txt" | Out-Null

    $teamStatus = Invoke-EnsembleCapture `
        "team status json" `
        @("team", "status", "--repo", ".", "--team", $Team, "--json") `
        "team-status.initial.json"
    $teamStatusJson = Convert-JsonOrFail "team status json" $teamStatus.Stdout
    if ($teamStatusJson.team -ne $Team) {
        Fail "team status reported team '$($teamStatusJson.team)', expected '$Team'"
    }

    $teamInbox = Invoke-EnsembleCapture `
        "team inbox json" `
        @("team", "inbox", "--repo", ".", "--team", $Team, "--since", "0", "--json") `
        "team-inbox.initial.json"
    $teamInboxJson = Convert-JsonOrFail "team inbox json" $teamInbox.Stdout
    $operatorPosts = @($teamInboxJson.messages | Where-Object {
        $_.from -eq "operator" -and $_.kind -eq "note" -and $_.body -eq "operator: smoke started"
    })
    if ($operatorPosts.Count -lt 1) {
        Fail "team inbox did not include the operator smoke-start message"
    }

    if (-not $SkipLauncherDryRun) {
        foreach ($client in @("codex", "claude", "opencode")) {
            $member = "$client@smoke"
            $launcherArgs = @("--repo", ".", "--team", $Team, "--member", $member, "--confirm-policy", "ask", "--print-config", $client)
            if ($client -eq "codex") {
                $codexSmokeHome = Join-Path $SmokeRoot ".codex-smoke"
                New-Item -ItemType Directory -Force $codexSmokeHome | Out-Null
                $preview = Invoke-WithProcessEnv "CODEX_HOME" $codexSmokeHome {
                    Invoke-EnsembleCapture "launcher dry-run $client" $launcherArgs "launcher-$client.txt"
                }
            } else {
                $preview = Invoke-EnsembleCapture "launcher dry-run $client" $launcherArgs "launcher-$client.txt"
            }
            Assert-Contains "launcher dry-run $client" $preview.Stdout "client=$client"
            Assert-Contains "launcher dry-run $client" $preview.Stdout "member=$member"
            Assert-Contains "launcher dry-run $client" $preview.Stdout "team=$Team"
            Assert-Contains "launcher dry-run $client" $preview.Stdout "--team"
            Assert-Contains "launcher dry-run $client" $preview.Stdout $Team
        }

        $agyPromptPreview = Invoke-EnsembleCapture `
            "launcher dry-run agy prompt" `
            @("--repo", ".", "--team", $Team, "--member", "agy@smoke", "--timeout", "$AgyTimeoutSecs", "--confirm-policy", "ask", "--print-prompt", "agy", "--prompt", "Summarize the team board for smoke preflight.") `
            "launcher-agy-prompt.txt"
        Assert-Contains "launcher dry-run agy prompt" $agyPromptPreview.Stdout "agy@smoke"
        Assert-Contains "launcher dry-run agy prompt" $agyPromptPreview.Stdout "local ensemble team ``$Team"
    }

    if (-not $SkipAgyWrapper) {
        $agyTurn = Invoke-EnsembleCapture `
            "bounded agy team wrapper" `
            @("--repo", ".", "--team", $Team, "--member", "agy@smoke", "--timeout", "$AgyTimeoutSecs", "--confirm-policy", "ask", "--json", "agy", "--prompt", "Read the team board and reply with a one-line smoke status.") `
            "agy-wrapper.json" `
            -AllowFailure
        if ($agyTurn.Code -ne 0) {
            Write-Host "  agy wrapper exited $($agyTurn.Code); verifying visible board flake/result" -ForegroundColor Yellow
        }
        $afterAgy = Invoke-EnsembleCapture `
            "team inbox after agy" `
            @("team", "inbox", "--repo", ".", "--team", $Team, "--since", "0", "--json") `
            "team-inbox.after-agy.json"
        $afterAgyJson = Convert-JsonOrFail "team inbox after agy" $afterAgy.Stdout
        $agyPosts = @($afterAgyJson.messages | Where-Object {
            $_.from -eq "agy@smoke" -and ($_.kind -eq "result" -or $_.kind -eq "flake")
        })
        if ($agyPosts.Count -lt 1) {
            Fail "agy wrapper did not post a visible result or flake to the team board"
        }
    }

    if ($ProbeAgents) {
        if ($PreflightOnly) {
            Fail "-ProbeAgents cannot be combined with -PreflightOnly"
        }
        foreach ($agent in $uniqueAgents) {
            Run-Step "probing $agent" {
                & $exe agent $agent "Reply with exactly: PONG" --repo . --no-discover --json
            }
        }
    }

    if ($PreflightOnly) {
        Invoke-EnsembleCapture `
            "final team status json" `
            @("team", "status", "--repo", ".", "--team", $Team, "--json") `
            "team-status.final.json" | Out-Null
        $gitStatus = git status --short
        if ($LASTEXITCODE -ne 0) {
            Fail "git status failed with exit $LASTEXITCODE"
        }
        Write-Utf8File (Join-Path $ArtifactDir "git-status.final.txt") (($gitStatus | Out-String).TrimEnd())
        if ($gitStatus) {
            Fail "preflight smoke repo is dirty:`n$gitStatus"
        }
        Write-Host ""
        Write-Host "== single-machine team preflight passed ==" -ForegroundColor Green
        Write-Host "  repo kept for inspection: $SmokeRoot" -ForegroundColor DarkGray
        Write-Host "  artifacts: $ArtifactDir" -ForegroundColor DarkGray
        exit 0
    }

    $runArgs = @("run", $Task, "--repo", ".", "--crew", "crew.toml", "--watch", $Watch, "--no-discover")
    if (-not $NoMerge) {
        $runArgs += "--merge"
    }

    $governedRun = Invoke-EnsembleCapture "governed multi-AI run" $runArgs "governed-run.txt" -AllowFailure
    $governedRunText = "$($governedRun.Stdout)`n$($governedRun.Stderr)"
    $runLanded = Contains-RunOutcome $governedRunText "LANDED"
    $runEscalated = Contains-RunOutcome $governedRunText "ESCALATED"

    if (-not $runLanded -and -not $runEscalated) {
        if ($governedRun.Code -ne 0) {
            Fail "governed multi-AI run did not produce a terminal LANDED or ESCALATED state"
        }
    }

    Invoke-EnsembleCapture "watch transcript render" @("watch", $Watch, "--repo", ".") "watch-transcript.txt" -AllowFailure | Out-Null

    if (($governedRun.Code -eq 0) -and (-not $SkipSupervisor)) {
        Invoke-EnsembleCapture `
            "supervisor advisory json" `
            @("supervise", $Watch, "--repo", ".", "--team", $Team, "--agent", "claude", "--json") `
            "supervisor-advisory.json" `
            -AllowFailure | Out-Null
    }

    Invoke-EnsembleCapture `
        "final team inbox json" `
        @("team", "inbox", "--repo", ".", "--team", $Team, "--since", "0", "--json") `
        "team-inbox.final.json" | Out-Null

    Invoke-EnsembleCapture `
        "final team status json" `
        @("team", "status", "--repo", ".", "--team", $Team, "--json") `
        "team-status.final.json" | Out-Null

    $finalStatus = git status --short
    if ($LASTEXITCODE -ne 0) {
        Fail "git status failed with exit $LASTEXITCODE"
    }
    Write-Utf8File (Join-Path $ArtifactDir "git-status.final.txt") (($finalStatus | Out-String).TrimEnd())
    $finalLog = git log --oneline --decorate --all -10
    if ($LASTEXITCODE -ne 0) {
        Fail "git log failed with exit $LASTEXITCODE"
    }
    Write-Utf8File (Join-Path $ArtifactDir "git-log.final.txt") (($finalLog | Out-String).TrimEnd())

    if ($governedRun.Code -ne 0) {
        if ($AllowEscalatedRun -and $runEscalated) {
            Write-Host "  run escalated; continuing by design because -AllowEscalatedRun is set." -ForegroundColor Yellow
        } else {
            Fail "governed multi-AI run failed with exit $($governedRun.Code); artifacts recorded under $ArtifactDir"
        }
    }

    if (-not $runLanded -and -not $AllowEscalatedRun) {
        Fail "governed multi-AI run did not land."
    }

    if ($runEscalated -and -not $AllowEscalatedRun) {
        Fail "governed multi-AI run escalated instead of landing."
    }

    if (-not $NoMerge) {
        if ($runLanded) {
            if (-not (Test-Path "RESULT.txt")) {
                Fail "RESULT.txt was not merged onto main"
            }
            $result = Get-Content -LiteralPath "RESULT.txt" -Raw
            if ($result -notmatch "SINGLE_MACHINE_MULTI_AI_OK") {
                Fail "RESULT.txt does not contain SINGLE_MACHINE_MULTI_AI_OK"
            }
        } elseif ($AllowEscalatedRun) {
            Write-Host "  skipping RESULT.txt merge assertions because run escalated." -ForegroundColor Yellow
        } else {
            Fail "run landed false and merge was requested."
        }
        $dirty = git status --porcelain
        if ($LASTEXITCODE -ne 0) {
            Fail "git status failed with exit $LASTEXITCODE"
        }
        if ($dirty -and -not ($AllowEscalatedRun -and $runEscalated)) {
            Fail "smoke repo is dirty after auto-merge:`n$dirty"
        }
    } else {
        Write-Host "  merged assertions skipped by -NoMerge." -ForegroundColor DarkGray
    }
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "== single-machine multi-AI smoke passed ==" -ForegroundColor Green
Write-Host "  repo kept for inspection: $SmokeRoot" -ForegroundColor DarkGray
Write-Host "  artifacts: $ArtifactDir" -ForegroundColor DarkGray
