#!/usr/bin/env pwsh
# ensemble smoke — build a release ensemble.exe, then drive one end-to-end task
# through the crew (codex implements -> claude reviews) in a throwaway git repo.
#
# Usage (from anywhere):
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1 "Create a file named NOTES.md with one line about git"
#   pwsh D:\Projects\ensemble\scripts\smoke.ps1 -Crew '@{implement="codex";review="claude"}'  # (default crew is fine)
#
# Notes:
#  * Uses the RELEASE build on purpose — `cargo build`/`cargo test` on the *debug*
#    target hit LNK1104 / os-error-5 because Windows Defender locks target\debug\*.exe.
#    The release path is built fresh and doesn't get hammered, so it links cleanly.
#  * codex/claude must be on PATH (they are, for this machine). A missing CLI -> the
#    run ESCALATES with "agent CLI not installed", not a crash.
#  * KNOWN BUG: prompts can be mangled passing through `cmd /C` on Windows (the npm
#    .cmd shims), so codex/claude may misread the task (e.g. "hello" -> "0"). The
#    orchestration itself is correct; the Windows arg-escaping fix is a tracked follow-up.

param(
    [string]$Task = "Create a file named hi.txt containing the word hello"
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot   # repo root (this script lives in scripts/)

Write-Host "== building release ensemble.exe ==" -ForegroundColor Cyan
Push-Location $repo
try {
    $built = $false
    for ($i = 1; $i -le 3; $i++) {
        cargo build --release --bin ensemble
        if ($LASTEXITCODE -eq 0) { $built = $true; break }
        Write-Host "  build try $i failed (Defender lock?) — retrying in 4s" -ForegroundColor Yellow
        Start-Sleep -Seconds 4
    }
    if (-not $built) { throw "cargo build --release failed (see above)" }
} finally { Pop-Location }

$exe = Join-Path $repo "target\release\ensemble.exe"
if (-not (Test-Path $exe)) { throw "build ok but $exe not found" }
Write-Host "  exe: $exe" -ForegroundColor DarkGray

# Fresh throwaway git repo for the run (each run starts clean).
$smoke = Join-Path $env:USERPROFILE "ensemble-smoke"
if (Test-Path $smoke) { Remove-Item -Recurse -Force $smoke }
New-Item -ItemType Directory -Force $smoke | Out-Null
Push-Location $smoke
try {
    git init -q
    git config user.email "t@t"
    git config user.name  "t"
    "# smoke" | Set-Content -NoNewline README.md
    git add -A
    git commit -q -m init

    @'
pipeline = ["implement", "review"]
[gate]
min_approvals = 1
max_rounds    = 1
on_flake      = "exclude"
[roles.implement]
agent = "codex"
[roles.review]
agent = "claude"
'@ | Set-Content crew.toml

    Write-Host "== running task ==" -ForegroundColor Cyan
    Write-Host "  task: $Task" -ForegroundColor DarkGray
    Write-Host "  repo: $smoke" -ForegroundColor DarkGray
    Write-Host ""
    & $exe run $Task --repo . --crew crew.toml
    $code = $LASTEXITCODE
} finally { Pop-Location }

Write-Host ""
Write-Host ("== exit {0} ({1}) ==" -f $code, $(if ($code -eq 0) { "LANDED" } else { "ESCALATED/failed" })) `
    -ForegroundColor $(if ($code -eq 0) { "Green" } else { "Yellow" })
exit $code
