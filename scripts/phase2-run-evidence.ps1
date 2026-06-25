#!/usr/bin/env pwsh
# Verify evidence left by one Phase-2 run after it has finished.
#
# Intended use after a main or satellite run:
#   pwsh scripts\phase2-run-evidence.ps1 -Repo <repo> -Team main -Watch main -TeamSince <n> -WatchSince <n>
#
# It reads the team board, watch stream, and optional control feed. It does not
# start agents, steer, abort, merge, or mutate repo state. For real run evidence, capture
# independent cursors before the run and pass -TeamSince, -WatchSince, and when
# control evidence is required, -ControlSince. The legacy -Since flag is intentionally
# not accepted for real evidence because the feeds have independent cursor spaces.

param(
    [string]$Repo = ".",
    [string]$Team = "main",
    [string]$Watch = "main",
    [ValidateSet("any", "landed", "escalated")]
    [string]$ExpectTerminal = "any",
    [int]$Since = -1,
    [int]$TeamSince = -1,
    [int]$WatchSince = -1,
    [int]$ControlSince = -1,
    [string]$TargetDir = "",
    [switch]$RequireControl,
    [switch]$RequireSteer,
    [switch]$RequireAbort,
    [switch]$NoBuild,
    [switch]$SelfTest,
    [switch]$SelfTestMismatch,
    [switch]$SelfTestBadControl,
    [switch]$SelfTestBadControlShape,
    [switch]$SelfTestBadTerminalPrefix,
    [switch]$SelfTestBadTerminalCase
)

$ScriptBoundParameters = @{} + $PSBoundParameters
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Host "FAIL: $Message" -ForegroundColor Red
    exit 1
}

function Test-IsSelfTestMode() {
    return [bool]($SelfTest -or $SelfTestMismatch -or $SelfTestBadControl -or $SelfTestBadControlShape -or $SelfTestBadTerminalPrefix -or $SelfTestBadTerminalCase)
}

function Reject-LegacySinceForRealEvidence() {
    if (-not (Test-IsSelfTestMode) -and $script:ScriptBoundParameters.ContainsKey("Since")) {
        Fail "single -Since is ambiguous for run evidence; pass -TeamSince and -WatchSince separately"
    }
}
function Resolve-RequiredCursor([string]$Name, [int]$Value) {
    if ($Value -ge 0) {
        return $Value
    }
    if (Test-IsSelfTestMode) {
        return 0
    }
    Fail "missing -$Name; capture this feed's cursor before the run and pass it here"
}

function Resolve-ControlCursor([bool]$Required) {
    if ($ControlSince -ge 0) {
        return $ControlSince
    }
    if (Test-IsSelfTestMode) {
        return 0
    }
    if ($Required) {
        Fail "missing -ControlSince; capture the control feed line count before the run when requiring control evidence"
    }
    return 0
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
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return [System.IO.Path]::GetFullPath($candidate)
        }
    }
    $cmd = Get-Command ensemble -ErrorAction SilentlyContinue
    if ($cmd) {
        return [System.IO.Path]::GetFullPath($cmd.Source)
    }
    if ($NoBuild) {
        Fail "ensemble.exe not found in -TargetDir or PATH, and -NoBuild was set"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail "cargo not found and ensemble.exe is unavailable"
    }
    $buildTarget = $MaybeTargetDir
    if ([string]::IsNullOrWhiteSpace($buildTarget)) {
        $buildTarget = Join-Path ([System.IO.Path]::GetTempPath()) "ensemble-phase2-evidence-target"
    }
    & cargo build --bin ensemble --target-dir $buildTarget
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build failed with exit $LASTEXITCODE"
    }
    $built = Join-Path ([System.IO.Path]::GetFullPath($buildTarget)) "debug\ensemble.exe"
    if (-not (Test-Path -LiteralPath $built -PathType Leaf)) {
        Fail "built ensemble.exe not found: $built"
    }
    return $built
}

function Parse-JsonOrFail([string]$Title, [string]$Text) {
    try {
        return $Text | ConvertFrom-Json
    } catch {
        Fail "$Title did not return valid JSON: $($_.Exception.Message)`n$Text"
    }
}

function Classify-Terminal([string]$Body) {
    if ($Body -ceq 'LANDED') {
        return "landed"
    }
    if ($Body -cmatch '^escalated:') {
        return "escalated"
    }
    return ""
}

function Test-TerminalMatches([string]$Terminal, [string]$Expected) {
    return $Expected -ceq "any" -or $Terminal -ceq $Expected
}

