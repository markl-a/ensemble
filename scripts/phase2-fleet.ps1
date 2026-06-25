#!/usr/bin/env pwsh
# Materialize Phase-2 five-node fleet crew files from one local manifest.
#
# Per-host service bootstrap examples:
#   pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Service install-print -RunService
#   pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Service install -RunService
#   pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Service uninstall -RunService
#   pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node all -Service install -RunService -RemoteService  # via tailscale ssh
#   pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node all -Service install -RunService -RemoteService -RemoteServiceTransport ssh  # via OpenSSH
# Repeatable Slice C acceptance example:
#   pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Materialize -RunSelected -VerifyEvidence -RepeatCount 2

param(
    [string]$Manifest = "phase2-fleet.local.json",
    [string]$Node = "all",
    [string]$OutDir = ".ensemble/phase2-fleet",
    [switch]$InitSample,
    [switch]$Force,
    [switch]$Materialize,
    [switch]$CheckNodes,
    [switch]$RunSelected,
    [switch]$VerifyReports,
    [switch]$VerifyEvidence,
    [switch]$AllowEscalatedRun,
    [switch]$RequireControlEvidence,
    [switch]$RequireSteerEvidence,
    [switch]$RequireAbortEvidence,
    [int]$RepeatCount = 1,
    [switch]$PlanOnly,
    [switch]$Json,
    [string]$Service = "none",
    [switch]$RunService,
    [switch]$RemoteService,
    [ValidateSet("tailscale", "ssh")]
    [string]$RemoteServiceTransport = "tailscale",
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Host "FAIL: $Message" -ForegroundColor Red
    exit 1
}

function Get-Prop($Object, [string]$Name, $Default = $null) {
    if ($null -eq $Object) {
        return $Default
    }
    $prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $prop) {
        return $Default
    }
    return $prop.Value
}

function Require-Prop($Object, [string]$Name, [string]$Path) {
    $value = Get-Prop $Object $Name $null
    if ($null -eq $value -or ([string]$value).Trim().Length -eq 0) {
        Fail "manifest is missing required field: $Path.$Name"
    }
    return [string]$value
}

function Toml-String([string]$Value) {
    $escaped = $Value.Replace('\', '\\').Replace('"', '\"')
    $escaped = $escaped.Replace("`r", '\r').Replace("`n", '\n').Replace("`t", '\t')
    return '"' + $escaped + '"'
}

function Quote-Arg([string]$Value) {
    if ([string]::IsNullOrEmpty($Value)) {
        return "''"
    }
    return "'" + $Value.Replace("'", "''") + "'"
}

function Format-Arg([string]$Value) {
    if ([string]::IsNullOrEmpty($Value)) {
        return "''"
    }
    if ($Value -match '^[A-Za-z0-9_./:@\\=-]+$') {
        return $Value
    }
    return Quote-Arg $Value
}

function Format-Command([string[]]$ArgList) {
    $parts = @("ensemble")
    foreach ($arg in $ArgList) {
        $parts += Format-Arg $arg
    }
    return ($parts -join " ")
}

function New-CommandSpec([string]$NodeName, [string]$Kind, [string[]]$ArgList) {
    return [pscustomobject]@{
        Node = $NodeName
        Kind = $Kind
        Args = @($ArgList)
        Text = Format-Command $ArgList
    }
}

function Count-NonEmptyFileLines([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return 0
    }
    return @((Get-Content -LiteralPath $Path) | Where-Object { ([string]$_).Trim().Length -gt 0 }).Count
}

function Parse-JsonOrFail([string]$Title, [string]$Text) {
    try {
        return $Text | ConvertFrom-Json
    } catch {
        Fail "$Title did not return valid JSON: $($_.Exception.Message)`n$Text"
    }
}

function Get-RunTerminal([string]$Text) {
    $landed = [regex]::Matches($Text, '(?m)^\s*LANDED after \d+ round\(s\)')
    $escalated = [regex]::Matches($Text, '(?m)^\s*ESCALATED after \d+ round\(s\):')
    if ($landed.Count -gt 0 -and $escalated.Count -gt 0) {
        Fail "selected run produced ambiguous LANDED and ESCALATED terminal states"
    }
    if ($landed.Count -eq 1) {
        return "landed"
    }
    if ($escalated.Count -eq 1) {
        return "escalated"
    }
    if ($landed.Count -gt 1 -or $escalated.Count -gt 1) {
        Fail "selected run produced multiple terminal state lines"
    }
    Fail "selected run did not produce a terminal LANDED or ESCALATED state"
}

function Resolve-EvidenceScript {
    $override = $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT
    if (-not [string]::IsNullOrWhiteSpace($override)) {
        return [System.IO.Path]::GetFullPath($override)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "phase2-run-evidence.ps1"))
}

function Get-ControlFeedPath([string]$Repo, [string]$Watch) {
    return Join-Path $Repo ".ensemble/control/$Watch.ndjson"
}

function Get-WatchFeedPath([string]$Repo, [string]$Watch) {
    return Join-Path $Repo ".ensemble/stream/$Watch.ndjson"
}

function Get-StringSha256([string]$Text) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($Text)
        $hash = $sha.ComputeHash($bytes)
        return (($hash | ForEach-Object { $_.ToString("x2") }) -join "")
    }
    finally {
        $sha.Dispose()
    }
}

function Get-FileSha256([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "generated crew file is missing: $Path (run phase2-fleet.ps1 with -Materialize before -RunSelected/-VerifyReports)"
    }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($Path)
        try {
            $hash = $sha.ComputeHash($stream)
            return (($hash | ForEach-Object { $_.ToString("x2") }) -join "")
        }
        finally {
            $stream.Dispose()
        }
    }
    finally {
        $sha.Dispose()
    }
}

function Get-ProjectCrewSha256($Project) {
    return Get-StringSha256 ([string]$Project.Text)
}

function Get-ProjectSpecSha256($Project, [string]$CrewSha256) {
    $spec = [ordered]@{
        kind = [string]$Project.Kind
        name = [string]$Project.Name
        node = [string]$Project.Node
        repo = [string]$Project.Repo
        crew = [string]$Project.Crew
        team = [string]$Project.Team
        watch = [string]$Project.Watch
        task = [string]$Project.Task
        merge = [bool]$Project.Merge
        minApprovals = [int]$Project.MinApprovals
        reviewerAgents = @($Project.ReviewerAgents | ForEach-Object { [string]$_ })
        crewSha256 = [string]$CrewSha256
    }
    return Get-StringSha256 ($spec | ConvertTo-Json -Depth 8 -Compress)
}

function Assert-GeneratedCrewCurrent($Project) {
    $expected = Get-ProjectCrewSha256 $Project
    $actual = Get-FileSha256 $Project.Crew
    if ($actual -ne $expected) {
        Fail "generated crew for '$($Project.Name)' is stale or hand-edited: $($Project.Crew). Run phase2-fleet.ps1 with -Materialize from the current manifest."
    }
    return $expected
}

function Normalize-ServiceAction([string]$Action) {
    if ([string]::IsNullOrWhiteSpace($Action)) {
        return "none"
    }
    $normalized = $Action.Trim().ToLowerInvariant()
    $allowed = @("none", "up", "install-print", "install", "uninstall-print", "uninstall")
    if ($allowed -notcontains $normalized) {
        Fail "unsupported -Service '$Action' (expected one of: $($allowed -join ', '))"
    }
    return $normalized
}

function New-ServiceArgs([string]$Action) {
    $normalized = Normalize-ServiceAction $Action
    switch ($normalized) {
        "none" { return @("up") }
        "up" { return @("up") }
        "install-print" { return @("serve", "--install-service", "--print") }
        "install" { return @("serve", "--install-service") }
        "uninstall-print" { return @("serve", "--uninstall-service", "--print") }
        "uninstall" { return @("serve", "--uninstall-service") }
        default { Fail "unsupported -Service '$Action'" }
    }
}

function Resolve-ManifestPath([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
}

function Resolve-FromBase([string]$Base, [string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $Base $Path))
}

