#!/usr/bin/env pwsh
# Deterministic single-machine Phase 1 acceptance.
#
# This script validates the local control plane without relying on a live AI UI to
# click through prompts. It creates a scratch git repo, builds or reuses a release
# ensemble.exe, then checks:
#   - doctor/readiness
#   - team status/say/inbox
#   - auto-generated member names and launcher MCP config previews
#   - controlled launcher path through a fake codex shim
#   - controlled agy launch path via `agy --help`
#   - bounded agy wrapper result/flake visibility
#   - real `ensemble mcp` stdio tools/list + team/control calls
#   - direct watch/steer/abort CLI surfaces
#
# For a real governed Codex/Claude run, add -RunFullSmoke.

param(
    [string]$SmokeRoot = "",
    [string]$TargetDir = "",
    [string]$Team = "phase1-auto",
    [string]$Watch = "phase1-auto-run",
    [int]$AgyTimeoutSecs = 1,
    [switch]$NoBuild,
    [switch]$SkipAgy,
    [switch]$RunFullSmoke
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

function Read-TextFile([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        return Get-Content -LiteralPath $Path -Raw
    }
    return ""
}

function Write-AsciiFile([string]$Path, [string]$Content) {
    $Content | Set-Content -LiteralPath $Path -Encoding ascii
}

function Write-Utf8File([string]$Path, [string]$Content) {
    $Content | Set-Content -LiteralPath $Path -Encoding utf8
}

function Assert-Contains([string]$Title, [string]$Text, [string]$Needle) {
    if (-not $Text.Contains($Needle)) {
        Fail "$Title did not contain expected text: $Needle"
    }
}

function Convert-JsonOrFail([string]$Title, [string]$JsonText) {
    try {
        return $JsonText | ConvertFrom-Json
    } catch {
        Fail "$Title did not return parseable JSON: $($_.Exception.Message)`n$JsonText"
    }
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
        Combined = "$stdout`n$stderr"
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
    }
}

function Json-Line($Object) {
    return ($Object | ConvertTo-Json -Compress -Depth 32)
}

function Invoke-McpStdio(
    [string]$Title,
    [string[]]$Lines,
    [string[]]$CommandArgs,
    [string]$ArtifactName
) {
    Write-Host "== $Title ==" -ForegroundColor Cyan
    $inputPath = Join-Path $ArtifactDir "$ArtifactName.input.ndjson"
    $stdoutPath = Join-Path $ArtifactDir "$ArtifactName.out.ndjson"
    $stderrPath = Join-Path $ArtifactDir "$ArtifactName.err"
    $Lines | Set-Content -LiteralPath $inputPath -Encoding ascii
    Get-Content -LiteralPath $inputPath | & $exe @CommandArgs > $stdoutPath 2> $stderrPath
    $code = $LASTEXITCODE
    $stdout = Read-TextFile $stdoutPath
    $stderr = Read-TextFile $stderrPath
    if (-not [string]::IsNullOrWhiteSpace($stdout)) {
        Write-Host $stdout.TrimEnd()
    }
    if (-not [string]::IsNullOrWhiteSpace($stderr)) {
        Write-Host $stderr.TrimEnd() -ForegroundColor DarkYellow
    }
    if ($code -ne 0) {
        Fail "$Title failed with exit $code. See $stdoutPath and $stderrPath"
    }
    $responses = @{}
    foreach ($line in @(Get-Content -LiteralPath $stdoutPath | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) {
        try {
            $obj = $line | ConvertFrom-Json
        } catch {
            Fail "$Title emitted non-JSON line: $line"
        }
        if ($null -ne $obj.id) {
            $responses["$($obj.id)"] = $obj
        }
    }
    return [pscustomobject]@{
        Responses = $responses
        Stdout = $stdout
        Stderr = $stderr
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
    }
}

function Assert-McpOk([hashtable]$Responses, [int]$Id) {
    $key = "$Id"
    if (-not $Responses.ContainsKey($key)) {
        Fail "MCP response id $Id was missing"
    }
    $resp = $Responses[$key]
    $errorProp = $resp.PSObject.Properties["error"]
    if (($null -ne $errorProp) -and ($null -ne $errorProp.Value)) {
        Fail "MCP response id $Id returned error: $($errorProp.Value | ConvertTo-Json -Compress)"
    }
    $resultProp = $resp.PSObject.Properties["result"]
    if ($null -eq $resultProp) {
        Fail "MCP response id $Id did not include a result"
    }
    return $resultProp.Value
}

$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    if ($NoBuild) {
        $TargetDir = Join-Path $repo "target"
    } elseif (Test-Path "D:\tmp") {
        $TargetDir = Join-Path "D:\tmp" "ensemble-acceptance-target-$PID"
    } else {
        $TargetDir = Join-Path $env:TEMP "ensemble-acceptance-target-$PID"
    }
}
$exe = Join-Path $TargetDir "release\ensemble.exe"