function Get-TerminalFromMessages($Messages) {
    $matches = New-Object System.Collections.Generic.List[object]
    foreach ($message in @($Messages)) {
        $from = [string]$message.from
        $kind = [string]$message.kind
        $body = [string]$message.body
        $terminal = Classify-Terminal $body
        if ($from -ceq "conductor" -and $kind -ceq "decision" -and $terminal) {
            $matches.Add([pscustomobject]@{
                Terminal = $terminal
                Body     = $body
            })
        }
    }
    if ($matches.Count -eq 0) {
        return $null
    }
    return $matches[$matches.Count - 1]
}

function Get-WatchObjects([string[]]$Lines) {
    $objects = @()
    foreach ($line in $Lines) {
        $trimmed = ([string]$line).Trim()
        if ($trimmed.Length -eq 0) {
            continue
        }
        try {
            $objects += ($trimmed | ConvertFrom-Json)
        } catch {
            Fail "watch emitted invalid JSON line: $($_.Exception.Message)`n$trimmed"
        }
    }
    return @($objects)
}

function Get-ControlObjects([string[]]$Lines) {
    $objects = @()
    foreach ($line in $Lines) {
        $trimmed = ([string]$line).Trim()
        if ($trimmed.Length -eq 0) {
            continue
        }
        try {
            $objects += ($trimmed | ConvertFrom-Json)
        } catch {
            Fail "control feed contains invalid JSON line: $($_.Exception.Message)`n$trimmed"
        }
    }
    return @($objects)
}
function Test-HasNonEmptyString($Obj, [string]$Name) {
    $prop = $Obj.PSObject.Properties[$Name]
    return $null -ne $prop -and $prop.Value -is [string] -and -not [string]::IsNullOrWhiteSpace($prop.Value)
}

function Test-IsValidSteer($Obj) {
    return [string]$Obj.cmd -ceq "steer" -and (Test-HasNonEmptyString $Obj "from") -and (Test-HasNonEmptyString $Obj "prompt")
}

function Test-IsValidAbort($Obj) {
    if ([string]$Obj.cmd -cne "abort" -or -not (Test-HasNonEmptyString $Obj "from")) {
        return $false
    }
    $hard = $Obj.PSObject.Properties["hard"]
    return $null -eq $hard -or $hard.Value -is [bool]
}
function Test-IsValidControl($Obj) {
    return (Test-IsValidSteer $Obj) -or (Test-IsValidAbort $Obj)
}
function Get-TerminalFromWatch($Objects) {
    $matches = New-Object System.Collections.Generic.List[object]
    foreach ($obj in @($Objects)) {
        $from = [string]$obj.from
        $kind = [string]$obj.kind
        $body = [string]$obj.body
        $terminal = Classify-Terminal $body
        if ($from -ceq "conductor" -and $kind -ceq "decision" -and $terminal) {
            $matches.Add([pscustomobject]@{
                Terminal = $terminal
                Body     = $body
            })
        }
    }
    if ($matches.Count -eq 0) {
        return $null
    }
    return $matches[$matches.Count - 1]
}