function Normalize-NodeUrl([string]$NodeName) {
    $trimmed = $NodeName.Trim().TrimEnd('/')
    if ($trimmed.Length -eq 0) {
        Fail "node name must not be blank"
    }
    if ($trimmed.StartsWith("http://") -or $trimmed.StartsWith("https://")) {
        return $trimmed
    }
    if ($trimmed -match '^[^:/\[\]]+:\d+$') {
        return "http://$trimmed"
    }
    if ($trimmed.StartsWith("[") -and $trimmed.Contains("]")) {
        if ($trimmed -match '^\[[^\]]+\]:\d+$') {
            return "http://$trimmed"
        }
        return "http://$trimmed`:7878"
    }
    if ($trimmed.Contains(":") -and -not $trimmed.StartsWith("[")) {
        return "http://[$trimmed]:7878"
    }
    return "http://$trimmed`:7878"
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

function New-MainCrewText($Main) {
    $routes = Get-Prop $Main "routes" $null
    if ($null -eq $routes) {
        Fail "manifest is missing required field: main.routes"
    }
    $codexNode = Normalize-NodeUrl (Require-Prop $routes "codex" "main.routes")
    $claudeNode = Normalize-NodeUrl (Require-Prop $routes "claude" "main.routes")
    $agyNode = Normalize-NodeUrl (Require-Prop $routes "agy" "main.routes")
    $testCommand = [string](Get-Prop $Main "test" "cargo test --quiet")

    $lines = @(
        "# Generated by scripts/phase2-fleet.ps1. Do not edit by hand.",
        "# Edit the manifest, then rerun the script.",
        "# opencode is intentionally excluded from headless governed roles until its",
        "# non-interactive approval/hang behavior is stable.",
        'pipeline = ["implement", "review", "audit"]',
        "",
        "[gate]",
        "min_approvals = 2",
        "max_rounds = 2",
        'on_flake = "exclude"',
        "stall_limit = 0",
        "max_task_secs = 0",
        "",
        "[test]",
        "command = $(Toml-String $testCommand)",
        "",
        "[roles.implement]",
        'agent = "codex"',
        "",
        "[roles.review]",
        'agent = "claude"',
        "blind = true",
        "",
        "[roles.audit]",
        'agent = "agy"',
        "blind = true",
        "",
        "[agents.codex]",
        "node = $(Toml-String $codexNode)",
        "timeout = 60",
        'backup = "agy"',
        "",
        "[agents.claude]",
        "node = $(Toml-String $claudeNode)",
        "timeout = 60",
        'backup = "agy"',
        "",
        "[agents.agy]",
        "node = $(Toml-String $agyNode)",
        "timeout = 180"
    )
    return ($lines -join [Environment]::NewLine) + [Environment]::NewLine
}

function New-SatelliteCrewText($Satellite) {
    $nodeUrl = Normalize-NodeUrl (Require-Prop $Satellite "node" "satellites[]")
    $testCommand = [string](Get-Prop $Satellite "test" "cargo test --quiet")

    $lines = @(
        "# Generated by scripts/phase2-fleet.ps1. Do not edit by hand.",
        "# Edit the manifest, then rerun the script.",
        "# Satellite governed role set is codex + claude only.",
        "# codex also supplies the second reviewer role so satellites keep min_approvals=2.",
        'pipeline = ["implement", "review", "audit"]',
        "",
        "[gate]",
        "min_approvals = 2",
        "max_rounds = 3",
        'on_flake = "exclude"',
        "",
        "[test]",
        "command = $(Toml-String $testCommand)",
        "",
        "[roles.implement]",
        'agent = "codex"',
        "",
        "[roles.review]",
        'agent = "claude"',
        "blind = true",
        "",
        "[roles.audit]",
        'agent = "codex"',
        "blind = true",
        "",
        "[agents.codex]",
        "node = $(Toml-String $nodeUrl)",
        "timeout = 120",
        "",
        "[agents.claude]",
        "node = $(Toml-String $nodeUrl)",
        "timeout = 150"
    )
    return ($lines -join [Environment]::NewLine) + [Environment]::NewLine
}

function New-RunArgs([string]$Task, [string]$Crew, [string]$Repo, [string]$Team, [string]$Watch, [bool]$Merge) {
    $argList = @(
        "run",
        $Task,
        "--crew",
        $Crew,
        "--repo",
        $Repo,
        "--team",
        $Team,
        "--watch",
        $Watch
    )
    if ($Merge) {
        $argList += "--merge"
    }
    return $argList
}

function Select-Node([string]$Candidate) {
    return $Node -eq "all" -or $Node -eq $Candidate
}

function New-FleetPlan($Fleet, [string]$ManifestDir) {
    $nodes = @(Get-Prop $Fleet "nodes" @())
    if ($nodes.Count -eq 0) {
        Fail "manifest.nodes must contain m1..m5 (or your mapped host names)"
    }
    $conductor = [string](Get-Prop $Fleet "conductor" $nodes[0])
    $main = Get-Prop $Fleet "main" $null
    if ($null -eq $main) {
        Fail "manifest is missing required field: main"
    }
    $mainRepo = Resolve-FromBase $ManifestDir (Require-Prop $main "repo" "main")
    $mainTeam = [string](Get-Prop $main "team" "main")
    $mainWatch = [string](Get-Prop $main "watch" $mainTeam)
    $mainTask = [string](Get-Prop $main "task" "phase2 main smoke run")
    $mainMerge = [bool](Get-Prop $main "merge" $true)
    $mainOutDir = Resolve-FromBase $mainRepo $OutDir
    $mainCrew = Resolve-FromBase $mainOutDir "crew-main.generated.toml"
    $projects = @()
    $commands = @()

    $serviceArgs = New-ServiceArgs $Service
    foreach ($fleetNode in $nodes) {
        if (Select-Node ([string]$fleetNode)) {
            $commands += New-CommandSpec -NodeName ([string]$fleetNode) -Kind "service" -ArgList $serviceArgs
        }
    }

    $mainSelected = Select-Node $conductor
    $projects += [pscustomobject]@{
        Kind     = "main"
        Name     = "main"
        Node     = $conductor
        Repo     = $mainRepo
        Crew     = $mainCrew
        Team     = $mainTeam
        Watch    = $mainWatch
        Task     = $mainTask
        Merge    = $mainMerge
        Text     = New-MainCrewText $main
        MinApprovals = 2
        ReviewerAgents = @("claude", "agy")
        Selected = $mainSelected
    }
    if ($mainSelected) {
        $commands += New-CommandSpec -NodeName $conductor -Kind "run" -ArgList (New-RunArgs $mainTask $mainCrew $mainRepo $mainTeam $mainWatch $mainMerge)
        $commands += New-CommandSpec -NodeName $conductor -Kind "watch" -ArgList @("watch", $mainWatch, "--repo", $mainRepo, "--team", $mainTeam, "--follow")
    }

    $satellites = @(Get-Prop $Fleet "satellites" @())
    foreach ($sat in $satellites) {
        $satName = Require-Prop $sat "name" "satellites[]"
        $satNode = Require-Prop $sat "node" "satellites[$satName]"
        $satRepo = Resolve-FromBase $ManifestDir (Require-Prop $sat "repo" "satellites[$satName]")
        $satTeam = [string](Get-Prop $sat "team" $satName)
        $satWatch = [string](Get-Prop $sat "watch" $satTeam)
        $satTask = [string](Get-Prop $sat "task" "phase2 satellite smoke run")
        $satOutDir = Resolve-FromBase $satRepo $OutDir
        $satCrew = Resolve-FromBase $satOutDir "crew-$satName.generated.toml"
        $satSelected = Select-Node $satNode
        $projects += [pscustomobject]@{
            Kind     = "satellite"
            Name     = $satName
            Node     = $satNode
            Repo     = $satRepo
            Crew     = $satCrew
            Team     = $satTeam
            Watch    = $satWatch
            Task     = $satTask
            Merge    = $false
            Text     = New-SatelliteCrewText $sat
            MinApprovals = 2
            ReviewerAgents = @("claude", "codex")
            Selected = $satSelected
        }
        if ($satSelected) {
            $commands += New-CommandSpec -NodeName $satNode -Kind "run" -ArgList (New-RunArgs $satTask $satCrew $satRepo $satTeam $satWatch $false)
            $commands += New-CommandSpec -NodeName $satNode -Kind "watch" -ArgList @("watch", $satWatch, "--repo", $satRepo, "--team", $satTeam, "--follow")
        }
    }

    return [pscustomobject]@{
        Nodes     = @($nodes | ForEach-Object { [string]$_ })
        Conductor = $conductor
        Projects  = $projects
        Commands  = $commands
    }
}

function Read-FleetManifest([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        Fail "manifest not found: $Path (use -InitSample first)"
    }
    return Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
}

function Write-Plan($Plan) {
    Write-Host "Fleet nodes: $($Plan.Nodes -join ', ')"
    Write-Host "Conductor : $($Plan.Conductor)"
    Write-Host ""
    Write-Host "Generated crew files:"
    foreach ($project in $Plan.Projects) {
        $mark = if ($project.Selected) { "*" } else { " " }
        Write-Host (" {0} [{1}] {2} on {3}" -f $mark, $project.Kind, $project.Crew, $project.Node)
    }
    Write-Host ""
    Write-Host "Commands:"
    foreach ($cmd in $Plan.Commands) {
        Write-Host ("[{0}] {1}" -f $cmd.Node, $cmd.Text)
    }
}

function Convert-PlanForJson($Plan) {
    return [pscustomobject]@{
        nodes     = @($Plan.Nodes)
        conductor = $Plan.Conductor
        projects  = @($Plan.Projects | ForEach-Object {
                [pscustomobject]@{
                    kind     = $_.Kind
                    name     = $_.Name
                    node     = $_.Node
                    repo     = $_.Repo
                    crew     = $_.Crew
                    team     = $_.Team
                    watch    = $_.Watch
                    task     = $_.Task
                    merge    = [bool]$_.Merge
                    min_approvals = [int]$_.MinApprovals
                    reviewer_agents = @($_.ReviewerAgents)
                    selected = [bool]$_.Selected
                }
            })
        commands  = @($Plan.Commands | ForEach-Object {
                [pscustomobject]@{
                    node = $_.Node
                    kind = $_.Kind
                    args = @($_.Args)
                    text = $_.Text
                }
            })
    }
}

function Write-PlanJson($Plan) {
    Convert-PlanForJson $Plan | ConvertTo-Json -Depth 12
}

function Materialize-Plan($Plan) {
    foreach ($project in $Plan.Projects) {
        if (-not $project.Selected) {
            continue
        }
        if (-not (Test-Path -LiteralPath $project.Repo -PathType Container)) {
            Fail "$($project.Kind) repo for '$($project.Name)' is not accessible: $($project.Repo)"
        }
        $dir = Split-Path -Parent $project.Crew
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        [System.IO.File]::WriteAllText($project.Crew, [string]$project.Text, [System.Text.UTF8Encoding]::new($false))
        Write-Host "wrote $($project.Crew)"
    }
}

function Check-FleetNodes($Plan) {
    if (-not (Get-Command ensemble -ErrorAction SilentlyContinue)) {
        Fail "ensemble is not on PATH; install first"
    }
    $meshOut = & ensemble mesh 2>&1
    $meshCode = $LASTEXITCODE
    $meshText = ($meshOut | Out-String)
    Write-Host $meshText.TrimEnd()
    if ($meshCode -ne 0) {
        Fail "ensemble mesh exited $meshCode"
    }
    $nodesOut = & ensemble nodes 2>&1
    $nodesCode = $LASTEXITCODE
    $nodesText = ($nodesOut | Out-String)
    Write-Host $nodesText.TrimEnd()
    if ($nodesCode -ne 0) {
        Fail "ensemble nodes exited $nodesCode"
    }
    $hosts = @(Get-UrlHosts $meshText)
    $expectedRemoteNodes = @($Plan.Nodes | Where-Object {
            -not ([string]$_).Equals($Plan.Conductor, [System.StringComparison]::OrdinalIgnoreCase)
        })
    $missing = New-Object System.Collections.Generic.List[string]
    foreach ($fleetNode in $expectedRemoteNodes) {
        if (-not (Test-HostPresent $fleetNode $hosts)) {
            $missing.Add($fleetNode)
        }
    }
    if ($missing.Count -gt 0) {
        Fail "expected remote fleet node(s) not found in mesh output: $($missing -join ', ')"
    }
    Write-Host "fleet node check passed (remote peers: $($expectedRemoteNodes -join ', '); local conductor skipped: $($Plan.Conductor))"
}

function Capture-RunEvidenceCursors($Project) {
    $inboxOut = & ensemble team inbox --repo $Project.Repo --team $Project.Team --json 2>&1
    $inboxCode = $LASTEXITCODE
    if ($null -eq $inboxCode) {
        $inboxCode = 0
    }
    $inboxText = ($inboxOut | Out-String).Trim()
    if ($inboxCode -ne 0) {
        Fail "team inbox cursor capture failed for '$($Project.Name)' with exit code $inboxCode`n$inboxText"
    }
    $inbox = Parse-JsonOrFail "team inbox cursor capture for '$($Project.Name)'" $inboxText
    return [pscustomobject]@{
        Team    = [int]$inbox.next
        Watch   = Count-NonEmptyFileLines (Get-WatchFeedPath $Project.Repo $Project.Watch)
        Control = Count-NonEmptyFileLines (Get-ControlFeedPath $Project.Repo $Project.Watch)
    }
}

function Invoke-RunEvidenceVerifier($Project, $Cursors, [string]$ExpectedTerminal) {
    $evidenceScript = Resolve-EvidenceScript
    if (-not (Test-Path -LiteralPath $evidenceScript -PathType Leaf)) {
        Fail "phase2 run evidence verifier not found: $evidenceScript"
    }
    $args = @(
        "-NoProfile", "-File", $evidenceScript,
        "-Repo", $Project.Repo,
        "-Team", $Project.Team,
        "-Watch", $Project.Watch,
        "-TeamSince", "$($Cursors.Team)",
        "-WatchSince", "$($Cursors.Watch)",
        "-ControlSince", "$($Cursors.Control)",
        "-ExpectTerminal", $ExpectedTerminal,
        "-NoBuild"
    )
    if ($RequireControlEvidence) {
        $args += "-RequireControl"
    }
    if ($RequireSteerEvidence) {
        $args += "-RequireSteer"
    }
    if ($RequireAbortEvidence) {
        $args += "-RequireAbort"
    }
    Write-Host ("verifying evidence [{0}] {1}/{2} since team={3} watch={4} control={5}" -f $Project.Node, $Project.Team, $Project.Watch, $Cursors.Team, $Cursors.Watch, $Cursors.Control) -ForegroundColor Cyan
    & pwsh @args
    $code = $LASTEXITCODE
    if ($null -eq $code) {
        $code = 0
    }
    if ($code -ne 0) {
        Fail "phase2 run evidence verifier failed for '$($Project.Name)' with exit code $code"
    }
}

function Convert-ToReportFilePart([string]$Value) {
    $part = [regex]::Replace($Value.ToLowerInvariant(), '[^a-z0-9._-]+', '-').Trim('-')
    $part = [regex]::Replace($part, '^[.]+|[.]+$', '')
    if ([string]::IsNullOrWhiteSpace($part)) {
        return "unnamed"
    }
    return $part
}

function Get-AcceptanceReportPath($Project) {
    $dir = Split-Path -Parent $Project.Crew
    $projectPart = Convert-ToReportFilePart $Project.Name
    $nodePart = Convert-ToReportFilePart $Project.Node
    return Join-Path $dir ("acceptance-{0}-{1}.json" -f $projectPart, $nodePart)
}

function Clear-AcceptanceReport($Project) {
    $path = Get-AcceptanceReportPath $Project
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        Remove-Item -LiteralPath $path -Force
    }
}

