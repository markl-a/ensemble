#!/usr/bin/env pwsh
# Phase-2 verification script for single-machine + optional remote-control checks.
#
# It can run the four acceptance slices from the Phase-2 goal:
#  A) control-plane smoke + local/remote routing errors
#  B) run/watch/steer/abort + terminal decision signal
#  C) fleet mesh/nodes checks
#  D) clean uninstall -> install -> smoke/reinstall path
#
# Example:
#   pwsh -NoProfile scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble
#
# Cross-machine checks (Slice B/C/D) are command-driven and will skip when inputs are not
# provided for this host.

param(
    [string]$Repo = ".",
    [string]$InstallDir = "",
    [string]$Team = "main",
    [string]$Watch = "main",
    [string]$Crew = "crew-main.toml",
    [string]$Task = "",
    [int]$RunTimeoutSecs = 180,
    [int]$SmokeTimeoutSecs = 120,
    [int]$UpWarmupSecs = 6,
    [string]$RemoteNode = "",
    [string]$LocalFleetNode = "",
    [string[]]$ExpectedFleetNodes = @(),
    [string]$FleetManifest = "",
    [string]$FleetNode = "",
    [switch]$CheckFleetManifestNodes,
    [string]$SmokeRoot = "",
    [string]$TargetDir = "",
    [switch]$SkipSliceA,
    [switch]$SkipSliceB,
    [switch]$SkipSliceC,
    [switch]$SkipSliceD,
    [switch]$SkipCleanSmoke,
    [string]$UpBind = ""

)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Host "FAIL: $Message" -ForegroundColor Red
    exit 1
}

function Step([string]$Title, [scriptblock]$Action) {
    Write-Host ""
    Write-Host "==== $Title ====" -ForegroundColor Cyan
    & $Action
}

function Require-Tool([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "required tool not found on PATH: $Name"
    }
}

function Get-UrlHosts([string]$Text) {
    $hosts = New-Object System.Collections.Generic.List[string]
    foreach ($match in [regex]::Matches($Text, 'https?://(\[[^\]]+\]|[^/:\s]+)(?::\d+)?')) {
        $hosts.Add($match.Groups[1].Value.Trim('[', ']'))
    }
    return $hosts
}

function Get-ExpectedHost([string]$NodeName) {
    $trimmed = $NodeName.Trim().TrimEnd('/')
    $match = [regex]::Match($trimmed, '^(?:https?://)?(\[[^\]]+\]|[^/:\s]+)(?::\d+)?(?:/.*)?$')
    if ($match.Success) {
        return $match.Groups[1].Value.Trim('[', ']')
    }
    return $trimmed.Trim('[', ']')
}