Require-Tool git
if (-not $NoBuild) {
    Require-Tool cargo
    Write-Host "== building release ensemble.exe ==" -ForegroundColor Cyan
    Push-Location $repo
    try {
        $env:CARGO_TARGET_DIR = $TargetDir
        cargo build --release --bin ensemble
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    } finally {
        Pop-Location
    }
}
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    Fail "ensemble.exe not found at $exe; run without -NoBuild first"
}

if ([string]::IsNullOrWhiteSpace($SmokeRoot)) {
    if (Test-Path "D:\tmp") {
        $SmokeRoot = Join-Path "D:\tmp" "ensemble-acceptance-single-machine"
    } else {
        $SmokeRoot = Join-Path $env:TEMP "ensemble-acceptance-single-machine"
    }
}
if (Test-Path -LiteralPath $SmokeRoot) {
    Remove-Item -LiteralPath $SmokeRoot -Recurse -Force
}
New-Item -ItemType Directory -Force $SmokeRoot | Out-Null
$ArtifactDir = Join-Path $SmokeRoot "acceptance-artifacts"
New-Item -ItemType Directory -Force $ArtifactDir | Out-Null

Write-Host "  exe: $exe" -ForegroundColor DarkGray
Write-Host "  target: $TargetDir" -ForegroundColor DarkGray
Write-Host "  repo: $SmokeRoot" -ForegroundColor DarkGray
Write-Host "  team: $Team" -ForegroundColor DarkGray
Write-Host "  watch: $Watch" -ForegroundColor DarkGray
Write-Host "  artifacts: $ArtifactDir" -ForegroundColor DarkGray
Write-Host ""