function Write-AcceptanceReport($Project, [object[]]$Runs) {
    $crewSha256 = Assert-GeneratedCrewCurrent $Project
    $specSha256 = Get-ProjectSpecSha256 $Project $crewSha256
    $path = Get-AcceptanceReportPath $Project
    $dir = Split-Path -Parent $path
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $report = [pscustomobject]@{
        ok = $true
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString("o")
        node = $Project.Node
        repeatCount = [int]$RepeatCount
        verifyEvidence = [bool]$VerifyEvidence
        allowEscalatedRun = [bool]$AllowEscalatedRun
        requireControlEvidence = [bool]$RequireControlEvidence
        requireSteerEvidence = [bool]$RequireSteerEvidence
        requireAbortEvidence = [bool]$RequireAbortEvidence
        project = [pscustomobject]@{
            kind = $Project.Kind
            name = $Project.Name
            repo = $Project.Repo
            team = $Project.Team
            watch = $Project.Watch
            crew = $Project.Crew
            task = $Project.Task
            crewSha256 = $crewSha256
            specSha256 = $specSha256
            merge = [bool]$Project.Merge
            minApprovals = [int]$Project.MinApprovals
            reviewerAgents = @($Project.ReviewerAgents)
        }
        runs = @($Runs)
    }
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $path -Encoding utf8NoBOM
    Write-Host "wrote acceptance report: $path"
}

function Read-AcceptanceReport($Project) {
    $path = Get-AcceptanceReportPath $Project
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Fail "missing acceptance report for '$($Project.Name)' on node '$($Project.Node)': $path"
    }
    try {
        return Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
    }
    catch {
        Fail "acceptance report for '$($Project.Name)' is not valid JSON: $path`n$($_.Exception.Message)"
    }
}

function Require-ReportField($Object, [string]$Name, [string]$Context) {
    if ($null -eq $Object) {
        Fail "$Context is missing required field '$Name'"
    }
    $prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $prop -or $null -eq $prop.Value) {
        Fail "$Context is missing required field '$Name'"
    }
    return $prop.Value
}