function Test-HostPresent([string]$ExpectedNode, [string[]]$Hosts) {
    $expected = Get-ExpectedHost $ExpectedNode
    foreach ($hostName in $Hosts) {
        if ($hostName.Equals($expected, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
        if ($hostName.StartsWith("$expected.", [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Expand-NodeList([string[]]$Nodes) {
    $expanded = New-Object System.Collections.Generic.List[string]
    foreach ($node in $Nodes) {
        foreach ($part in ([string]$node).Split(',')) {
            $trimmed = $part.Trim()
            if ($trimmed.Length -gt 0) {
                $expanded.Add($trimmed)
            }
        }
    }
    return $expanded
}

function Resolve-VerifyPath([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $Repo $Path))
}

function Read-FleetManifestInfo([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }
    $manifestPath = Resolve-VerifyPath $Path
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        Fail "fleet manifest not found: $manifestPath"
    }
    try {
        $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    } catch {
        Fail "fleet manifest is not valid JSON: $manifestPath. $($_.Exception.Message)"
    }
    $nodesProp = $manifest.PSObject.Properties["nodes"]
    if ($null -eq $nodesProp -or @($nodesProp.Value).Count -eq 0) {
        Fail "fleet manifest must define a non-empty nodes array: $manifestPath"
    }
    $nodes = @($nodesProp.Value | ForEach-Object { [string]$_ })
    $conductorProp = $manifest.PSObject.Properties["conductor"]
    $conductor = if ($null -ne $conductorProp -and -not [string]::IsNullOrWhiteSpace([string]$conductorProp.Value)) {
        [string]$conductorProp.Value
    } else {
        [string]$nodes[0]
    }
    return [pscustomobject]@{
        Path      = $manifestPath
        Nodes     = $nodes
        Conductor = $conductor
    }
}

function Invoke-FleetManifestPlan([string]$Path, [string]$NodeName) {
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }
    $info = Read-FleetManifestInfo $Path
    $fleetScript = Join-Path $PSScriptRoot "phase2-fleet.ps1"
    if (-not (Test-Path -LiteralPath $fleetScript -PathType Leaf)) {
        Fail "phase2 fleet script missing: $fleetScript"
    }
    $nodeArg = if ([string]::IsNullOrWhiteSpace($NodeName)) {
        "all"
    } else {
        $NodeName
    }
    Write-Host "== fleet manifest plan ($nodeArg) ==" -ForegroundColor DarkGray
    $planOut = pwsh -NoProfile -File $fleetScript -Manifest $info.Path -Node $nodeArg -PlanOnly 2>&1
    $planCode = $LASTEXITCODE
    $planText = ($planOut | Out-String).TrimEnd()
    if (-not [string]::IsNullOrWhiteSpace($planText)) {
        Write-Host $planText
    }
    if ($planCode -ne 0) {
        Fail "phase2-fleet plan failed for manifest $($info.Path)"
    }
    return $info
}

function Get-EnsembleCmd {
    if (Get-Command ensemble -ErrorAction SilentlyContinue) {
        return "ensemble"
    }
    $cands = @()
    if ($TargetDir) {
        $cands += Join-Path $TargetDir "release/ensemble.exe"
        $cands += Join-Path $TargetDir "debug/ensemble.exe"
    }
    $cands += Join-Path $Repo "target/release/ensemble.exe"
    $cands += Join-Path $Repo "target/debug/ensemble.exe"
    $cands += Join-Path $Repo "target/release/ensemble.exe"
    foreach ($cand in $cands) {
        if ($cand -and (Test-Path -LiteralPath $cand)) {
            return $cand
        }
    }
    if ($InstallDir) {
        $installed = Join-Path $InstallDir "ensemble.exe"
        if (Test-Path -LiteralPath $installed) {
            return $installed
        }
    } elseif ($env:LOCALAPPDATA) {
        $installed = Join-Path $env:LOCALAPPDATA "ensemble\\bin\\ensemble.exe"
        if (Test-Path -LiteralPath $installed) {
            return $installed
        }
    }
    return $null
}

function Invoke-EnsembleCapture {
    param(
        [string[]]$CommandArgs,
        [string]$Title = "",
        [int]$TimeoutSec = 30,
        [int[]]$AllowedExitCodes = @(0),
        [switch]$AllowFailure,
        [switch]$AllowTimeout
    )

    function Quote-CmdArg([string]$Value) {
        if ([string]::IsNullOrEmpty($Value)) {
            return '""'
        }
        $escaped = $Value.Replace('\', '\\').Replace('"', '\"')
        if ($Value -match '[\s"]') {
            return "`"$escaped`""
        }
        return $escaped
    }

    $ensemble = Get-EnsembleCmd
    if (-not $ensemble) {
        Fail "could not locate ensemble command. Ensure PATH or provide -TargetDir."
    }
    $stdout = New-TemporaryFile
    $stderr = New-TemporaryFile
    try {
        if ($Title) {
            Write-Host "== $Title ==" -ForegroundColor DarkGray
        }
        $quotedArgs = @()
        foreach ($arg in $CommandArgs) {
            $quotedArgs += Quote-CmdArg $arg
        }
        $argumentString = $quotedArgs -join " "
        $proc = Start-Process -FilePath $ensemble -ArgumentList $argumentString -PassThru -NoNewWindow `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WorkingDirectory $Repo
        if (-not $proc) {
            throw "failed to start process: $ensemble"
        }
        $done = $proc.WaitForExit($TimeoutSec * 1000)
        if (-not $done) {
            try {
                $proc.Kill() | Out-Null
            } catch { }
            if ($AllowTimeout) {
                return [pscustomobject]@{
                    Code      = 124
                    Stdout    = ""
                    Stderr    = ""
                    TimedOut  = $true
                    StdoutPath = $stdout.FullName
                    StderrPath = $stderr.FullName
                }
            }
                Fail "$($CommandArgs -join ' ') timed out after ${TimeoutSec}s"
            }
        $code = $proc.ExitCode
        $outText = Get-Content -Path $stdout -Raw
        $errText = Get-Content -Path $stderr -Raw
        if (-not [string]::IsNullOrWhiteSpace($outText)) {
            Write-Host $outText.TrimEnd()
        }
        if (-not [string]::IsNullOrWhiteSpace($errText)) {
            Write-Host $errText.TrimEnd() -ForegroundColor DarkYellow
        }
        if (-not $AllowFailure -and -not ($AllowedExitCodes -contains $code)) {
            Fail "command failed with exit $code (expected $($AllowedExitCodes -join ', ')): $($CommandArgs -join ' ')"
        }
        return [pscustomobject]@{
            Code      = $code
            Stdout    = $outText
            Stderr    = $errText
            TimedOut  = -not $done
            StdoutPath = $stdout.FullName
            StderrPath = $stderr.FullName
        }
    } finally {
        Remove-Item -LiteralPath $stdout -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stderr -ErrorAction SilentlyContinue
    }
}

function Assert-Contains([string]$Text, [string]$Needle, [string]$Label) {
    if ($Text -notmatch [regex]::Escape($Needle)) {
        Fail "$Label did not contain expected text: $Needle"
    }
}

function Start-EnsembleUp {
    param(
        [string[]]$EnsembleArgs,
        [int]$WarmupSeconds = 6,
        [string]$Log = ""
    )

    if (-not $Log) {
        $Log = Join-Path $env:TEMP "ensemble-up-check.log"
    }
    $Err = "$Log.err"
    $exe = Get-EnsembleCmd
    if (-not $exe) {
        return [pscustomobject]@{
            Started  = $false
            Proc     = $null
            ExitCode = -1
            Error    = "ensemble executable not found"
            Out      = ""
        }
    }
    $proc = Start-Process -FilePath $exe -ArgumentList $EnsembleArgs `
        -NoNewWindow -PassThru -RedirectStandardOutput $Log -RedirectStandardError $Err
    Start-Sleep -Seconds $WarmupSeconds
    if (-not $proc -or $proc.HasExited) {
        $exitCode = if ($proc) { $proc.ExitCode } else { $null }
        $outText = Get-Content -Path $Log -Raw -ErrorAction SilentlyContinue
        $errText = Get-Content -Path $Err -Raw -ErrorAction SilentlyContinue
        if (-not $outText) { $outText = "" }
        if (-not $errText) { $errText = "" }
        if ($outText) { Write-Host $outText.TrimEnd() }
        if ($errText) { Write-Host $errText.TrimEnd() -ForegroundColor DarkYellow }
        return [pscustomobject]@{
            Started  = $false
            Proc     = $null
            ExitCode = $exitCode
            Error    = ($errText.Trim())
            Out      = ($outText.Trim())
        }
    }

    return [pscustomobject]@{
        Started  = $true
        Proc     = $proc
        ExitCode = $null
        Error    = $null
        Out      = $null
    }
}

function Assert-NotEmpty([string]$Text, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Text)) {
        Fail "$Label output was empty"
    }
}

function Parse-JsonOrFail([string]$Text, [string]$Label) {
    try {
        return $Text | ConvertFrom-Json
    } catch {
        Fail "$Label output is not JSON. $($_.Exception.Message)`n$Text"
    }
}

function Stream-Cursor([string]$RepoPath, [string]$Name) {
    $streamPath = Join-Path $RepoPath ".ensemble/stream/$Name.ndjson"
    if (-not (Test-Path -LiteralPath $streamPath -PathType Leaf)) {
        return 0
    }
    $lines = @(Get-Content -LiteralPath $streamPath)
    return $lines.Count
}

function Run-SliceA {
    Step "Slice A: control plane local + routing checks" {
        Require-Tool git
        Push-Location $Repo
        try {
        $status = Invoke-EnsembleCapture @(
                "team", "status",
                "--repo", ".",
                "--team", $Team,
                "--json"
            ) "team status --team $Team --json" -TimeoutSec 20

            $teamStatus = Parse-JsonOrFail $status.Stdout "team status"
            if ($teamStatus.team -ne $Team) {
                Fail "team status reported '$($teamStatus.team)' not '$Team'"
            }

            $nodesOut = Invoke-EnsembleCapture @("nodes") "nodes" -TimeoutSec 20
            Assert-NotEmpty $nodesOut.Stdout "nodes"

            $member = if ($RemoteNode) { "codex@$RemoteNode" } else { "codex@local" }
            $watchArgs = @("watch", $member, "--repo", ".", "--team", $Team, "--since", "0")
            if ($RemoteNode) {
                $watchArgs += @("--node", $RemoteNode)
            } else {
                $watchArgs += @("--node", "local")
            }
            $watchResult = Invoke-EnsembleCapture $watchArgs "watch baseline" -TimeoutSec 20
            if ($watchResult.Code -ne 0) {
                Fail "watch baseline failed (code $($watchResult.Code))"
            }

            $autoNodeErr = Invoke-EnsembleCapture @(
                "watch",
                "codex@auto",
                "--repo", ".",
                "--team", $Team,
                "--since", "0",
                "--node", "auto"
            ) "watch explicit --node auto error path" -TimeoutSec 20 -AllowedExitCodes @(1, 2, 3) -AllowFailure
            if ($autoNodeErr.Code -eq 0) {
                Fail "watch with --node auto should fail explicitly"
            }
            $autoNodeText = "$($autoNodeErr.Stdout)`n$($autoNodeErr.Stderr)"
            Assert-Contains $autoNodeText "auto is not supported" "invalid --node"
            Write-Host "Slice A checks passed." -ForegroundColor Green
        } finally {
            Pop-Location
        }
    }
}

function Run-SliceB {
    Step "Slice B: governed run + watch + steer/abort" {
        $crewPath = if ([System.IO.Path]::IsPathRooted($Crew)) { $Crew } else { Join-Path $Repo $Crew }
        if (-not (Test-Path -LiteralPath $crewPath)) {
            # Backward compatibility: default to examples/crew.toml if previous location missing.
            if ($Crew -eq "crew-main.toml") {
                $Crew = "examples\\crew.toml"
                $crewPath = if ([System.IO.Path]::IsPathRooted($Crew)) { $Crew } else { Join-Path $Repo $Crew }
            }
            if (-not (Test-Path -LiteralPath $crewPath)) {
                Fail "crew file not found: $crewPath"
            }
        }

        $runTask = $Task
        if ([string]::IsNullOrWhiteSpace($runTask)) {
            $ts = [DateTime]::UtcNow.ToString("yyyyMMddHHmmss")
            $runTask = "phase2-verify-$ts"
        }

        $preTeamInbox = Invoke-EnsembleCapture @(
            "team", "inbox",
            "--repo", $Repo,
            "--team", $Team,
            "--since", "0",
            "--json"
        ) "team inbox cursor before run" -TimeoutSec 20
        $preInboxJson = Parse-JsonOrFail $preTeamInbox.Stdout "team inbox before run"
        $preInboxCursor = [int]$preInboxJson.next

        $preWatchCursor = Stream-Cursor $Repo $Watch
        Write-Host "watch cursor before run: $preWatchCursor" -ForegroundColor DarkGray

        $runArgs = @(
            "run", $runTask,
            "--crew", $Crew,
            "--repo", $Repo,
            "--team", $Team,
            "--watch", $Watch
        )
            $runResult = Invoke-EnsembleCapture $runArgs "ensemble run (LANDED/ESCALATED)" `
                -TimeoutSec $RunTimeoutSecs -AllowedExitCodes @(0, 1)

        $runText = "$($runResult.Stdout)`n$($runResult.Stderr)"
        if ($runText -notmatch "LANDED|ESCALATED") {
            Fail "ensemble run output did not show LANDED/ESCALATED terminal state"
        }
        $expectedDecision = if ($runResult.Code -eq 0 -and $runText -match "LANDED") {
            "LANDED"
        } elseif ($runResult.Code -ne 0 -and $runText -match "ESCALATED") {
            "ESCALATED"
        } else {
            Fail "ensemble run exit/output mismatch; exit=$($runResult.Code), output did not clearly identify the terminal outcome"
        }

        $postTeamInbox = Invoke-EnsembleCapture @(
            "team", "inbox",
            "--repo", $Repo,
            "--team", $Team,
            "--since", "$preInboxCursor",
            "--json"
        ) "team inbox events from run" -TimeoutSec 20
        $postInboxJson = Parse-JsonOrFail $postTeamInbox.Stdout "team inbox after run"
        $runDecisionMessages = @(
            $postInboxJson.messages | Where-Object {
                $_.from -eq "conductor" -and
                $_.kind -eq "decision"
            }
        )
        if ($runDecisionMessages.Count -lt 1) {
            Fail "run --team $Team did not write a conductor decision event to the team board"
        }
        $lastDecision = $runDecisionMessages[$runDecisionMessages.Count - 1]
        if ($expectedDecision -eq "LANDED") {
            if ($lastDecision.body -ne "LANDED") {
                Fail "last team-board conductor decision was '$($lastDecision.body)', expected LANDED"
            }
        } else {
            if ($lastDecision.body -notmatch "^escalated:") {
                Fail "last team-board conductor decision was '$($lastDecision.body)', expected escalated"
            }
        }

        $watchResult = Invoke-EnsembleCapture @(
            "watch", $Watch,
            "--repo", $Repo,
            "--team", $Team,
            "--since", "$preWatchCursor"
        ) "watch run trace" -TimeoutSec 20
        if ($watchResult.Code -ne 0) {
            Fail "watch run trace failed"
        }
        if ($watchResult.Stdout -notmatch "\[conductor\s*.\s*decision\]") {
            Fail "watch output from cursor $preWatchCursor did not include a conductor decision from this run"
        }

        $steerResult = Invoke-EnsembleCapture @(
            "steer", $Watch,
            "keep result minimal and only write RESULT.txt for the operator",
            "--repo", $Repo,
            "--team", $Team
        ) "steer command" -TimeoutSec 20
        if ($steerResult.Code -ne 0) {
            Fail "steer command failed"
        }

        $abortResult = Invoke-EnsembleCapture @(
            "abort", $Watch,
            "--repo", $Repo,
            "--team", $Team
        ) "abort command" -TimeoutSec 20
        if ($abortResult.Code -ne 0) {
            Fail "abort command failed"
        }

        $controlPath = Join-Path $Repo ".ensemble/control/$Watch.ndjson"
        if (-not (Test-Path -LiteralPath $controlPath)) {
            Fail "control feed missing after steer/abort: $controlPath"
        }
        $control = Get-Content -LiteralPath $controlPath -Raw
        Assert-Contains $control '"cmd":"steer"' "control feed"
        Assert-Contains $control '"cmd":"abort"' "control feed"
        Write-Host "Slice B checks passed." -ForegroundColor Green
    }
}

function Run-SliceC {
    Step "Slice C: fleet visibility check" {
        $mesh = Invoke-EnsembleCapture @("mesh") "mesh" -TimeoutSec 20
        if ($mesh.Code -ne 0) {
            Fail "mesh failed"
        }
        $nodes = Invoke-EnsembleCapture @("nodes") "nodes" -TimeoutSec 20
        if ($nodes.Code -ne 0) {
            Fail "nodes failed"
        }

        $manifestInfo = Invoke-FleetManifestPlan $FleetManifest $FleetNode
        $nodesToCheck = @(Expand-NodeList $ExpectedFleetNodes)
        $localNodeForCheck = $LocalFleetNode
        if ($CheckFleetManifestNodes) {
            if ($null -eq $manifestInfo) {
                Fail "-CheckFleetManifestNodes requires -FleetManifest <path>"
            }
            if ($nodesToCheck.Count -gt 0) {
                Write-Host "-CheckFleetManifestNodes is set; using fleet manifest nodes instead of -ExpectedFleetNodes." -ForegroundColor Yellow
            }
            $nodesToCheck = @(Expand-NodeList $manifestInfo.Nodes)
            if ([string]::IsNullOrWhiteSpace($localNodeForCheck)) {
                $localNodeForCheck = if (-not [string]::IsNullOrWhiteSpace($FleetNode)) {
                    $FleetNode
                } else {
                    $manifestInfo.Conductor
                }
            }
        }

        if ($nodesToCheck.Count -gt 0) {
            $hosts = @(Get-UrlHosts $mesh.Stdout)
            $expectedNodes = @($nodesToCheck | Where-Object {
                    -not ($localNodeForCheck -and ([string]$_).Equals($localNodeForCheck, [System.StringComparison]::OrdinalIgnoreCase))
                })
            $missing = New-Object System.Collections.Generic.List[string]
            foreach ($expectedHost in $expectedNodes) {
                if (-not (Test-HostPresent $expectedHost $hosts)) {
                    $missing.Add($expectedHost)
                }
            }
            if ($missing.Count -gt 0) {
                Fail "expected remote fleet node(s) not found in mesh output: $($missing -join ', ')"
            }
            if ($localNodeForCheck) {
                Write-Host "Slice C skipped local fleet node '$localNodeForCheck' because mesh/nodes only list tailnet peers." -ForegroundColor DarkGray
            }
        }
        Write-Host "Slice C note: full 5-node restart/run loop still needs per-host terminal execution." -ForegroundColor Yellow
        Write-Host "  m1~m5: run 'ensemble up'; confirm m1 local CLIs and remote peers via mesh, with nodes as an agent-route helper." -ForegroundColor Yellow
        Write-Host "Slice C local checks passed (mesh/nodes runnable)." -ForegroundColor Green
    }
}

function Run-SliceD {
    Step "Slice D: clean reinstall + re-attach smoke path" {
        $scriptDir = $PSScriptRoot
        $installScript = Join-Path $scriptDir "install.ps1"
        $uninstallScript = Join-Path $scriptDir "uninstall.ps1"
        $smokeScript = Join-Path $scriptDir "smoke.ps1"
        if (-not (Test-Path -LiteralPath $installScript)) {
            Fail "install script missing: $installScript"
        }
        if (-not (Test-Path -LiteralPath $uninstallScript)) {
            Fail "uninstall script missing: $uninstallScript"
        }

        Write-Host "-> uninstall clean baseline" -ForegroundColor DarkGray
        $baselineExe = ""
        if ($InstallDir) {
            $candidate = Join-Path $InstallDir "ensemble.exe"
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $baselineExe = $candidate
            }
        } else {
            $local = [Environment]::GetFolderPath("LocalApplicationData")
            if ($local) {
                $candidate = Join-Path $local "ensemble\bin\ensemble.exe"
                if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                    $baselineExe = $candidate
                }
            }
            if (-not $baselineExe) {
                $cmd = Get-Command ensemble -ErrorAction SilentlyContinue
                if ($cmd) {
                    $baselineExe = $cmd.Source
                }
            }
        }
        if ($baselineExe) {
            pwsh -NoProfile -File $uninstallScript -RemoveMcpConfig -EnsembleExe $baselineExe -Repo $Repo -Clients codex,claude,opencode
        } else {
            Write-Host "skip baseline MCP cleanup (ensemble.exe unavailable for this host)." -ForegroundColor Yellow
            pwsh -NoProfile -File $uninstallScript
        }

        Write-Host "-> install" -ForegroundColor DarkGray
        pwsh -NoProfile -File $installScript

        $ensembleAfterInstall = Get-EnsembleCmd
        if (-not $ensembleAfterInstall) {
            Fail "ensemble command not found after install"
        }

        Write-Host "-> service install/uninstall dry-run" -ForegroundColor DarkGray
        Invoke-EnsembleCapture @("serve", "--install-service", "--print") "serve service install plan" -TimeoutSec 20 | Out-Null
        Invoke-EnsembleCapture @("serve", "--uninstall-service", "--print") "serve service uninstall plan" -TimeoutSec 20 | Out-Null

        Write-Host "-> smoke preflight using installed command" -ForegroundColor DarkGray
        if (-not $SmokeRoot) {
            if (Test-Path "D:\tmp") {
                $SmokeRoot = Join-Path "D:\tmp" "ensemble-phase2-reinstall-smoke"
            } else {
                $SmokeRoot = Join-Path $env:TEMP "ensemble-phase2-reinstall-smoke"
            }
        }
        New-Item -ItemType Directory -Path $SmokeRoot -Force | Out-Null
        if (-not $TargetDir) {
            if (Test-Path "D:\tmp") {
                $TargetDir = Join-Path "D:\tmp" "ensemble-phase2-reinstall-target"
            } else {
                $TargetDir = Join-Path $env:TEMP "ensemble-phase2-reinstall-target"
            }
        }
        if (-not $SkipCleanSmoke) {
            $smokeExe = Join-Path $TargetDir "release/ensemble.exe"
            if (Test-Path -LiteralPath $smokeExe) {
                pwsh -NoProfile -File $smokeScript `
                    -NoBuild -Repo $Repo -SmokeRoot $SmokeRoot -TargetDir $TargetDir `
                    -Reviewers "claude" -AllowEscalatedRun -TimeoutSecs $SmokeTimeoutSecs
            } else {
                Write-Host "release binary missing under $TargetDir; running smoke without -NoBuild to build once." -ForegroundColor Yellow
                pwsh -NoProfile -File $smokeScript `
                    -Repo $Repo -SmokeRoot $SmokeRoot -TargetDir $TargetDir `
                    -Reviewers "claude" -AllowEscalatedRun -TimeoutSecs $SmokeTimeoutSecs
            }
        } else {
            Write-Host "SkipCleanSmoke enabled, skipping smoke command."
        }

        Write-Host "-> up reachability check" -ForegroundColor DarkGray
        $upLog = Join-Path $SmokeRoot "ensemble-up.log"

        $upArgs = @("up")
        if ($UpBind) {
            $upArgs += @("--bind", $UpBind)
        }

        $upAttempt = Start-EnsembleUp -EnsembleArgs $upArgs -WarmupSeconds $UpWarmupSecs -Log $upLog
        if (-not $upAttempt.Started) {
        $upFailureText = "$($upAttempt.Error)`n$($upAttempt.Out)"
        if ($upAttempt.ExitCode -eq 1 -and $upFailureText -match "一次只能用一個通訊端位址|address already in use|10048") {
                if (-not $UpBind) {
                    Write-Host "default bind in use; retry up with loopback ephemeral port" -ForegroundColor Yellow
                    $upArgs = @("up", "--bind", "127.0.0.1:0")
                    $upAttempt = Start-EnsembleUp -EnsembleArgs $upArgs -WarmupSeconds $UpWarmupSecs -Log ($upLog + ".alt")
                }
            }
        }

        if (-not $upAttempt.Started) {
            if ($UpBind) {
                Fail "ensemble up exited before warmup window (bind=$UpBind)"
            }
            Fail "ensemble up exited before warmup window"
        }
        $upProc = $upAttempt.Proc

        Stop-Process -Id $upProc.Id -Force -ErrorAction SilentlyContinue
        $upProc.WaitForExit(3000) | Out-Null

        Invoke-EnsembleCapture @("mesh") "mesh (post-reinstall)" -TimeoutSec 20 | Out-Null
        Invoke-EnsembleCapture @("nodes") "nodes (post-reinstall)" -TimeoutSec 20 | Out-Null

        Write-Host "-> uninstall again" -ForegroundColor DarkGray
        pwsh -NoProfile -File $uninstallScript
        Write-Host "Slice D checks passed." -ForegroundColor Green
    }
}

if (-not (Test-Path -LiteralPath $Repo)) {
    Fail "repo path not found: $Repo"
}
if (-not (Test-Path -LiteralPath (Join-Path $Repo ".") )) {
    Fail "repo is not a path: $Repo"
}

$resolvedEnsemble = Get-EnsembleCmd
if (-not $resolvedEnsemble) {
    Write-Host "warning: ensemble not yet on PATH; this script can still proceed with slice D using install scripts." -ForegroundColor Yellow
}

if (-not $SkipSliceA) { Run-SliceA }
if (-not $SkipSliceB) { Run-SliceB }
if (-not $SkipSliceC) { Run-SliceC }
if (-not $SkipSliceD) { Run-SliceD }

Write-Host ""
Write-Host "Phase 2 verify script finished" -ForegroundColor Green
