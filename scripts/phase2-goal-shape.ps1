#!/usr/bin/env pwsh
# Validate that a Phase-2 fleet manifest matches the goal shape:
# one five-node main fleet plus four codex+claude satellite projects.

param(
    [string]$Manifest = "phase2-fleet.local.json",
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$script:ExpectingFailure = $false

function Fail([string]$Message) {
    if ($script:ExpectingFailure) {
        throw $Message
    }
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

function Resolve-ManifestPath([string]$Path) {
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
}

function Get-ExpectedHost([string]$NodeName) {
    $trimmed = $NodeName.Trim().TrimEnd("/")
    $match = [regex]::Match($trimmed, "^(?:https?://)?(\[[^\]]+\]|[^/:\s]+)(?::\d+)?(?:/.*)?$")
    if ($match.Success) {
        return $match.Groups[1].Value.Trim("[", "]")
    }
    return $trimmed.Trim("[", "]")
}

function Test-NodeKnown([string]$Value, [string[]]$Nodes) {
    $expected = Get-ExpectedHost $Value
    foreach ($node in $Nodes) {
        $known = Get-ExpectedHost $node
        if ($expected.Equals($known, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
        if ($expected.StartsWith("$known.", [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
        if ($known.StartsWith("$expected.", [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Assert-UniqueValues([string[]]$Values, [string]$Label) {
    $seen = @{}
    foreach ($value in $Values) {
        $key = $value.ToLowerInvariant()
        if ($seen.ContainsKey($key)) {
            Fail "Phase 2 goal manifest has duplicate ${Label}: $value"
        }
        $seen[$key] = $true
    }
}

function Test-NodeSame([string]$Left, [string]$Right) {
    $a = Get-ExpectedHost $Left
    $b = Get-ExpectedHost $Right
    if ($a.Equals($b, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    if ($a.StartsWith("$b.", [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    if ($b.StartsWith("$a.", [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    return $false
}

function Assert-NodeAllowed([string]$Value, [string[]]$ForbiddenNodes, [string]$Label) {
    foreach ($forbidden in $ForbiddenNodes) {
        if (Test-NodeSame $Value $forbidden) {
            Fail "Phase 2 goal manifest uses forbidden node '$Value' at $Label"
        }
    }
}

function Assert-Phase2GoalShape($Fleet) {
    $nodes = @((Get-Prop $Fleet "nodes" @()) | ForEach-Object { [string]$_ })
    $forbiddenNodes = @((Get-Prop $Fleet "forbidden_nodes" @()) | ForEach-Object { [string]$_ } | Where-Object { $_.Trim().Length -gt 0 })
    if ($nodes.Count -ne 5) {
        Fail "Phase 2 goal requires exactly 5 fleet nodes; manifest has $($nodes.Count)"
    }
    Assert-UniqueValues $nodes "node"
    Assert-UniqueValues $forbiddenNodes "forbidden node"
    foreach ($node in $nodes) {
        Assert-NodeAllowed $node $forbiddenNodes "nodes"
    }

    $conductor = [string](Get-Prop $Fleet "conductor" $nodes[0])
    Assert-NodeAllowed $conductor $forbiddenNodes "conductor"
    if (-not (Test-NodeKnown $conductor $nodes)) {
        Fail "Phase 2 goal conductor '$conductor' is not listed in manifest.nodes"
    }

    $main = Get-Prop $Fleet "main" $null
    if ($null -eq $main) {
        Fail "Phase 2 goal manifest is missing main project"
    }
    Require-Prop $main "repo" "main" | Out-Null
    $routes = Get-Prop $main "routes" $null
    if ($null -eq $routes) {
        Fail "Phase 2 goal manifest main.routes is required"
    }
    foreach ($agent in @("codex", "claude", "agy")) {
        $route = Require-Prop $routes $agent "main.routes"
        Assert-NodeAllowed $route $forbiddenNodes "main.routes.$agent"
        if (-not (Test-NodeKnown $route $nodes)) {
            Fail "Phase 2 goal main route '$agent=$route' does not point at a manifest node"
        }
    }

    $satellites = @(Get-Prop $Fleet "satellites" @())
    if ($satellites.Count -ne 4) {
        Fail "Phase 2 goal requires exactly 4 satellite projects; manifest has $($satellites.Count)"
    }

    $satNames = New-Object System.Collections.Generic.List[string]
    $satTeams = New-Object System.Collections.Generic.List[string]
    $satWatches = New-Object System.Collections.Generic.List[string]
    foreach ($sat in $satellites) {
        $satName = Require-Prop $sat "name" "satellites[]"
        Require-Prop $sat "repo" "satellites[$satName]" | Out-Null
        $satNode = Require-Prop $sat "node" "satellites[$satName]"
        Assert-NodeAllowed $satNode $forbiddenNodes "satellites[$satName].node"
        if (-not (Test-NodeKnown $satNode $nodes)) {
            Fail "Phase 2 goal satellite '$satName' node '$satNode' is not listed in manifest.nodes"
        }
        $satTeam = [string](Get-Prop $sat "team" $satName)
        $satWatch = [string](Get-Prop $sat "watch" $satTeam)
        $satNames.Add($satName)
        $satTeams.Add($satTeam)
        $satWatches.Add($satWatch)
    }
    Assert-UniqueValues @($satNames) "satellite name"
    Assert-UniqueValues @($satTeams) "satellite team"
    Assert-UniqueValues @($satWatches) "satellite watch"

    Write-Host "Phase 2 goal shape passed: 5 nodes, 1 main project, 4 satellites, codex/claude/agy main routes." -ForegroundColor Green
}

function Read-FleetManifest([string]$Path) {
    $manifestPath = Resolve-ManifestPath $Path
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        Fail "manifest not found: $manifestPath"
    }
    try {
        return Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    }
    catch {
        Fail "manifest is not valid JSON: $manifestPath. $($_.Exception.Message)"
    }
}

function Expect-Fail([string]$Title, [scriptblock]$Body) {
    $oldPreference = $ErrorActionPreference
    $ErrorActionPreference = "Stop"
    $script:ExpectingFailure = $true
    try {
        & $Body
    }
    catch {
        return
    }
    finally {
        $script:ExpectingFailure = $false
        $ErrorActionPreference = $oldPreference
    }
    Fail "self-test expected failure: $Title"
}

function Invoke-SelfTest {
    $good = @{
        nodes      = @("m1", "m2", "m3", "m4", "m5")
        conductor  = "m1"
        main       = @{
            repo   = "D:\Projects\main"
            routes = @{
                codex  = "m1"
                claude = "http://m2:7878"
                agy    = "m3.tail.example"
            }
        }
        satellites = @(
            @{ name = "sat-a"; repo = "D:\Projects\sat-a"; node = "m2" },
            @{ name = "sat-b"; repo = "D:\Projects\sat-b"; node = "m3" },
            @{ name = "sat-c"; repo = "D:\Projects\sat-c"; node = "m4" },
            @{ name = "sat-d"; repo = "D:\Projects\sat-d"; node = "m5" }
        )
    } | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    Assert-Phase2GoalShape $good

    $tooFewNodes = @{
        nodes      = @("m1", "m2")
        conductor  = "m1"
        main       = @{ repo = "x"; routes = @{ codex = "m1"; claude = "m2"; agy = "m2" } }
        satellites = @()
    } | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    Expect-Fail "too few nodes" { Assert-Phase2GoalShape $tooFewNodes }

    $badRoute = @{
        nodes      = @("m1", "m2", "m3", "m4", "m5")
        conductor  = "m1"
        main       = @{ repo = "x"; routes = @{ codex = "m1"; claude = "missing"; agy = "m3" } }
        satellites = @(
            @{ name = "sat-a"; repo = "a"; node = "m2" },
            @{ name = "sat-b"; repo = "b"; node = "m3" },
            @{ name = "sat-c"; repo = "c"; node = "m4" },
            @{ name = "sat-d"; repo = "d"; node = "m5" }
        )
    } | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    Expect-Fail "unknown main route" { Assert-Phase2GoalShape $badRoute }

    $duplicateSatellite = @{
        nodes      = @("m1", "m2", "m3", "m4", "m5")
        conductor  = "m1"
        main       = @{ repo = "x"; routes = @{ codex = "m1"; claude = "m2"; agy = "m3" } }
        satellites = @(
            @{ name = "sat-a"; repo = "a"; node = "m2" },
            @{ name = "sat-a"; repo = "b"; node = "m3" },
            @{ name = "sat-c"; repo = "c"; node = "m4" },
            @{ name = "sat-d"; repo = "d"; node = "m5" }
        )
    } | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    Expect-Fail "duplicate satellite" { Assert-Phase2GoalShape $duplicateSatellite }

    $forbiddenNode = @{
        nodes           = @("m1", "m2", "m3", "m4", "m5")
        forbidden_nodes = @("m3")
        conductor       = "m1"
        main            = @{ repo = "x"; routes = @{ codex = "m1"; claude = "m2"; agy = "m3" } }
        satellites      = @(
            @{ name = "sat-a"; repo = "a"; node = "m2" },
            @{ name = "sat-b"; repo = "b"; node = "m3" },
            @{ name = "sat-c"; repo = "c"; node = "m4" },
            @{ name = "sat-d"; repo = "d"; node = "m5" }
        )
    } | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    Expect-Fail "forbidden node used" { Assert-Phase2GoalShape $forbiddenNode }

    Write-Host "phase2-goal-shape self-test passed" -ForegroundColor Green
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

Assert-Phase2GoalShape (Read-FleetManifest $Manifest)