function Assert-AcceptanceReport($Project, $Report) {
    $context = "acceptance report for '$($Project.Name)'"
    $ok = Require-ReportField $Report "ok" $context
    $node = Require-ReportField $Report "node" $context
    $verifyEvidence = Require-ReportField $Report "verifyEvidence" $context
    $repeatCount = Require-ReportField $Report "repeatCount" $context
    $projectInfo = Require-ReportField $Report "project" $context
    $runsValue = Require-ReportField $Report "runs" $context

    if ($ok -ne $true) {
        Fail "$context is not ok=true"
    }
    if ($verifyEvidence -ne $true) {
        Fail "$context was not produced by -VerifyEvidence"
    }
    if ($node -ne $Project.Node) {
        Fail "$context has node '$node', expected '$($Project.Node)'"
    }

    $projectName = Require-ReportField $projectInfo "name" "$context project"
    $projectTeam = Require-ReportField $projectInfo "team" "$context project"
    $projectWatch = Require-ReportField $projectInfo "watch" "$context project"
    $projectCrewSha256 = Require-ReportField $projectInfo "crewSha256" "$context project"
    $projectSpecSha256 = Require-ReportField $projectInfo "specSha256" "$context project"
    $expectedCrewSha256 = Assert-GeneratedCrewCurrent $Project
    if ([string]$projectCrewSha256 -ne $expectedCrewSha256) {
        Fail "$context was produced from a stale generated crew hash"
    }
    $expectedSpecSha256 = Get-ProjectSpecSha256 $Project $expectedCrewSha256
    if ([string]$projectSpecSha256 -ne $expectedSpecSha256) {
        Fail "$context was produced from a stale project spec hash"
    }
    if ($projectName -ne $Project.Name) {
        Fail "acceptance report project name mismatch: got '$projectName', expected '$($Project.Name)'"
    }
    if ($projectTeam -ne $Project.Team -or $projectWatch -ne $Project.Watch) {
        Fail "$context has wrong team/watch"
    }
    if ([int]$repeatCount -lt [int]$RepeatCount) {
        Fail "$context repeatCount=$repeatCount, expected at least $RepeatCount"
    }
    $runs = @($runsValue)
    if ($runs.Count -lt [int]$RepeatCount) {
        Fail "$context records $($runs.Count) run(s), expected at least $RepeatCount"
    }
    $index = 0
    foreach ($run in $runs) {
        $index++
        $runContext = "$context run #$index"
        $evidenceVerified = Require-ReportField $run "evidenceVerified" $runContext
        $terminal = Require-ReportField $run "terminal" $runContext
        $exitCode = Require-ReportField $run "exitCode" $runContext
        if ($evidenceVerified -ne $true) {
            Fail "$context has an unverified run entry"
        }
        if ($terminal -ne "landed" -and $terminal -ne "escalated") {
            Fail "$context has invalid terminal '$terminal'"
        }
        if ([int]$exitCode -ne 0) {
            $allowedEscalated = Require-ReportField $run "allowedEscalated" $runContext
            if ($allowedEscalated -ne $true) {
                Fail "$context has nonzero exitCode without allowedEscalated=true"
            }
        }
    }
}
function Invoke-AcceptanceReportVerifier($Plan) {
    $projects = @($Plan.Projects | Where-Object { $_.Selected })
    if ($projects.Count -eq 0) {
        Fail "no selected acceptance reports for node '$Node'"
    }
    foreach ($project in $projects) {
        $report = Read-AcceptanceReport $project
        Assert-AcceptanceReport $project $report
        Write-Host ("verified acceptance report [{0}] {1}: {2}" -f $project.Node, $project.Name, (Get-AcceptanceReportPath $project))
    }
}
function Invoke-SelectedRuns($Plan) {
    $projects = @($Plan.Projects | Where-Object { $_.Selected })
    if ($projects.Count -eq 0) {
        Fail "no selected run commands for node '$Node'"
    }
    foreach ($project in $projects) {
        Clear-AcceptanceReport $project
    }
    if (-not (Get-Command ensemble -ErrorAction SilentlyContinue)) {
        Fail "ensemble is not on PATH; install first"
    }
    $acceptanceReports = @()
    foreach ($project in $projects) {
        Assert-GeneratedCrewCurrent $project | Out-Null
        $runArgs = New-RunArgs $project.Task $project.Crew $project.Repo $project.Team $project.Watch ([bool]$project.Merge)
        $cmdText = Format-Command $runArgs
        $runReports = @()
        for ($iteration = 1; $iteration -le $RepeatCount; $iteration++) {
            Write-Host ""
            if ($RepeatCount -gt 1) {
                Write-Host ("running [{0}] repeat {1}/{2}: {3}" -f $project.Node, $iteration, $RepeatCount, $cmdText)
            } else {
                Write-Host ("running [{0}] {1}" -f $project.Node, $cmdText)
            }
            if (-not $VerifyEvidence) {
                & ensemble @runArgs
                $code = $LASTEXITCODE
                if ($null -eq $code) {
                    $code = 0
                }
                if ($code -ne 0) {
                    Fail "selected run failed on node '$($project.Node)' with exit code $code"
                }
                continue
            }

            $cursors = Capture-RunEvidenceCursors $project
            $runOut = & ensemble @runArgs 2>&1
            $code = $LASTEXITCODE
            if ($null -eq $code) {
                $code = 0
            }
            $runText = ($runOut | Out-String)
            if (-not [string]::IsNullOrWhiteSpace($runText)) {
                Write-Host $runText.TrimEnd()
            }
            $terminal = Get-RunTerminal $runText
            Invoke-RunEvidenceVerifier $project $cursors $terminal
            $allowedEscalated = $false
            if ($code -ne 0) {
                if ($terminal -eq "escalated" -and $AllowEscalatedRun) {
                    $allowedEscalated = $true
                    Write-Host "  run escalated; continuing by design because -AllowEscalatedRun is set." -ForegroundColor Yellow
                } else {
                    Fail "selected run failed on node '$($project.Node)' with exit code $code"
                }
            }
            $runReports += [pscustomobject]@{
                iteration = [int]$iteration
                command = $cmdText
                exitCode = [int]$code
                terminal = $terminal
                evidenceVerified = $true
                allowedEscalated = $allowedEscalated
                teamSince = [int]$cursors.Team
                watchSince = [int]$cursors.Watch
                controlSince = [int]$cursors.Control
            }
        }
        if ($VerifyEvidence) {
            $acceptanceReports += [pscustomobject]@{
                Project = $project
                Runs    = @($runReports)
            }
        }
    }
    if ($VerifyEvidence) {
        foreach ($report in $acceptanceReports) {
            Write-AcceptanceReport -Project $report.Project -Runs @($report.Runs)
        }
    }
}

function Invoke-SelectedServices($Plan) {
    if ($RemoteService) {
        if ($RemoteServiceTransport -eq "tailscale" -and -not (Get-Command tailscale -ErrorAction SilentlyContinue)) {
            Fail "tailscale is not on PATH; remote service bootstrap with -RemoteServiceTransport tailscale requires Tailscale SSH"
        }
        if ($RemoteServiceTransport -eq "ssh" -and -not (Get-Command ssh -ErrorAction SilentlyContinue)) {
            Fail "ssh is not on PATH; remote service bootstrap with -RemoteServiceTransport ssh requires OpenSSH"
        }
    }
    if (-not (Get-Command ensemble -ErrorAction SilentlyContinue)) {
        Fail "ensemble is not on PATH; install first"
    }
    $services = @($Plan.Commands | Where-Object { $_.Kind -eq "service" })
    if ($services.Count -eq 0) {
        Fail "no selected service commands for node '$Node'"
    }
    foreach ($cmd in $services) {
        Write-Host ""
        $isLocalConductor = $RemoteService -and $cmd.Node.Equals([string]$Plan.Conductor, [System.StringComparison]::OrdinalIgnoreCase)
        if ($RemoteService -and -not $isLocalConductor) {
            if ($RemoteServiceTransport -eq "ssh") {
                Write-Host ("running remote [{0}] ssh -o BatchMode=yes -o ConnectTimeout=10 {0} ensemble {1}" -f $cmd.Node, ($cmd.Args -join " "))
                & ssh -o BatchMode=yes -o ConnectTimeout=10 $cmd.Node ensemble @($cmd.Args)
            }
            else {
                Write-Host ("running remote [{0}] tailscale ssh {0} ensemble {1}" -f $cmd.Node, ($cmd.Args -join " "))
                & tailscale ssh $cmd.Node ensemble @($cmd.Args)
            }
        }
        else {
            Write-Host ("running [{0}] {1}" -f $cmd.Node, $cmd.Text)
            & ensemble @($cmd.Args)
        }
        $code = $LASTEXITCODE
        if ($null -eq $code) {
            $code = 0
        }
        if ($code -ne 0) {
            Fail "selected service command failed on node '$($cmd.Node)' with exit code $code"
        }
    }
}