function Invoke-Evidence([string]$RepoPath, [string]$TargetDirForExe) {
    $ensembleExe = Resolve-EnsembleExe $TargetDirForExe -NoBuild:$NoBuild
    Write-Host "ensemble: $ensembleExe"
    Write-Host "repo: $RepoPath"
    Write-Host "team: $Team"
    Write-Host "watch: $Watch"

    Reject-LegacySinceForRealEvidence
    $teamCursor = Resolve-RequiredCursor "TeamSince" $TeamSince
    $watchCursor = Resolve-RequiredCursor "WatchSince" $WatchSince
    $controlRequired = [bool]($RequireControl -or $RequireSteer -or $RequireAbort)
    $controlCursor = Resolve-ControlCursor $controlRequired

    $teamOut = & $ensembleExe team inbox --repo $RepoPath --team $Team --since "$teamCursor" --json 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "team inbox failed with exit $LASTEXITCODE`n$($teamOut -join [Environment]::NewLine)"
    }
    $teamJson = Parse-JsonOrFail "team inbox" ($teamOut -join [Environment]::NewLine)
    $teamTerminal = Get-TerminalFromMessages $teamJson.messages
    if ($null -eq $teamTerminal) {
        Fail "team inbox has no conductor decision terminal message since cursor $teamCursor"
    }
    if (-not (Test-TerminalMatches $teamTerminal.Terminal $ExpectTerminal)) {
        Fail "team terminal was $($teamTerminal.Terminal), expected $ExpectTerminal"
    }

    $watchOut = & $ensembleExe watch $Watch --repo $RepoPath --team $Team --since "$watchCursor" --json 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "watch failed with exit $LASTEXITCODE`n$($watchOut -join [Environment]::NewLine)"
    }
    $watchObjects = @(Get-WatchObjects $watchOut)
    if ($watchObjects.Count -eq 0) {
        Fail "watch stream has no events since cursor $watchCursor"
    }
    $watchTerminal = Get-TerminalFromWatch $watchObjects
    if ($null -eq $watchTerminal) {
        Fail "watch stream has no conductor decision terminal event since cursor $watchCursor"
    }
    if (-not (Test-TerminalMatches $watchTerminal.Terminal $ExpectTerminal)) {
        Fail "watch terminal was $($watchTerminal.Terminal), expected $ExpectTerminal"
    }
    if ($teamTerminal.Terminal -cne $watchTerminal.Terminal -or $teamTerminal.Body -cne $watchTerminal.Body) {
        Fail "team/watch terminal evidence disagree: team='$($teamTerminal.Body)' watch='$($watchTerminal.Body)'"
    }

    $controlPath = Join-Path $RepoPath ".ensemble/control/$Watch.ndjson"
    $controlLines = @()
    if (Test-Path -LiteralPath $controlPath -PathType Leaf) {
        $allControlLines = @(Get-Content -LiteralPath $controlPath | Where-Object { ([string]$_).Trim().Length -gt 0 })
        if ($controlCursor -gt $allControlLines.Count) {
            Fail "control cursor $controlCursor is beyond feed length $($allControlLines.Count): $controlPath"
        }
        $controlLines = @($allControlLines | Select-Object -Skip $controlCursor)
    }
    if ($RequireControl -and $controlLines.Count -eq 0) {
        Fail "control feed missing or empty: $controlPath"
    }
    $controlObjects = @()
    if ($RequireControl -or $RequireSteer -or $RequireAbort) {
        $controlObjects = @(Get-ControlObjects $controlLines)
    }
    if ($controlRequired -and @($controlObjects | Where-Object { -not (Test-IsValidControl $_) }).Count -gt 0) {
        Fail "control feed contains an invalid command shape: $controlPath"
    }
    if ($RequireSteer -and -not (@($controlObjects | Where-Object { Test-IsValidSteer $_ }).Count -gt 0)) {
        Fail "control feed does not contain a valid steer command: $controlPath"
    }
    if ($RequireAbort -and -not (@($controlObjects | Where-Object { Test-IsValidAbort $_ }).Count -gt 0)) {
        Fail "control feed does not contain a valid abort command: $controlPath"
    }

    $summary = [pscustomobject]@{
        ok              = $true
        repo            = $RepoPath
        team            = $Team
        watch           = $Watch
        teamSince       = $teamCursor
        watchSince      = $watchCursor
        controlSince    = $controlCursor
        terminal        = $teamTerminal.Terminal
        teamDecision    = $teamTerminal.Body
        watchDecision   = $watchTerminal.Body
        watchEvents     = $watchObjects.Count
        controlEvents   = $controlLines.Count
        requireControl  = [bool]$RequireControl
        requireSteer    = [bool]$RequireSteer
        requireAbort    = [bool]$RequireAbort
    }
    $summary | ConvertTo-Json -Compress
    Write-Host "Phase 2 run evidence passed." -ForegroundColor Green
}