Push-Location $SmokeRoot
try {
    Write-Host "== initializing scratch repo ==" -ForegroundColor Cyan
    git init -q
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    git config user.email "single-machine-acceptance@example.invalid"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    git config user.name "ensemble acceptance"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    git config core.autocrlf false
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-AsciiFile ".gitignore" ".ensemble/`nacceptance-artifacts/`ncrew.toml`n"
    Write-AsciiFile "README.md" "# ensemble single-machine acceptance`n"
    git add .gitignore README.md
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    git commit -q -m init
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    git branch -M main
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-AsciiFile "crew.toml" @"
pipeline = ["implement", "review"]

[gate]
min_approvals = 1
max_rounds = 1
on_flake = "exclude"
stall_limit = 1
max_task_secs = 120

[roles.implement]
agent = "codex"

[roles.review]
agent = "claude"
blind = true

[agents.codex]
timeout = 60

[agents.claude]
timeout = 60
"@

    Invoke-EnsembleCapture "doctor" @("doctor") "doctor.txt" | Out-Null

    Invoke-EnsembleCapture `
        "team say" `
        @("team", "say", "operator: automated acceptance started", "--repo", ".", "--team", $Team, "--from", "operator") `
        "team-say.txt" | Out-Null

    $status = Invoke-EnsembleCapture `
        "team status json" `
        @("team", "status", "--repo", ".", "--team", $Team, "--json") `
        "team-status.initial.json"
    $statusJson = Convert-JsonOrFail "team status json" $status.Stdout
    if ($statusJson.team -ne $Team) {
        Fail "team status reported '$($statusJson.team)', expected '$Team'"
    }

    $inbox = Invoke-EnsembleCapture `
        "team inbox json" `
        @("team", "inbox", "--repo", ".", "--team", $Team, "--since", "0", "--json") `
        "team-inbox.initial.json"
    $inboxJson = Convert-JsonOrFail "team inbox json" $inbox.Stdout
    $operatorPosts = @($inboxJson.messages | Where-Object {
        $_.from -eq "operator" -and $_.body -eq "operator: automated acceptance started"
    })
    if ($operatorPosts.Count -lt 1) {
        Fail "operator team post was not visible in team inbox"
    }

    Write-Host "== launcher config previews ==" -ForegroundColor Cyan
    foreach ($client in @("codex", "claude", "opencode")) {
        $launcherArgs = @("--repo", ".", "--team", $Team, "--confirm-policy", "ask", "--print-config", $client, "--vendor-smoke")
        if ($client -eq "codex") {
            $codexHome = Join-Path $SmokeRoot ".codex-acceptance"
            New-Item -ItemType Directory -Force $codexHome | Out-Null
            $preview = Invoke-WithProcessEnv "CODEX_HOME" $codexHome {
                Invoke-EnsembleCapture "launcher $client" $launcherArgs "launcher-$client.txt"
            }
        } else {
            $preview = Invoke-EnsembleCapture "launcher $client" $launcherArgs "launcher-$client.txt"
        }
        Assert-Contains "launcher $client" $preview.Stdout "client=$client"
        Assert-Contains "launcher $client" $preview.Stdout "member=$client@"
        Assert-Contains "launcher $client" $preview.Stdout "team=$Team"
        Assert-Contains "launcher $client" $preview.Stdout "--team"
        Assert-Contains "launcher $client" $preview.Stdout "--vendor-smoke"
    }

    Write-Host "== controlled launcher smoke ==" -ForegroundColor Cyan
    $fakeCodexHome = Join-Path $ArtifactDir "fake-codex-home"
    New-Item -ItemType Directory -Force $fakeCodexHome | Out-Null
    $fakeCodexCmd = Join-Path $SmokeRoot "codex.cmd"
    Write-AsciiFile $fakeCodexCmd "@echo off`necho FAKE_CODEX_ARGS:%*`nexit /b 0`n"
    try {
        Invoke-WithProcessEnv "CODEX_HOME" $fakeCodexHome {
            $fakeCodex = Invoke-EnsembleCapture `
                "controlled codex shim" `
                @("--repo", ".", "--team", $Team, "--member", "codex@fake", "--confirm-policy", "ask", "codex", "--fake-arg") `
                "controlled-codex-shim.txt"
            Assert-Contains "controlled codex shim" $fakeCodex.Combined "ensemble: launching controlled ``codex`` as ``codex@fake``"
            Assert-Contains "controlled codex shim" $fakeCodex.Combined "FAKE_CODEX_ARGS:--fake-arg"
        }
    } finally {
        Remove-Item -LiteralPath $fakeCodexCmd -Force -ErrorAction SilentlyContinue
    }

    if (-not $SkipAgy) {
        if (-not (Get-Command agy -ErrorAction SilentlyContinue)) {
            Fail "agy is not on PATH; pass -SkipAgy to skip direct agy checks"
        }
        $agyDirect = Invoke-EnsembleCapture `
            "agy direct launch path" `
            @("--repo", ".", "--team", $Team, "--member", "agy@auto", "--confirm-policy", "ask", "agy", "--help") `
            "agy-direct-help.txt"
        Assert-Contains "agy direct launch path" $agyDirect.Combined "ensemble: launching controlled ``agy`` as ``agy@auto``"
        Assert-Contains "agy direct launch path" $agyDirect.Combined "Usage of agy"

        $agyTurn = Invoke-EnsembleCapture `
            "agy bounded wrapper" `
            @("--repo", ".", "--team", $Team, "--member", "agy@auto-prompt", "--timeout", "$AgyTimeoutSecs", "--json", "agy", "--prompt", "Read the team board and reply with one line.") `
            "agy-wrapper.json" `
            -AllowFailure
        if (-not [string]::IsNullOrWhiteSpace($agyTurn.Stdout)) {
            Convert-JsonOrFail "agy bounded wrapper" $agyTurn.Stdout | Out-Null
        }
        $afterAgy = Invoke-EnsembleCapture `
            "team inbox after agy" `
            @("team", "inbox", "--repo", ".", "--team", $Team, "--since", "0", "--json") `
            "team-inbox.after-agy.json"
        $afterAgyJson = Convert-JsonOrFail "team inbox after agy" $afterAgy.Stdout
        $agyPosts = @($afterAgyJson.messages | Where-Object {
            $_.from -eq "agy@auto-prompt" -and ($_.kind -eq "result" -or $_.kind -eq "flake")
        })
        if ($agyPosts.Count -lt 1) {
            Fail "agy bounded wrapper did not write a visible result or flake"
        }
    }

    $streamDir = Join-Path ".ensemble" "stream"
    New-Item -ItemType Directory -Force $streamDir | Out-Null
    Write-AsciiFile (Join-Path $streamDir "$Watch.ndjson") '{"from":"codex@auto","kind":"result","body":"stream smoke"}'
    $watchOut = Invoke-EnsembleCapture `
        "watch stream" `
        @("watch", $Watch, "--repo", ".") `
        "watch.txt"
    Assert-Contains "watch stream" $watchOut.Stdout "stream smoke"

    Invoke-EnsembleCapture `
        "direct steer" `
        @("steer", "cli-control", "operator redirect", "--repo", ".") `
        "direct-steer.txt" | Out-Null
    Invoke-EnsembleCapture `
        "direct abort" `
        @("abort", "cli-control", "--hard", "--repo", ".") `
        "direct-abort.txt" | Out-Null
    $directControl = Read-TextFile (Join-Path ".ensemble" "control\cli-control.ndjson")
    Assert-Contains "direct control feed" $directControl '"cmd":"steer"'
    Assert-Contains "direct control feed" $directControl '"cmd":"abort"'

    $mcpLines = @(
        (Json-Line @{ jsonrpc = "2.0"; id = 1; method = "initialize"; params = @{ protocolVersion = "2025-06-18" } })
        (Json-Line @{ jsonrpc = "2.0"; id = 2; method = "tools/list" })
        (Json-Line @{ jsonrpc = "2.0"; id = 3; method = "tools/call"; params = @{ name = "ensemble_team_say"; arguments = @{ kind = "note"; body = "mcp automated hello" } } })
        (Json-Line @{ jsonrpc = "2.0"; id = 4; method = "tools/call"; params = @{ name = "ensemble_team_inbox"; arguments = @{ since = 0 } } })
        (Json-Line @{ jsonrpc = "2.0"; id = 5; method = "tools/call"; params = @{ name = "ensemble_steer"; arguments = @{ name = "mcp-control"; prompt = "stay focused" } } })
        (Json-Line @{ jsonrpc = "2.0"; id = 6; method = "tools/call"; params = @{ name = "ensemble_abort"; arguments = @{ name = "mcp-control"; hard = $true } } })
        (Json-Line @{ jsonrpc = "2.0"; id = 7; method = "tools/call"; params = @{ name = "ensemble_watch"; arguments = @{ name = $Watch; since = 0; limit = 10 } } })
    )
    $mcp = Invoke-McpStdio `
        "mcp stdio team/control tools" `
        $mcpLines `
        @("mcp", "--repo", ".", "--team", $Team, "--name", "mcp@auto", "--crew", "crew.toml") `
        "mcp-stdio"

    Assert-McpOk $mcp.Responses 1 | Out-Null
    $tools = Assert-McpOk $mcp.Responses 2
    $toolNames = @($tools.tools | ForEach-Object { $_.name })
    foreach ($tool in @("ensemble_team_status", "ensemble_team_say", "ensemble_team_inbox", "ensemble_watch", "ensemble_steer", "ensemble_abort", "ensemble_supervise")) {
        if ($toolNames -notcontains $tool) {
            Fail "tools/list did not advertise $tool"
        }
    }
    Assert-McpOk $mcp.Responses 3 | Out-Null
    Assert-McpOk $mcp.Responses 4 | Out-Null
    Assert-McpOk $mcp.Responses 5 | Out-Null
    Assert-McpOk $mcp.Responses 6 | Out-Null
    $mcpWatch = Assert-McpOk $mcp.Responses 7
    if (($mcpWatch | ConvertTo-Json -Compress -Depth 16) -notlike "*stream smoke*") {
        Fail "ensemble_watch did not return the seeded stream event"
    }

    $afterMcp = Invoke-EnsembleCapture `
        "team inbox after mcp" `
        @("team", "inbox", "--repo", ".", "--team", $Team, "--since", "0", "--json") `
        "team-inbox.after-mcp.json"
    $afterMcpJson = Convert-JsonOrFail "team inbox after mcp" $afterMcp.Stdout
    $mcpPosts = @($afterMcpJson.messages | Where-Object {
        $_.from -eq "mcp@auto" -and $_.body -eq "mcp automated hello"
    })
    if ($mcpPosts.Count -lt 1) {
        Fail "MCP team_say post from mcp@auto was not visible in team inbox"
    }

    $mcpControl = Read-TextFile (Join-Path ".ensemble" "control\mcp-control.ndjson")
    Assert-Contains "mcp control feed" $mcpControl '"cmd":"steer"'
    Assert-Contains "mcp control feed" $mcpControl '"cmd":"abort"'

    $finalStatus = git status --short
    if ($LASTEXITCODE -ne 0) {
        Fail "git status failed with exit $LASTEXITCODE"
    }
    Write-Utf8File (Join-Path $ArtifactDir "git-status.final.txt") (($finalStatus | Out-String).TrimEnd())
    if ($finalStatus) {
        Fail "acceptance scratch repo is dirty:`n$finalStatus"
    }
} finally {
    Pop-Location
}

if ($RunFullSmoke) {
    Write-Host "== running full governed smoke ==" -ForegroundColor Cyan
    $fullSmokeRoot = Join-Path $SmokeRoot "full-smoke"
    Push-Location $repo
    try {
        pwsh -NoProfile -File scripts\smoke.ps1 -NoBuild -SmokeRoot $fullSmokeRoot -TargetDir $TargetDir
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    } finally {
        Pop-Location
    }
}

Write-Host ""
Write-Host "== single-machine automated acceptance passed ==" -ForegroundColor Green
Write-Host "  repo kept for inspection: $SmokeRoot" -ForegroundColor DarkGray
Write-Host "  artifacts: $ArtifactDir" -ForegroundColor DarkGray
