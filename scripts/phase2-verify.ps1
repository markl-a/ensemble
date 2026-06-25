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
    [switch]$RequireExplicitRemoteAgents,
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
    [switch]$SliceBPreflightOnly,
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

function Get-FreeLoopbackPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try {
        return ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    } finally {
        $listener.Stop()
    }
}

function Wait-HttpHealth([string]$BaseUrl, [int]$TimeoutSec = 10) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $healthUrl = "$BaseUrl/health"
    while ((Get-Date) -lt $deadline) {
        try {
            $resp = Invoke-WebRequest -Uri $healthUrl -UseBasicParsing -TimeoutSec 2 -ErrorAction Stop
            if ($resp.StatusCode -eq 200 -and ([string]$resp.Content) -match '"ok"\s*:\s*true') {
                return
            }
        } catch { }
        Start-Sleep -Milliseconds 200
    }
    Fail "loopback remote control serve did not become healthy at $healthUrl"
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

function Get-JsonProp($Object, [string]$Name, $Default = $null) {
    if ($null -eq $Object) {
        return $Default
    }
    $prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $prop) {
        return $Default
    }
    return $prop.Value
}

function Get-JsonArray($Value) {
    if ($null -eq $Value) {
        return @()
    }
    return @($Value)
}

function Get-JsonArgs($Command) {
    return @(Get-JsonArray (Get-JsonProp $Command "args" @()) | ForEach-Object { [string]$_ })
}

function Get-JsonFlagValue($Command, [string]$Flag) {
    $argList = @(Get-JsonArgs $Command)
    for ($i = 0; $i -lt ($argList.Count - 1); $i++) {
        if ($argList[$i] -eq $Flag) {
            return $argList[$i + 1]
        }
    }
    return $null
}

function Test-JsonCommandStartsWith($Command, [string]$Verb) {
    $argList = @(Get-JsonArgs $Command)
    return $argList.Count -gt 0 -and $argList[0] -eq $Verb
}

function Test-JsonCommandHasArg($Command, [string]$Needle) {
    return @(Get-JsonArgs $Command) -contains $Needle
}

function Test-JsonCommandArgsEqual($Command, [string[]]$Expected) {
    $argList = @(Get-JsonArgs $Command)
    if ($argList.Count -ne $Expected.Count) {
        return $false
    }
    for ($i = 0; $i -lt $Expected.Count; $i++) {
        if ($argList[$i] -ne $Expected[$i]) {
            return $false
        }
    }
    return $true
}