function Write-SampleManifest([string]$Path) {
    if ((Test-Path -LiteralPath $Path) -and -not $Force) {
        Fail "manifest already exists: $Path (use -Force to overwrite)"
    }
    $sample = @'
{
  "nodes": ["m1", "m2", "m3", "m4", "m5"],
  "conductor": "m1",
  "main": {
    "repo": "D:\\Projects\\main-project",
    "team": "main",
    "watch": "main",
    "task": "phase2 main smoke run",
    "test": "cargo test --quiet",
    "merge": true,
    "routes": {
      "codex": "m1",
      "claude": "m2",
      "agy": "m3"
    }
  },
  "satellites": [
    {
      "name": "sat-a",
      "repo": "D:\\Projects\\sat-a",
      "node": "m2",
      "team": "sat-a",
      "watch": "sat-a",
      "task": "phase2 satellite A smoke run",
      "test": "cargo test --quiet"
    },
    {
      "name": "sat-b",
      "repo": "D:\\Projects\\sat-b",
      "node": "m3",
      "team": "sat-b",
      "watch": "sat-b",
      "task": "phase2 satellite B smoke run",
      "test": "cargo test --quiet"
    },
    {
      "name": "sat-c",
      "repo": "D:\\Projects\\sat-c",
      "node": "m4",
      "team": "sat-c",
      "watch": "sat-c",
      "task": "phase2 satellite C smoke run",
      "test": "cargo test --quiet"
    },
    {
      "name": "sat-d",
      "repo": "D:\\Projects\\sat-d",
      "node": "m5",
      "team": "sat-d",
      "watch": "sat-d",
      "task": "phase2 satellite D smoke run",
      "test": "cargo test --quiet"
    }
  ]
}
'@
    $dir = Split-Path -Parent $Path
    if ($dir) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
    Set-Content -LiteralPath $Path -Value $sample -Encoding utf8NoBOM
    Write-Host "wrote sample manifest: $Path"
}