function Invoke-SelfTest([switch]$Mismatch, [switch]$BadControl, [switch]$BadControlShape, [switch]$BadTerminalPrefix, [switch]$BadTerminalCase) {
    $root = Join-Path ([System.IO.Path]::GetTempPath()) "ensemble-phase2-evidence-selftest-$PID"
    if (Test-Path -LiteralPath $root) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    $pushed = $false
    try {
        Push-Location $root
        $pushed = $true
        git init -q
        git config user.email "phase2-evidence@example.invalid"
        git config user.name "phase2 evidence"
        "# evidence" | Set-Content -LiteralPath README.md -Encoding ascii
        git add README.md
        git commit -q -m init
        $teamDir = Join-Path $root ".ensemble/teams/main"
        $streamDir = Join-Path $root ".ensemble/stream"
        $controlDir = Join-Path $root ".ensemble/control"
        New-Item -ItemType Directory -Path $teamDir, $streamDir, $controlDir -Force | Out-Null
        $teamLines = if ($BadTerminalPrefix) {
            @('{"from":"conductor","kind":"decision","body":"ESCALATED_PENDING"}')
        } elseif ($BadTerminalCase) {
            @('{"from":"conductor","kind":"decision","body":"landed"}')
        } else {
            @(
                '{"from":"conductor","kind":"decision","body":"escalated: old failure"}'
                '{"from":"conductor","kind":"decision","body":"LANDED"}'
            )
        }
        $teamLines | Set-Content -LiteralPath (Join-Path $teamDir "board.jsonl") -Encoding utf8
        $watchLines = if ($BadTerminalPrefix) {
            @('{"from":"conductor","kind":"decision","body":"ESCALATED_PENDING"}')
        } elseif ($BadTerminalCase) {
            @('{"from":"conductor","kind":"decision","body":"landed"}')
        } elseif ($Mismatch) {
            @(
                '{"from":"conductor","kind":"decision","body":"LANDED"}'
                '{"from":"conductor","kind":"decision","body":"escalated: later stream failure"}'
            )
        } else {
            @(
                '{"from":"conductor","kind":"decision","body":"escalated: old failure"}'
                '{"from":"conductor","kind":"decision","body":"LANDED"}'
            )
        }
        $watchLines | Set-Content -LiteralPath (Join-Path $streamDir "main.ndjson") -Encoding utf8
        $controlLines = if ($BadControl) {
            @('not-json but contains "cmd":"steer" and "cmd":"abort"')
        } elseif ($BadControlShape) {
            @(
                '{"cmd":"steer","from":"operator"}'
                '{"cmd":"abort","from":"operator","hard":"no"}'
            )
        } else {
            @(
                '{"cmd":"steer","from":"operator","prompt":"focus"}'
                '{"cmd":"abort","from":"operator","hard":true}'
            )
        }
        $controlLines | Set-Content -LiteralPath (Join-Path $controlDir "main.ndjson") -Encoding utf8
        Pop-Location
        $pushed = $false
        if ($Mismatch -or $BadControl -or $BadControlShape -or $BadTerminalPrefix -or $BadTerminalCase) {
            $childArgs = @("-NoProfile", "-File", $PSCommandPath, "-Repo", $root, "-Team", $Team, "-Watch", $Watch, "-TeamSince", "0", "-WatchSince", "0", "-TargetDir", $TargetDir, "-NoBuild")
            $expectedFailure = ""
            if ($Mismatch) {
                $expectedFailure = "team/watch terminal evidence disagree"
            } elseif ($BadControl) {
                $childArgs += @("-RequireControl", "-ControlSince", "0")
                $expectedFailure = "control feed contains invalid JSON line"
            } elseif ($BadControlShape) {
                $childArgs += @("-RequireControl", "-RequireSteer", "-RequireAbort", "-ControlSince", "0")
                $expectedFailure = "control feed contains an invalid command shape"
            } elseif ($BadTerminalPrefix -or $BadTerminalCase) {
                $expectedFailure = "team inbox has no conductor decision terminal message"
            }
            $childOut = & pwsh @childArgs 2>&1
            $childText = $childOut -join [Environment]::NewLine
            if ($LASTEXITCODE -eq 0) {
                Fail "negative self-test unexpectedly accepted bad evidence`n$childText"
            }
            if ($childText -cnotmatch [regex]::Escape($expectedFailure)) {
                Fail "negative self-test failed for an unexpected reason; wanted '$expectedFailure'`n$childText"
            }
            Write-Host "Negative self-test rejected bad evidence as expected." -ForegroundColor Green
            return
        }
        Invoke-Evidence $root $TargetDir
    } finally {
        if ($pushed) {
            Pop-Location
        }
        if (Test-Path -LiteralPath $root) {
            Remove-Item -LiteralPath $root -Recurse -Force
        }
    }
}

if ($SelfTest -or $SelfTestMismatch -or $SelfTestBadControl -or $SelfTestBadControlShape -or $SelfTestBadTerminalPrefix -or $SelfTestBadTerminalCase) {
    Invoke-SelfTest -Mismatch:$SelfTestMismatch -BadControl:$SelfTestBadControl -BadControlShape:$SelfTestBadControlShape -BadTerminalPrefix:$SelfTestBadTerminalPrefix -BadTerminalCase:$SelfTestBadTerminalCase
    exit 0
}

$repoFull = Resolve-Repo $Repo
if (-not (Test-Path -LiteralPath $repoFull -PathType Container)) {
    Fail "repo path not found: $repoFull"
}

Invoke-Evidence $repoFull $TargetDir