function Assert-Phase2FleetGeneratedPlanShape($Plan) {
    $nodes = @(Get-JsonArray (Get-JsonProp $Plan "nodes" @()) | ForEach-Object { [string]$_ })
    if ($nodes.Count -ne 5) {
        Fail "Phase 2 Slice C generated fleet plan must contain exactly 5 nodes; got $($nodes.Count)"
    }

    $projects = @(Get-JsonArray (Get-JsonProp $Plan "projects" @()))
    $mainProjects = @($projects | Where-Object { ([string](Get-JsonProp $_ "kind" "")) -eq "main" })
    $satelliteProjects = @($projects | Where-Object { ([string](Get-JsonProp $_ "kind" "")) -eq "satellite" })
    if ($mainProjects.Count -ne 1) {
        Fail "Phase 2 Slice C generated fleet plan must contain exactly 1 main project; got $($mainProjects.Count)"
    }
    if ($satelliteProjects.Count -ne 4) {
        Fail "Phase 2 Slice C generated fleet plan must contain exactly 4 satellite projects; got $($satelliteProjects.Count)"
    }

    $commands = @(Get-JsonArray (Get-JsonProp $Plan "commands" @()))
    $serviceCommands = @($commands | Where-Object { ([string](Get-JsonProp $_ "kind" "")) -eq "service" })
    $runCommands = @($commands | Where-Object { ([string](Get-JsonProp $_ "kind" "")) -eq "run" })
    $watchCommands = @($commands | Where-Object { ([string](Get-JsonProp $_ "kind" "")) -eq "watch" })
    if ($serviceCommands.Count -ne 5) {
        Fail "Phase 2 Slice C generated fleet plan must contain one service command per node; got $($serviceCommands.Count)"
    }
    foreach ($fleetNode in $nodes) {
        $nodeServices = @($serviceCommands | Where-Object {
                ([string](Get-JsonProp $_ "node" "")).Equals($fleetNode, [System.StringComparison]::OrdinalIgnoreCase)
            })
        if ($nodeServices.Count -ne 1 -or -not (Test-JsonCommandArgsEqual $nodeServices[0] @("up"))) {
            Fail "Phase 2 Slice C generated fleet plan must contain exactly one 'ensemble up' service command for node '$fleetNode'; got $($nodeServices.Count)"
        }
    }
    if ($runCommands.Count -ne 5) {
        Fail "Phase 2 Slice C generated fleet plan must contain one run command per project; got $($runCommands.Count)"
    }
    if ($watchCommands.Count -ne 5) {
        Fail "Phase 2 Slice C generated fleet plan must contain one watch command per project; got $($watchCommands.Count)"
    }

    foreach ($project in $projects) {
        $name = [string](Get-JsonProp $project "name" "")
        $node = [string](Get-JsonProp $project "node" "")
        $repo = [string](Get-JsonProp $project "repo" "")
        $crew = [string](Get-JsonProp $project "crew" "")
        $team = [string](Get-JsonProp $project "team" "")
        $watch = [string](Get-JsonProp $project "watch" "")
        $runMatch = @($runCommands | Where-Object {
                $cmdNode = [string](Get-JsonProp $_ "node" "")
                $cmdNode.Equals($node, [System.StringComparison]::OrdinalIgnoreCase) -and
                (Test-JsonCommandStartsWith $_ "run") -and
                (Get-JsonFlagValue $_ "--crew") -eq $crew -and
                (Get-JsonFlagValue $_ "--repo") -eq $repo -and
                (Get-JsonFlagValue $_ "--team") -eq $team -and
                (Get-JsonFlagValue $_ "--watch") -eq $watch
            })
        if ($runMatch.Count -ne 1) {
            Fail "Phase 2 Slice C generated fleet plan must contain exactly one run command for project '$name'; got $($runMatch.Count)"
        }
        $watchMatch = @($watchCommands | Where-Object {
                $cmdNode = [string](Get-JsonProp $_ "node" "")
                $argList = @(Get-JsonArgs $_)
                $cmdNode.Equals($node, [System.StringComparison]::OrdinalIgnoreCase) -and
                $argList.Count -ge 2 -and
                $argList[0] -eq "watch" -and
                $argList[1] -eq $watch -and
                (Get-JsonFlagValue $_ "--repo") -eq $repo -and
                (Get-JsonFlagValue $_ "--team") -eq $team -and
                (Test-JsonCommandHasArg $_ "--follow")
            })
        if ($watchMatch.Count -ne 1) {
            Fail "Phase 2 Slice C generated fleet plan must contain exactly one watch command for project '$name'; got $($watchMatch.Count)"
        }
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
    Write-Host "== fleet manifest generated plan shape (all nodes) ==" -ForegroundColor DarkGray
    $jsonOut = pwsh -NoProfile -File $fleetScript -Manifest $info.Path -Node all -PlanOnly -Json 2>&1
    $jsonCode = $LASTEXITCODE
    $jsonText = ($jsonOut | Out-String).Trim()
    if ($jsonCode -ne 0) {
        if (-not [string]::IsNullOrWhiteSpace($jsonText)) {
            Write-Host $jsonText
        }
        Fail "phase2-fleet JSON plan failed for manifest $($info.Path)"
    }
    $jsonPlan = Parse-JsonOrFail $jsonText "phase2-fleet JSON plan"
    Assert-Phase2FleetGeneratedPlanShape $jsonPlan
    Write-Host "fleet generated plan shape passed: 5 nodes, 1 main run, 4 satellite runs." -ForegroundColor DarkGray
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

function Assert-Phase2RunCrewGovernance([string]$CrewPath) {
    $inspect = Invoke-EnsembleCapture @(
        "crew", "inspect",
        "--crew", $CrewPath,
        "--json"
    ) "crew governance inspect" -TimeoutSec 20
    $json = Parse-JsonOrFail $inspect.Stdout "crew governance inspect"

    if ([int]$json.min_approvals -lt 2) {
        Fail "Phase 2 Slice B requires gate.min_approvals >= 2; got $($json.min_approvals)"
    }
    if ([string]::IsNullOrWhiteSpace([string]$json.test_command)) {
        Fail "Phase 2 Slice B requires a non-empty [test] command; the full run must still prove it passes"
    }
    $reviewerAgents = @($json.reviewer_agents | ForEach-Object { [string]$_ })
    if ($reviewerAgents.Count -lt 2) {
        Fail "Phase 2 Slice B requires at least two reviewer roles; got $($reviewerAgents.Count)"
    }
    if ([int]$json.distinct_reviewer_agents -lt 2) {
        Fail "Phase 2 Slice B requires at least two different reviewer vendors; got $($json.distinct_reviewer_agents)"
    }
    if ([int]$json.distinct_reviewer_agents -lt [int]$json.min_approvals) {
        Fail "Phase 2 Slice B requires enough distinct reviewer vendors to satisfy min_approvals=$($json.min_approvals); got $($json.distinct_reviewer_agents)"
    }
    if ($RequireExplicitRemoteAgents) {
        $remoteAgents = @($json.explicit_remote_agents | ForEach-Object { [string]$_ })
        if ($remoteAgents.Count -lt 1) {
            Fail "Phase 2 Slice B was asked to require remote routing, but no active pipeline role has an explicit [agents.<name>].node entry"
        }
        Write-Host "explicit remote agents: $($remoteAgents -join ', ')" -ForegroundColor DarkGray
    }
    Write-Host "Phase 2 crew governance passed: min_approvals=$($json.min_approvals), reviewers=$($reviewerAgents -join ', '), test_command=$($json.test_command)" -ForegroundColor DarkGray
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

            $localNodeStatus = Invoke-EnsembleCapture @(
                "team", "status",
                "--repo", ".",
                "--team", $Team,
                "--node", "local",
                "--json"
            ) "team status --node local" -TimeoutSec 20
            $localNodeJson = Parse-JsonOrFail $localNodeStatus.Stdout "team status --node local"
            if ($localNodeJson.team -ne $Team) {
                Fail "team status --node local reported '$($localNodeJson.team)' not '$Team'"
            }

            $teamAutoErr = Invoke-EnsembleCapture @(
                "team", "status",
                "--repo", ".",
                "--team", $Team,
                "--node", "auto",
                "--json"
            ) "team explicit --node auto error path" -TimeoutSec 20 -AllowedExitCodes @(1, 2, 3) -AllowFailure
            if ($teamAutoErr.Code -eq 0) {
                Fail "team status with --node auto should fail explicitly"
            }
            $teamAutoText = "$($teamAutoErr.Stdout)`n$($teamAutoErr.Stderr)"
            Assert-Contains $teamAutoText "auto is not supported" "team invalid --node"

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

            $repoRoot = (Get-Location).Path
            $port = Get-FreeLoopbackPort
            $bind = "127.0.0.1:$port"
            $nodeUrl = "http://$bind"
            $remoteToken = "phase2-loopback-token-$PID"
            $remoteTeam = "phase2-loopback-$PID"
            $remoteStream = "phase2-loopback-stream-$PID"
            $remoteControl = "phase2-loopback-control-$PID"
            $remoteTeamRoot = Join-Path $repoRoot ".ensemble/teams/$remoteTeam"
            $serveLog = Join-Path $env:TEMP "ensemble-phase2-control-serve-$PID.log"
            $streamFile = $null
            $controlPath = $null
            $serveAttempt = Start-EnsembleUp -EnsembleArgs @(
                "serve", "--bind", $bind, "--token", $remoteToken
            ) -WarmupSeconds 2 -Log $serveLog
            if (-not $serveAttempt.Started) {
                foreach ($artifact in @($serveLog, "$serveLog.err")) {
                    if (Test-Path -LiteralPath $artifact) {
                        Remove-Item -LiteralPath $artifact -Force -ErrorAction SilentlyContinue
                    }
                }
                Fail "loopback remote control serve exited before warmup window"
            }
            $serveProc = $serveAttempt.Proc
            try {
                Wait-HttpHealth $nodeUrl 10

                $remoteStatus = Invoke-EnsembleCapture @(
                    "team", "status",
                    "--repo", ".",
                    "--team", $remoteTeam,
                    "--node", $nodeUrl,
                    "--json"
                ) "remote team status over loopback" -TimeoutSec 20
                $remoteStatusJson = Parse-JsonOrFail $remoteStatus.Stdout "remote team status"
                if ($remoteStatusJson.team -ne $remoteTeam) {
                    Fail "remote team status reported '$($remoteStatusJson.team)' not '$remoteTeam'"
                }

                $badToken = Invoke-EnsembleCapture @(
                    "team", "say", "remote mutation with wrong token",
                    "--repo", ".",
                    "--team", $remoteTeam,
                    "--node", $nodeUrl,
                    "--token", "wrong-token"
                ) "remote team say wrong token" -TimeoutSec 20 -AllowedExitCodes @(1, 2, 3) -AllowFailure
                if ($badToken.Code -eq 0) {
                    Fail "remote team say with a wrong token should fail explicitly"
                }
                $badTokenText = "$($badToken.Stdout)`n$($badToken.Stderr)"
                Assert-Contains $badTokenText "Unauthorized" "remote wrong-token failure"

                $remoteSay = Invoke-EnsembleCapture @(
                    "team", "say", "remote loopback hello",
                    "--repo", ".",
                    "--team", $remoteTeam,
                    "--node", $nodeUrl,
                    "--token", $remoteToken
                ) "remote team say with token" -TimeoutSec 20
                Assert-Contains $remoteSay.Stdout "next=1" "remote authorized team say"

                $remoteInbox = Invoke-EnsembleCapture @(
                    "team", "inbox",
                    "--repo", ".",
                    "--team", $remoteTeam,
                    "--node", $nodeUrl,
                    "--json"
                ) "remote team inbox over loopback" -TimeoutSec 20
                $remoteInboxJson = Parse-JsonOrFail $remoteInbox.Stdout "remote team inbox"
                $remoteBodies = @($remoteInboxJson.messages | ForEach-Object { [string]$_.body })
                if ($remoteBodies -contains "remote mutation with wrong token") {
                    Fail "remote wrong-token team say unexpectedly appeared in inbox"
                }
                if ($remoteBodies -notcontains "remote loopback hello") {
                    Fail "remote team inbox did not include the token-authorized loopback message"
                }

                $streamDir = Join-Path $repoRoot ".ensemble/stream"
                New-Item -ItemType Directory -Force -Path $streamDir | Out-Null
                $streamFile = Join-Path $streamDir "$remoteStream.ndjson"
                '{"from":"codex@loopback","kind":"result","body":"remote loopback stream smoke"}' |
                    Add-Content -LiteralPath $streamFile -Encoding utf8NoBOM
                $remoteWatch = Invoke-EnsembleCapture @(
                    "watch", $remoteStream,
                    "--repo", ".",
                    "--team", $remoteTeam,
                    "--node", $nodeUrl,
                    "--since", "0",
                    "--json"
                ) "remote watch over loopback" -TimeoutSec 20
                Assert-Contains $remoteWatch.Stdout "remote loopback stream smoke" "remote watch output"

                $controlPath = Join-Path $repoRoot ".ensemble/control/$remoteControl.ndjson"
                $badSteer = Invoke-EnsembleCapture @(
                    "steer", $remoteControl, "remote wrong-token steer",
                    "--repo", ".",
                    "--node", $nodeUrl,
                    "--token", "wrong-token"
                ) "remote steer wrong token" -TimeoutSec 20 -AllowedExitCodes @(1, 2, 3) -AllowFailure
                if ($badSteer.Code -eq 0) {
                    Fail "remote steer with a wrong token should fail explicitly"
                }
                $badSteerText = "$($badSteer.Stdout)`n$($badSteer.Stderr)"
                Assert-Contains $badSteerText "Unauthorized" "remote wrong-token steer failure"
                $badSteerWritten = (Test-Path -LiteralPath $controlPath) -and `
                    ((Get-Content -LiteralPath $controlPath -Raw) -match [regex]::Escape("remote wrong-token steer"))
                if ($badSteerWritten) {
                    Fail "remote wrong-token steer unexpectedly appeared in control feed"
                }

                Invoke-EnsembleCapture @(
                    "steer", $remoteControl, "remote loopback steer",
                    "--repo", ".",
                    "--node", $nodeUrl,
                    "--token", $remoteToken
                ) "remote steer with token" -TimeoutSec 20 | Out-Null
                if (-not (Test-Path -LiteralPath $controlPath -PathType Leaf)) {
                    Fail "remote steer did not create expected control feed: $controlPath"
                }
                $controlText = Get-Content -LiteralPath $controlPath -Raw
                Assert-Contains $controlText '"cmd":"steer"' "remote steer control feed"
                Assert-Contains $controlText "remote loopback steer" "remote steer control feed"

                $badAbort = Invoke-EnsembleCapture @(
                    "abort", $remoteControl, "--hard",
                    "--repo", ".",
                    "--node", $nodeUrl,
                    "--token", "wrong-token"
                ) "remote abort wrong token" -TimeoutSec 20 -AllowedExitCodes @(1, 2, 3) -AllowFailure
                if ($badAbort.Code -eq 0) {
                    Fail "remote abort with a wrong token should fail explicitly"
                }
                $badAbortText = "$($badAbort.Stdout)`n$($badAbort.Stderr)"
                Assert-Contains $badAbortText "Unauthorized" "remote wrong-token abort failure"
                $controlText = Get-Content -LiteralPath $controlPath -Raw
                if ($controlText -match [regex]::Escape('"cmd":"abort"')) {
                    Fail "remote wrong-token abort unexpectedly appeared in control feed"
                }

                Invoke-EnsembleCapture @(
                    "abort", $remoteControl, "--hard",
                    "--repo", ".",
                    "--node", $nodeUrl,
                    "--token", $remoteToken
                ) "remote abort with token" -TimeoutSec 20 | Out-Null
                $abortLines = @(Get-Content -LiteralPath $controlPath | Where-Object { $_ -match [regex]::Escape('"cmd":"abort"') })
                if ($abortLines.Count -ne 1) {
                    Fail "remote abort control feed should contain exactly one abort record; found $($abortLines.Count)"
                }
                Assert-Contains $abortLines[0] '"hard":true' "remote abort control feed"
            } finally {
                if ($serveProc) {
                    Stop-Process -Id $serveProc.Id -Force -ErrorAction SilentlyContinue
                    $serveProc.WaitForExit(3000) | Out-Null
                }
                foreach ($artifact in @($streamFile, $controlPath, $serveLog, "$serveLog.err")) {
                    if ($artifact -and (Test-Path -LiteralPath $artifact)) {
                        Remove-Item -LiteralPath $artifact -Force -ErrorAction SilentlyContinue
                    }
                }
                if ($remoteTeamRoot -and (Test-Path -LiteralPath $remoteTeamRoot)) {
                    Remove-Item -LiteralPath $remoteTeamRoot -Recurse -Force -ErrorAction SilentlyContinue
                }
            }
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
            # Phase 2 requires a test-gated, two-reviewer crew. If the operator has not
            # materialized crew-main.toml yet, use the local Phase 2 sample instead of
            # the older Phase 1 example.
            if ($Crew -eq "crew-main.toml") {
                $Crew = "examples\\crew-phase2.toml"
                $crewPath = if ([System.IO.Path]::IsPathRooted($Crew)) { $Crew } else { Join-Path $Repo $Crew }
            }
            if (-not (Test-Path -LiteralPath $crewPath)) {
                Fail "crew file not found: $crewPath"
            }
        }

        Assert-Phase2RunCrewGovernance $crewPath
        if ($SliceBPreflightOnly) {
            Write-Host "SliceBPreflightOnly enabled; skipping governed run after crew governance check." -ForegroundColor Yellow
            Write-Host "Slice B preflight checks passed." -ForegroundColor Green
            return
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