function Invoke-SelfTest {
    $root = Join-Path ([System.IO.Path]::GetTempPath()) ("ensemble-phase2-fleet-" + [System.Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $mainRepo = Join-Path $root "main"
    $satRepo = Join-Path $root "sat-a"
    New-Item -ItemType Directory -Force -Path $mainRepo, $satRepo | Out-Null
    $json = @{
        nodes      = @("m1", "m2")
        conductor  = "m1"
        main       = @{
            repo   = $mainRepo
            test   = "echo main"
            routes = @{
                codex  = "m1"
                claude = "m2"
                agy    = "m2"
            }
        }
        satellites = @(
            @{
                name = "sat-a"
                repo = $satRepo
                node = "m2"
                test = "echo sat"
            }
        )
    } | ConvertTo-Json -Depth 6
    $manifestPath = Join-Path $root "fleet.json"
    Set-Content -LiteralPath $manifestPath -Value $json -Encoding utf8NoBOM
    $fleet = Read-FleetManifest $manifestPath
    $plan = New-FleetPlan $fleet $root
    $jsonPlan = Convert-PlanForJson $plan | ConvertTo-Json -Depth 12 | ConvertFrom-Json
    if (@($jsonPlan.projects).Count -ne 2) {
        Fail "self-test expected JSON plan to include main plus one satellite project"
    }
    foreach ($project in @($jsonPlan.projects)) {
        if ([int]$project.min_approvals -lt 2) {
            Fail "self-test expected every generated project to keep min_approvals >= 2"
        }
        if (@($project.reviewer_agents).Count -lt [int]$project.min_approvals) {
            Fail "self-test expected every generated project to expose enough reviewer agents for its quorum"
        }
    }
    if (@($jsonPlan.commands | Where-Object { $_.kind -eq "service" }).Count -ne 2) {
        Fail "self-test expected JSON plan to include one service command per node"
    }
    if (@($jsonPlan.commands | Where-Object { $_.kind -eq "run" }).Count -ne 2) {
        Fail "self-test expected JSON plan to include one run command per project"
    }
    if (@($jsonPlan.commands | Where-Object { $_.kind -eq "watch" }).Count -ne 2) {
        Fail "self-test expected JSON plan to include one watch command per project"
    }
    $main = @($plan.Projects | Where-Object { $_.Kind -eq "main" })[0]
    if ($main.Text -notmatch 'node = "http://m1:7878"') {
        Fail "self-test expected main codex route to be normalized"
    }
    if ((Normalize-NodeUrl "m2:9000") -ne "http://m2:9000") {
        Fail "self-test expected host:port to be preserved"
    }
    if ($main.Text -notmatch 'min_approvals = 2') {
        Fail "self-test expected main quorum to stay 2"
    }
    if (-not (Test-HostPresent "m2" @("m2.tail.ts.net"))) {
        Fail "self-test expected short host to match MagicDNS host"
    }
    if (Test-HostPresent "m1" @("m10.tail.ts.net")) {
        Fail "self-test expected m1 not to match m10"
    }
    if ((Get-RunTerminal "reviewer mentioned ESCALATED`nLANDED after 1 round(s)(s)") -ne "landed") {
        Fail "self-test expected terminal parser to use the explicit LANDED terminal line"
    }
    if ((Get-RunTerminal "notes mention LANDED`nESCALATED after 2 round(s): max rounds reached") -ne "escalated") {
        Fail "self-test expected terminal parser to use the explicit ESCALATED terminal line"
    }
    $script:Node = "m2"
    $nodePlan = New-FleetPlan $fleet $root
    $sat = @($nodePlan.Projects | Where-Object { $_.Name -eq "sat-a" })[0]
    if (-not $sat.Selected) {
        Fail "self-test expected m2 satellite to be selected"
    }
    if ($sat.Text -match 'backup = "agy"') {
        Fail "self-test expected satellite generated crew to stay codex+claude only"
    }
    if ($sat.Text -notmatch 'min_approvals = 2') {
        Fail "self-test expected satellite generated crew to keep the Phase 2 min_approvals=2 invariant"
    }
    if ($sat.Text -notmatch '\[roles\.audit\]' -or $sat.Text -notmatch 'agent = "codex"') {
        Fail "self-test expected satellite generated crew to include a codex audit reviewer"
    }
    if ($sat.Text -notmatch 'node = "http://m2:7878"') {
        Fail "self-test expected satellite node to be normalized"
    }
    Materialize-Plan $nodePlan
    $fakeBin = Join-Path $root "fake-bin"
    New-Item -ItemType Directory -Force -Path $fakeBin | Out-Null
    $fakeLog = Join-Path $root "fake-ensemble.log"
    if ($IsWindows) {
        $fakeExe = Join-Path $fakeBin "ensemble.cmd"
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "@echo off",
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            "exit /b 0"
        )
    }
    else {
        $fakeExe = Join-Path $fakeBin "ensemble"
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "#!/bin/sh",
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            "exit 0"
        )
        & chmod +x $fakeExe
    }
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    try {
        Invoke-SelectedRuns $nodePlan
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) {
            Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue
        }
        else {
            $env:ENSEMBLE_FAKE_LOG = $oldFakeLog
        }
    }
    $fakeOut = Get-Content -Raw -LiteralPath $fakeLog
    $fakeLines = @($fakeOut -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    if ($fakeLines.Count -ne 1) {
        Fail "self-test expected -RunSelected to execute exactly one selected run"
    }
    if ($fakeLines[0] -notmatch '^run "?phase2 satellite smoke run"? --crew .+crew-sat-a\.generated\.toml --repo .+sat-a --team sat-a --watch sat-a') {
        Fail "self-test expected -RunSelected to execute the selected satellite run"
    }
    if ($fakeLines[0] -match '^(watch|up)(\s|$)') {
        Fail "self-test expected -RunSelected to execute run commands only"
    }

    Set-Content -LiteralPath $fakeLog -Encoding utf8NoBOM -Value ""
    $evidenceLog = Join-Path $root "fake-evidence.log"
    $fakeEvidence = Join-Path $root "fake-evidence.ps1"
    Set-Content -LiteralPath $fakeEvidence -Encoding utf8NoBOM -Value @(
        '$text = $args -join '' ''',
        "Set-Content -LiteralPath `$env:ENSEMBLE_FAKE_EVIDENCE_LOG -Encoding utf8NoBOM -Value `$text",
        "if (`$text -notmatch '-ExpectTerminal landed') { exit 42 }",
        "if (`$text -notmatch '-TeamSince 0') { exit 43 }",
        "if (`$text -notmatch '-WatchSince 0') { exit 44 }",
        "exit 0"
    )
    if ($IsWindows) {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "@echo off",
            'if "%1"=="team" (',
            '  echo {"messages":[],"next":0}',
            '  exit /b 0',
            ')',
            'if "%1"=="watch" (',
            '  exit /b 0',
            ')',
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            'echo LANDED after 1 round(s)',
            'exit /b 0'
        )
    }
    else {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "#!/bin/sh",
            'if [ "$1" = "team" ]; then printf ''{"messages":[],"next":0}\n''; exit 0; fi',
            'if [ "$1" = "watch" ]; then exit 0; fi',
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            'printf ''LANDED after 1 round(s)\n''',
            'exit 0'
        )
        & chmod +x $fakeExe
    }
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $oldEvidenceScript = $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT
    $oldEvidenceLog = $env:ENSEMBLE_FAKE_EVIDENCE_LOG
    $oldVerifyEvidence = $script:VerifyEvidence
    $oldRepeatCount = $script:RepeatCount
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $fakeEvidence
    $env:ENSEMBLE_FAKE_EVIDENCE_LOG = $evidenceLog
    $script:VerifyEvidence = $true
    $script:RepeatCount = 2
    try {
        Invoke-SelectedRuns $nodePlan
    }
    finally {
        $env:PATH = $oldPath
        $script:VerifyEvidence = $oldVerifyEvidence
        $script:RepeatCount = $oldRepeatCount
        if ($null -eq $oldFakeLog) { Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_LOG = $oldFakeLog }
        if ($null -eq $oldEvidenceScript) { Remove-Item Env:\ENSEMBLE_PHASE2_EVIDENCE_SCRIPT -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $oldEvidenceScript }
        if ($null -eq $oldEvidenceLog) { Remove-Item Env:\ENSEMBLE_FAKE_EVIDENCE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_EVIDENCE_LOG = $oldEvidenceLog }
    }
    $verifyFakeOut = Get-Content -Raw -LiteralPath $fakeLog
    $verifyFakeLines = @($verifyFakeOut -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    if ($verifyFakeLines.Count -ne 2) {
        Fail "self-test expected -RunSelected -VerifyEvidence -RepeatCount 2 to execute exactly two selected runs"
    }
    $evidenceOut = Get-Content -Raw -LiteralPath $evidenceLog
    if ($evidenceOut -notmatch '-Repo .+sat-a') {
        Fail "self-test expected verifier to receive the selected satellite repo"
    }
    if ($evidenceOut -notmatch '-Team sat-a') {
        Fail "self-test expected verifier to receive the selected satellite team"
    }
    $acceptanceReport = Join-Path (Split-Path -Parent $sat.Crew) "acceptance-sat-a-m2.json"
    if (-not (Test-Path -LiteralPath $acceptanceReport -PathType Leaf)) {
        Fail "self-test expected -RunSelected -VerifyEvidence -RepeatCount 2 to write a per-project acceptance report"
    }
    $report = Get-Content -Raw -LiteralPath $acceptanceReport | ConvertFrom-Json
    if ($report.ok -ne $true) {
        Fail "self-test expected acceptance report ok=true"
    }
    if ([int]$report.repeatCount -ne 2) {
        Fail "self-test expected acceptance report to record repeatCount=2"
    }
    if (@($report.runs).Count -ne 2) {
        Fail "self-test expected acceptance report to record both repeated runs"
    }
    if (@($report.runs | Where-Object { $_.terminal -eq "landed" -and $_.evidenceVerified -eq $true }).Count -ne 2) {
        Fail "self-test expected acceptance report to record verified landed terminals"
    }
    if ($report.project.name -ne "sat-a" -or $report.node -ne "m2") {
        Fail "self-test expected acceptance report to identify the selected project and node"
    }
    if ([string]::IsNullOrWhiteSpace([string]$report.project.crewSha256)) {
        Fail "self-test expected acceptance report to bind the generated crew hash"
    }
    if ([string]::IsNullOrWhiteSpace([string]$report.project.specSha256)) {
        Fail "self-test expected acceptance report to bind the generated project spec hash"
    }
    Invoke-AcceptanceReportVerifier $nodePlan
    $scriptPath = if ([string]::IsNullOrWhiteSpace($PSCommandPath)) { Join-Path $PSScriptRoot "phase2-fleet.ps1" } else { $PSCommandPath }
    $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $manifestPath -Node m2 -VerifyReports -RepeatCount 2 2>&1
    $childCode = $LASTEXITCODE
    if ($null -eq $childCode) {
        $childCode = 0
    }
    if ($childCode -ne 0) {
        Fail "self-test expected CLI -VerifyReports to validate the selected acceptance report`n$($childOut | Out-String)"
    }

    Set-Content -LiteralPath $sat.Crew -Encoding utf8NoBOM -Value "stale generated crew"
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $oldEvidenceScript = $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT
    $oldEvidenceLog = $env:ENSEMBLE_FAKE_EVIDENCE_LOG
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $fakeEvidence
    $env:ENSEMBLE_FAKE_EVIDENCE_LOG = $evidenceLog
    try {
        $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $manifestPath -Node m2 -RunSelected -VerifyEvidence 2>&1
        $childCode = $LASTEXITCODE
        if ($null -eq $childCode) {
            $childCode = 0
        }
        if ($childCode -ne 0) {
            Fail "self-test expected CLI -RunSelected to refresh a stale generated crew before running`n$($childOut | Out-String)"
        }
        Assert-GeneratedCrewCurrent $sat | Out-Null
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) { Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_LOG = $oldFakeLog }
        if ($null -eq $oldEvidenceScript) { Remove-Item Env:\ENSEMBLE_PHASE2_EVIDENCE_SCRIPT -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $oldEvidenceScript }
        if ($null -eq $oldEvidenceLog) { Remove-Item Env:\ENSEMBLE_FAKE_EVIDENCE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_EVIDENCE_LOG = $oldEvidenceLog }
    }

    $malformedReport = [pscustomobject]@{
        ok = $true
        node = "m2"
        verifyEvidence = $true
        repeatCount = 2
        project = [pscustomobject]@{
            name = "sat-a"
            team = "sat-a"
            watch = "sat-a"
        }
        runs = @(
            [pscustomobject]@{
                iteration = 1
                terminal = "landed"
                evidenceVerified = $true
                exitCode = $null
            },
            [pscustomobject]@{
                iteration = 2
                terminal = "landed"
                evidenceVerified = $true
                exitCode = $null
            }
        )
    }
    $malformedReport | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $acceptanceReport -Encoding utf8NoBOM
    $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $manifestPath -Node m2 -VerifyReports -RepeatCount 2 2>&1
    $childCode = $LASTEXITCODE
    if ($null -eq $childCode) {
        $childCode = 0
    }
    if ($childCode -eq 0) {
        Fail "self-test expected CLI -VerifyReports to reject a run entry missing exitCode`n$($childOut | Out-String)"
    }
    $wrongCrewHashReport = [pscustomobject]@{
        ok = $true
        node = "m2"
        verifyEvidence = $true
        repeatCount = 2
        project = [pscustomobject]@{
            name = "sat-a"
            team = "sat-a"
            watch = "sat-a"
            crewSha256 = "0000000000000000000000000000000000000000000000000000000000000000"
        }
        runs = @(
            [pscustomobject]@{
                iteration = 1
                terminal = "landed"
                evidenceVerified = $true
                exitCode = 0
            },
            [pscustomobject]@{
                iteration = 2
                terminal = "landed"
                evidenceVerified = $true
                exitCode = 0
            }
        )
    }
    $wrongCrewHashReport | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $acceptanceReport -Encoding utf8NoBOM
    $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $manifestPath -Node m2 -VerifyReports -RepeatCount 2 2>&1
    $childCode = $LASTEXITCODE
    if ($null -eq $childCode) {
        $childCode = 0
    }
    if ($childCode -eq 0) {
        Fail "self-test expected CLI -VerifyReports to reject an acceptance report with a stale generated crew hash`n$($childOut | Out-String)"
    }
    $currentCrewHash = Assert-GeneratedCrewCurrent $sat
    $wrongSpecHashReport = [pscustomobject]@{
        ok = $true
        node = "m2"
        verifyEvidence = $true
        repeatCount = 2
        project = [pscustomobject]@{
            name = "sat-a"
            team = "sat-a"
            watch = "sat-a"
            crewSha256 = $currentCrewHash
            specSha256 = "0000000000000000000000000000000000000000000000000000000000000000"
        }
        runs = @(
            [pscustomobject]@{
                iteration = 1
                terminal = "landed"
                evidenceVerified = $true
                exitCode = 0
            },
            [pscustomobject]@{
                iteration = 2
                terminal = "landed"
                evidenceVerified = $true
                exitCode = 0
            }
        )
    }
    $wrongSpecHashReport | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $acceptanceReport -Encoding utf8NoBOM
    $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $manifestPath -Node m2 -VerifyReports -RepeatCount 2 2>&1
    $childCode = $LASTEXITCODE
    if ($null -eq $childCode) {
        $childCode = 0
    }
    if ($childCode -eq 0) {
        Fail "self-test expected CLI -VerifyReports to reject an acceptance report with a stale project spec hash`n$($childOut | Out-String)"
    }
    Set-Content -LiteralPath $acceptanceReport -Encoding utf8NoBOM -Value '{"ok":true,"stale":true}'
    $fakeEvidenceFail = Join-Path $root "fake-evidence-fail.ps1"
    Set-Content -LiteralPath $fakeEvidenceFail -Encoding utf8NoBOM -Value @(
        "exit 42"
    )
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $oldEvidenceScript = $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $fakeEvidenceFail
    try {
        $scriptPath = if ([string]::IsNullOrWhiteSpace($PSCommandPath)) { Join-Path $PSScriptRoot "phase2-fleet.ps1" } else { $PSCommandPath }
        $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $manifestPath -Node m2 -Materialize -RunSelected -VerifyEvidence 2>&1
        $childCode = $LASTEXITCODE
        if ($null -eq $childCode) {
            $childCode = 0
        }
        if ($childCode -eq 0) {
            Fail "self-test expected failing evidence verifier child run to exit nonzero`n$($childOut | Out-String)"
        }
        if (Test-Path -LiteralPath $acceptanceReport -PathType Leaf) {
            Fail "self-test expected failing evidence verifier child run to remove the stale acceptance report"
        }
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) { Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_LOG = $oldFakeLog }
        if ($null -eq $oldEvidenceScript) { Remove-Item Env:\ENSEMBLE_PHASE2_EVIDENCE_SCRIPT -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $oldEvidenceScript }
    }
    $multiJson = @{
        nodes      = @("m2")
        conductor  = "m2"
        main       = @{
            repo   = $mainRepo
            test   = "echo main"
            routes = @{
                codex  = "m2"
                claude = "m2"
                agy    = "m2"
            }
        }
        satellites = @(
            @{
                name = "sat-a"
                repo = $satRepo
                node = "m2"
                test = "echo sat"
            }
        )
    } | ConvertTo-Json -Depth 6
    $multiManifestPath = Join-Path $root "fleet-multi-selected.json"
    Set-Content -LiteralPath $multiManifestPath -Value $multiJson -Encoding utf8NoBOM
    $oldNodeForMulti = $script:Node
    $script:Node = "m2"
    try {
        $multiPlan = New-FleetPlan (Read-FleetManifest $multiManifestPath) $root
    }
    finally {
        $script:Node = $oldNodeForMulti
    }
    $multiReports = @($multiPlan.Projects | Where-Object { $_.Selected } | ForEach-Object { Get-AcceptanceReportPath $_ })
    if ($multiReports.Count -ne 2) {
        Fail "self-test expected multi-selected manifest to select two projects on m2"
    }
    $oldVerifyEvidenceForReports = $script:VerifyEvidence
    $oldRepeatCountForReports = $script:RepeatCount
    $script:VerifyEvidence = $true
    $script:RepeatCount = 2
    try {
        Materialize-Plan $multiPlan
        foreach ($project in @($multiPlan.Projects | Where-Object { $_.Selected })) {
            $runs = @(
                [pscustomobject]@{
                    iteration = 1
                    command = "fake"
                    exitCode = 0
                    terminal = "landed"
                    evidenceVerified = $true
                    allowedEscalated = $false
                    teamSince = 0
                    watchSince = 0
                    controlSince = 0
                },
                [pscustomobject]@{
                    iteration = 2
                    command = "fake"
                    exitCode = 0
                    terminal = "landed"
                    evidenceVerified = $true
                    allowedEscalated = $false
                    teamSince = 0
                    watchSince = 0
                    controlSince = 0
                }
            )
            Write-AcceptanceReport $project $runs
        }
    }
    finally {
        $script:VerifyEvidence = $oldVerifyEvidenceForReports
        $script:RepeatCount = $oldRepeatCountForReports
    }
    $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $multiManifestPath -Node all -VerifyReports -RepeatCount 2 2>&1
    $childCode = $LASTEXITCODE
    if ($null -eq $childCode) {
        $childCode = 0
    }
    if ($childCode -ne 0) {
        Fail "self-test expected CLI -Node all -VerifyReports to validate all selected acceptance reports`n$($childOut | Out-String)"
    }
    foreach ($staleReport in $multiReports) {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $staleReport) | Out-Null
        Set-Content -LiteralPath $staleReport -Encoding utf8NoBOM -Value '{"ok":true,"stale":true}'
    }
    $emptyPath = Join-Path $root "empty-path"
    New-Item -ItemType Directory -Force -Path $emptyPath | Out-Null
    $pwshExe = (Get-Command pwsh -ErrorAction Stop).Source
    $oldPath = $env:PATH
    $env:PATH = $emptyPath
    try {
        $scriptPath = if ([string]::IsNullOrWhiteSpace($PSCommandPath)) { Join-Path $PSScriptRoot "phase2-fleet.ps1" } else { $PSCommandPath }
        $childOut = & $pwshExe -NoProfile -File $scriptPath -Manifest $multiManifestPath -Node m2 -Materialize -RunSelected -VerifyEvidence 2>&1
        $childCode = $LASTEXITCODE
        if ($null -eq $childCode) {
            $childCode = 0
        }
        if ($childCode -eq 0) {
            Fail "self-test expected missing ensemble child run to exit nonzero`n$($childOut | Out-String)"
        }
        foreach ($staleReport in $multiReports) {
            if (Test-Path -LiteralPath $staleReport -PathType Leaf) {
                Fail "self-test expected missing ensemble preflight to remove every selected stale acceptance report"
            }
        }
    }
    finally {
        $env:PATH = $oldPath
    }
    foreach ($staleReport in $multiReports) {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $staleReport) | Out-Null
        Set-Content -LiteralPath $staleReport -Encoding utf8NoBOM -Value '{"ok":true,"stale":true}'
    }
    if ($IsWindows) {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "@echo off",
            'if "%1"=="team" (',
            '  echo {"messages":[],"next":0}',
            '  exit /b 0',
            ')',
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            'exit /b 9'
        )
    }
    else {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "#!/bin/sh",
            'if [ "$1" = "team" ]; then printf ''{"messages":[],"next":0}\n''; exit 0; fi',
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            "exit 9"
        )
        & chmod +x $fakeExe
    }
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    try {
        $scriptPath = if ([string]::IsNullOrWhiteSpace($PSCommandPath)) { Join-Path $PSScriptRoot "phase2-fleet.ps1" } else { $PSCommandPath }
        $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $multiManifestPath -Node m2 -Materialize -RunSelected -VerifyEvidence 2>&1
        $childCode = $LASTEXITCODE
        if ($null -eq $childCode) {
            $childCode = 0
        }
        if ($childCode -eq 0) {
            Fail "self-test expected multi-selected failing run child to exit nonzero`n$($childOut | Out-String)"
        }
        foreach ($staleReport in $multiReports) {
            if (Test-Path -LiteralPath $staleReport -PathType Leaf) {
                Fail "self-test expected multi-selected failing run to remove every selected stale acceptance report"
            }
        }
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) { Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_LOG = $oldFakeLog }
    }
    foreach ($staleReport in $multiReports) {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $staleReport) | Out-Null
        Set-Content -LiteralPath $staleReport -Encoding utf8NoBOM -Value '{"ok":true,"stale":true}'
    }
    $fakeRunCount = Join-Path $root "fake-run-count.txt"
    Remove-Item -LiteralPath $fakeRunCount -ErrorAction SilentlyContinue
    if ($IsWindows) {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "@echo off",
            'if "%1"=="team" (',
            '  echo {"messages":[],"next":0}',
            '  exit /b 0',
            ')',
            'set count=0',
            'if exist "%ENSEMBLE_FAKE_RUN_COUNT%" set /p count=<"%ENSEMBLE_FAKE_RUN_COUNT%"',
            'set /a count=%count%+1',
            '> "%ENSEMBLE_FAKE_RUN_COUNT%" echo %count%',
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            'if "%count%"=="1" (',
            '  echo LANDED after 1 round(s)',
            '  exit /b 0',
            ')',
            'exit /b 9'
        )
    }
    else {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "#!/bin/sh",
            'if [ "$1" = "team" ]; then printf ''{"messages":[],"next":0}\n''; exit 0; fi',
            'count=0',
            'if [ -f "$ENSEMBLE_FAKE_RUN_COUNT" ]; then count=$(cat "$ENSEMBLE_FAKE_RUN_COUNT"); fi',
            'count=$((count + 1))',
            'printf ''%s\n'' "$count" > "$ENSEMBLE_FAKE_RUN_COUNT"',
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            'if [ "$count" = "1" ]; then printf ''LANDED after 1 round(s)\n''; exit 0; fi',
            'exit 9'
        )
        & chmod +x $fakeExe
    }
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $oldEvidenceScript = $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT
    $oldFakeRunCount = $env:ENSEMBLE_FAKE_RUN_COUNT
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $fakeEvidence
    $env:ENSEMBLE_FAKE_RUN_COUNT = $fakeRunCount
    try {
        $scriptPath = if ([string]::IsNullOrWhiteSpace($PSCommandPath)) { Join-Path $PSScriptRoot "phase2-fleet.ps1" } else { $PSCommandPath }
        $childOut = & pwsh -NoProfile -File $scriptPath -Manifest $multiManifestPath -Node m2 -Materialize -RunSelected -VerifyEvidence 2>&1
        $childCode = $LASTEXITCODE
        if ($null -eq $childCode) {
            $childCode = 0
        }
        if ($childCode -eq 0) {
            Fail "self-test expected later selected project failure child to exit nonzero`n$($childOut | Out-String)"
        }
        foreach ($staleReport in $multiReports) {
            if (Test-Path -LiteralPath $staleReport -PathType Leaf) {
                Fail "self-test expected later selected project failure to leave no acceptance reports"
            }
        }
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) { Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_LOG = $oldFakeLog }
        if ($null -eq $oldEvidenceScript) { Remove-Item Env:\ENSEMBLE_PHASE2_EVIDENCE_SCRIPT -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_PHASE2_EVIDENCE_SCRIPT = $oldEvidenceScript }
        if ($null -eq $oldFakeRunCount) { Remove-Item Env:\ENSEMBLE_FAKE_RUN_COUNT -ErrorAction SilentlyContinue } else { $env:ENSEMBLE_FAKE_RUN_COUNT = $oldFakeRunCount }
    }

    if ($IsWindows) {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "@echo off",
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            "exit /b 0"
        )
    }
    else {
        Set-Content -LiteralPath $fakeExe -Encoding ascii -Value @(
            "#!/bin/sh",
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            "exit 0"
        )
        & chmod +x $fakeExe
    }

    $script:Service = "install-print"
    $servicePlan = New-FleetPlan $fleet $root
    $serviceCommands = @($servicePlan.Commands | Where-Object { $_.Kind -eq "service" })
    if ($serviceCommands.Count -ne 1) {
        Fail "self-test expected one selected service command"
    }
    if (($serviceCommands[0].Args -join " ") -ne "serve --install-service --print") {
        Fail "self-test expected service install-print to produce serve --install-service --print"
    }
    Set-Content -LiteralPath $fakeLog -Encoding utf8NoBOM -Value ""
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    try {
        Invoke-SelectedServices $servicePlan
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) {
            Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue
        }
        else {
            $env:ENSEMBLE_FAKE_LOG = $oldFakeLog
        }
    }
    $serviceFakeOut = Get-Content -Raw -LiteralPath $fakeLog
    $serviceFakeLines = @($serviceFakeOut -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    if ($serviceFakeLines.Count -ne 1) {
        Fail "self-test expected -RunService to execute exactly one selected service command"
    }
    if ($serviceFakeLines[0] -ne "serve --install-service --print") {
        Fail "self-test expected -RunService to execute service install-print"
    }
    $fakeTailscaleName = "tailscale"
    if ($IsWindows) {
        $fakeTailscaleName = "tailscale.cmd"
    }
    $fakeTailscale = Join-Path $fakeBin $fakeTailscaleName
    if ($IsWindows) {
        Set-Content -LiteralPath $fakeTailscale -Encoding ascii -Value @(
            "@echo off",
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            "exit /b 0"
        )
    }
    else {
        Set-Content -LiteralPath $fakeTailscale -Encoding ascii -Value @(
            "#!/bin/sh",
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            "exit 0"
        )
        & chmod +x $fakeTailscale
    }
    $fakeSshName = "ssh"
    if ($IsWindows) {
        $fakeSshName = "ssh.cmd"
    }
    $fakeSsh = Join-Path $fakeBin $fakeSshName
    if ($IsWindows) {
        Set-Content -LiteralPath $fakeSsh -Encoding ascii -Value @(
            "@echo off",
            'echo %*>>"%ENSEMBLE_FAKE_LOG%"',
            "exit /b 0"
        )
    }
    else {
        Set-Content -LiteralPath $fakeSsh -Encoding ascii -Value @(
            "#!/bin/sh",
            'printf ''%s\n'' "$*" >> "$ENSEMBLE_FAKE_LOG"',
            "exit 0"
        )
        & chmod +x $fakeSsh
    }
    $oldNodeForRemoteService = $script:Node
    $oldRemoteService = $script:RemoteService
    $script:Node = "all"
    $script:RemoteService = $true
    $remoteServicePlan = New-FleetPlan $fleet $root
    Set-Content -LiteralPath $fakeLog -Encoding utf8NoBOM -Value ""
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    try {
        Invoke-SelectedServices $remoteServicePlan
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) {
            Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue
        }
        else {
            $env:ENSEMBLE_FAKE_LOG = $oldFakeLog
        }
        $script:Node = $oldNodeForRemoteService
        $script:RemoteService = $oldRemoteService
    }
    $remoteServiceFakeOut = Get-Content -Raw -LiteralPath $fakeLog
    $remoteServiceFakeLines = @($remoteServiceFakeOut -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    if ($remoteServiceFakeLines.Count -ne 2) {
        Fail "self-test expected remote -RunService -Node all to execute the local conductor plus one remote command per peer"
    }
    if ($remoteServiceFakeLines[0] -ne "serve --install-service --print" -or $remoteServiceFakeLines[1] -ne "ssh m2 ensemble serve --install-service --print") {
        Fail "self-test expected remote service bootstrap to run conductor locally and peer through tailscale ssh"
    }
    $oldNodeForRemoteService = $script:Node
    $oldRemoteService = $script:RemoteService
    $oldRemoteServiceTransport = $script:RemoteServiceTransport
    $script:Node = "all"
    $script:RemoteService = $true
    $script:RemoteServiceTransport = "ssh"
    Set-Content -LiteralPath $fakeLog -Encoding utf8NoBOM -Value ""
    $oldPath = $env:PATH
    $oldFakeLog = $env:ENSEMBLE_FAKE_LOG
    $env:PATH = "$fakeBin$([System.IO.Path]::PathSeparator)$oldPath"
    $env:ENSEMBLE_FAKE_LOG = $fakeLog
    try {
        Invoke-SelectedServices $remoteServicePlan
    }
    finally {
        $env:PATH = $oldPath
        if ($null -eq $oldFakeLog) {
            Remove-Item Env:\ENSEMBLE_FAKE_LOG -ErrorAction SilentlyContinue
        }
        else {
            $env:ENSEMBLE_FAKE_LOG = $oldFakeLog
        }
        $script:Node = $oldNodeForRemoteService
        $script:RemoteService = $oldRemoteService
        $script:RemoteServiceTransport = $oldRemoteServiceTransport
    }
    $remoteSshServiceFakeOut = Get-Content -Raw -LiteralPath $fakeLog
    $remoteSshServiceFakeLines = @($remoteSshServiceFakeOut -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    if ($remoteSshServiceFakeLines.Count -ne 2) {
        Fail "self-test expected ssh remote service transport to execute the local conductor plus one remote command per peer"
    }
    if ($remoteSshServiceFakeLines[0] -ne "serve --install-service --print" -or $remoteSshServiceFakeLines[1] -ne "-o BatchMode=yes -o ConnectTimeout=10 m2 ensemble serve --install-service --print") {
        Fail "self-test expected ssh remote service transport to use non-interactive OpenSSH options"
    }
    $script:Service = "none"

    Write-Host "phase2-fleet self-test passed"
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

$manifestPath = Resolve-ManifestPath $Manifest
if ($InitSample) {
    Write-SampleManifest $manifestPath
    exit 0
}

$manifestDir = Split-Path -Parent $manifestPath
$fleet = Read-FleetManifest $manifestPath
$plan = New-FleetPlan $fleet $manifestDir
$serviceAction = Normalize-ServiceAction $Service

if ($AllowEscalatedRun -and -not $VerifyEvidence) {
    Fail "-AllowEscalatedRun requires -VerifyEvidence"
}
if (($RequireControlEvidence -or $RequireSteerEvidence -or $RequireAbortEvidence) -and -not $VerifyEvidence) {
    Fail "-RequireControlEvidence/-RequireSteerEvidence/-RequireAbortEvidence require -VerifyEvidence"
}
if ($RepeatCount -lt 1) {
    Fail "-RepeatCount must be >= 1"
}
if ($RunSelected -and $PlanOnly) {
    Fail "-RunSelected cannot be combined with -PlanOnly"
}
if ($VerifyReports -and $PlanOnly) {
    Fail "-VerifyReports cannot be combined with -PlanOnly"
}
if ($RunService -and $PlanOnly) {
    Fail "-RunService cannot be combined with -PlanOnly"
}
if ($RemoteService -and -not $RunService) {
    Fail "-RemoteService requires -RunService"
}
if ($RemoteService -and $serviceAction -eq "up") {
    Fail "-RemoteService does not support foreground -Service up; use install-print, install, uninstall-print, or uninstall"
}
if ($RunService -and $serviceAction -eq "none") {
    Fail "-RunService requires an explicit -Service action (install-print, install, uninstall-print, uninstall, or up)"
}
if ($RunSelected -and $Node.Equals("all", [System.StringComparison]::OrdinalIgnoreCase)) {
    Fail "-RunSelected requires an explicit -Node <name>; refusing to run every manifest task"
}
if ($RunService -and -not $RemoteService -and $Node.Equals("all", [System.StringComparison]::OrdinalIgnoreCase)) {
    Fail "-RunService requires an explicit -Node <name>; refusing to run every manifest service command on this host"
}
if ($Materialize) {
    Materialize-Plan $plan
}
elseif ($RunSelected) {
    Materialize-Plan $plan
}
if ($CheckNodes) {
    Check-FleetNodes $plan
}
if ($RunService) {
    Invoke-SelectedServices $plan
}
if ($RunSelected) {
    Invoke-SelectedRuns $plan
}
if ($VerifyReports) {
    Invoke-AcceptanceReportVerifier $plan
}
if ($PlanOnly -or (-not $Materialize -and -not $CheckNodes -and -not $RunSelected -and -not $VerifyReports -and -not $RunService)) {
    if ($Json) {
        Write-PlanJson $plan
    } else {
        Write-Plan $plan
    }
}
