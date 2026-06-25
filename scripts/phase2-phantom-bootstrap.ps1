#!/usr/bin/env pwsh
# Bootstrap Phase-2 ensemble services through Phantom's HMAC-protected admin shell.
#
# This is the fallback transport when SSH/Tailscale SSH is unavailable but
# `phantom serve` is reachable on the peer. The cluster secret is read from
# PHANTOM_CLUSTER_SECRET or -SecretFile and is never printed.

param(
    [string]$Manifest = "phase2-fleet.local.json",
    [string]$Node = "all",
    [ValidateSet("probe", "start", "verify")]
    [string]$Action = "probe",
    [int]$PhantomPort = 7878,
    [int]$ServicePort = 0,
    [string]$SecretFile = "",
    [string]$SecretEnv = "PHANTOM_CLUSTER_SECRET",
    [string]$WindowsExeUrl = "",
    [string]$WindowsExeSha256 = "",
    [string]$WindowsExePath = "",
    [string]$WindowsUploadDir = "D:\.phantom-mesh\phase2\ensemble-upload",
    [int]$UploadChunkChars = 131072,
    [string]$MacRepoPath = '$HOME/Projects/ensemble',
    [string]$Branch = "phase2-verify-fixes",
    [int]$BuildWaitSecs = 180,
    [switch]$PlanOnly,
    [switch]$Json,
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Fail([string]$Message) {
    Write-Host "FAIL: $Message" -ForegroundColor Red
    exit 1
}

function Get-HmacHex([string]$Secret, [string]$Body) {
    $hmac = [System.Security.Cryptography.HMACSHA256]::new([System.Text.Encoding]::UTF8.GetBytes($Secret))
    try {
        return (($hmac.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Body)) | ForEach-Object { $_.ToString("x2") }) -join "")
    }
    finally {
        $hmac.Dispose()
    }
}

function Get-FileSha256Hex([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead($Path)
        try {
            return (($sha.ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) -join "")
        }
        finally {
            $stream.Dispose()
        }
    }
    finally {
        $sha.Dispose()
    }
}

function Test-Sha256Hex([string]$Value) {
    return (-not [string]::IsNullOrWhiteSpace($Value) -and $Value -match '^[A-Fa-f0-9]{64}$')
}

function Test-ActionRequiresSecret([string]$Name) {
    return -not $Name.Equals("verify", [System.StringComparison]::OrdinalIgnoreCase)
}

function Assert-WindowsExeUrlPolicy {
    if ([string]::IsNullOrWhiteSpace($WindowsExeUrl)) {
        return
    }
    if ($WindowsExeUrl -match "['`r`n`0]") {
        Fail "-WindowsExeUrl contains unsupported characters"
    }
    if (-not (Test-Sha256Hex $WindowsExeSha256)) {
        Fail "-WindowsExeUrl requires -WindowsExeSha256 with a 64-character SHA-256 hex digest"
    }
}

function Read-SecretFromText([string]$Text) {
    foreach ($line in ($Text -split "`r?`n")) {
        if ($line -match '^\s*cluster_secret\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    $trimmed = $Text.Trim()
    if ($trimmed.Length -gt 0 -and $trimmed -notmatch '\s') {
        return $trimmed
    }
    return ""
}

function Resolve-Secret {
    $fromEnv = [System.Environment]::GetEnvironmentVariable($SecretEnv)
    if (-not [string]::IsNullOrWhiteSpace($fromEnv)) {
        return $fromEnv.Trim()
    }
    if (-not [string]::IsNullOrWhiteSpace($SecretFile)) {
        if (-not (Test-Path -LiteralPath $SecretFile -PathType Leaf)) {
            Fail "secret file not found: $SecretFile"
        }
        $secret = Read-SecretFromText (Get-Content -Raw -LiteralPath $SecretFile)
        if (-not [string]::IsNullOrWhiteSpace($secret)) {
            return $secret
        }
        Fail "secret file does not contain a cluster_secret assignment or raw secret: $SecretFile"
    }
    Fail "cluster secret missing. Set $SecretEnv or pass -SecretFile <path>."
}

function Read-Fleet {
    if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) {
        Fail "fleet manifest not found: $Manifest"
    }
    try {
        return Get-Content -Raw -LiteralPath $Manifest | ConvertFrom-Json
    }
    catch {
        Fail "fleet manifest is not valid JSON: $Manifest. $($_.Exception.Message)"
    }
}

function Get-FleetNodes($Fleet) {
    $prop = $Fleet.PSObject.Properties["nodes"]
    if ($null -eq $prop -or @($prop.Value).Count -eq 0) {
        Fail "fleet manifest must define a non-empty nodes array"
    }
    return @($prop.Value | ForEach-Object { [string]$_ })
}

function Get-FleetServicePort($Fleet) {
    if ($ServicePort -gt 0) {
        return $ServicePort
    }
    $service = $Fleet.PSObject.Properties["service"]
    if ($null -ne $service -and $null -ne $service.Value) {
        $port = $service.Value.PSObject.Properties["port"]
        if ($null -ne $port -and -not [string]::IsNullOrWhiteSpace([string]$port.Value)) {
            try {
                $parsed = [int]$port.Value
            }
            catch {
                Fail "manifest service.port must be an integer from 1 to 65535"
            }
            if ($parsed -lt 1 -or $parsed -gt 65535) {
                Fail "manifest service.port must be an integer from 1 to 65535"
            }
            return $parsed
        }
    }
    return 8788
}

function Select-Nodes([string[]]$Nodes) {
    if ($Node.Equals("all", [System.StringComparison]::OrdinalIgnoreCase)) {
        return $Nodes
    }
    $wanted = @($Node.Split(",") | ForEach-Object { $_.Trim() } | Where-Object { $_.Length -gt 0 })
    if ($wanted.Count -eq 0) {
        Fail "-Node must be 'all' or a comma-separated list"
    }
    foreach ($item in $wanted) {
        if (-not ($Nodes | Where-Object { $_.Equals($item, [System.StringComparison]::OrdinalIgnoreCase) })) {
            Fail "node '$item' is not present in $Manifest"
        }
    }
    return $wanted
}

function Resolve-NodeHost([string]$HostName) {
    if ($HostName -match '^(https?://|\[|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$)' -or $HostName.Contains(".")) {
        return $HostName.TrimEnd("/")
    }
    if (-not (Get-Command tailscale -ErrorAction SilentlyContinue)) {
        return $HostName
    }
    try {
        $status = tailscale status --json | ConvertFrom-Json
        $entries = @()
        if ($null -ne $status.Self) {
            $entries += $status.Self
        }
        if ($null -ne $status.Peer) {
            $entries += @($status.Peer.PSObject.Properties.Value)
        }
        foreach ($entry in $entries) {
            $dns = [string]$entry.DNSName
            $short = [string]$entry.HostName
            if ($short.Equals($HostName, [System.StringComparison]::OrdinalIgnoreCase)) {
                if (-not [string]::IsNullOrWhiteSpace($dns)) {
                    return $dns.TrimEnd(".")
                }
            }
            if (-not [string]::IsNullOrWhiteSpace($dns) -and $dns.StartsWith("$HostName.", [System.StringComparison]::OrdinalIgnoreCase)) {
                return $dns.TrimEnd(".")
            }
        }
    }
    catch {
        return $HostName
    }
    return $HostName
}

function Invoke-PhantomShell([string]$HostName, [string]$Command, [string]$Secret) {
    $body = [ordered]@{
        cmd = $Command
        timeout_secs = 30
    } | ConvertTo-Json -Compress
    $sig = Get-HmacHex $Secret $body
    $uri = "http://$HostName`:$PhantomPort/rpc/admin/shell"
    try {
        return Invoke-RestMethod -Method Post -Uri $uri -TimeoutSec 35 -Headers @{
            "X-Cluster-Auth" = $sig
        } -ContentType "application/json" -Body $body
    }
    catch {
        throw "phantom admin shell failed on $HostName`: $($_.Exception.Message)"
    }
}

function Invoke-PhantomToolCall([string]$HostName, [string]$Tool, $Args, [string]$Secret) {
    $body = [ordered]@{
        tool = $Tool
        args = $Args
    } | ConvertTo-Json -Compress -Depth 6
    $sig = Get-HmacHex $Secret $body
    $uri = "http://$HostName`:$PhantomPort/rpc/tool/call"
    try {
        return Invoke-RestMethod -Method Post -Uri $uri -TimeoutSec 45 -Headers @{
            "X-Cluster-Auth" = $sig
        } -ContentType "application/json" -Body $body
    }
    catch {
        throw "phantom tool call failed on $HostName`: $($_.Exception.Message)"
    }
}

function Install-WindowsExeViaToolCall([string]$HostName, [string]$Secret) {
    if ([string]::IsNullOrWhiteSpace($WindowsExePath)) {
        return $false
    }
    if (-not (Test-Path -LiteralPath $WindowsExePath -PathType Leaf)) {
        Fail "Windows exe not found: $WindowsExePath"
    }
    if ($UploadChunkChars -lt 16384 -or $UploadChunkChars -gt 524288) {
        Fail "-UploadChunkChars must be between 16384 and 524288"
    }

    $expectedSha = Get-FileSha256Hex $WindowsExePath
    $b64 = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($WindowsExePath))
    $chunks = [Math]::Ceiling($b64.Length / $UploadChunkChars)
    $uploadId = [Guid]::NewGuid().ToString("N")
    $uploadDir = $WindowsUploadDir.TrimEnd('\', '/') + "\upload-$uploadId"
    for ($i = 0; $i -lt $chunks; $i++) {
        $len = [Math]::Min($UploadChunkChars, $b64.Length - ($i * $UploadChunkChars))
        $chunk = $b64.Substring($i * $UploadChunkChars, $len)
        $remote = $uploadDir.TrimEnd('\', '/') + "\chunk-$($i.ToString('D6')).b64"
        $resp = Invoke-PhantomToolCall $HostName "file_write" ([ordered]@{
            path = $remote
            content = $chunk
            create_dirs = $true
        }) $Secret
        if ([string]$resp.output -notmatch '^Written ') {
            throw "upload chunk $($i + 1)/$chunks failed: $($resp.output)"
        }
    }

    $escapedUploadDir = $uploadDir.Replace("'", "''")
    $ps = @"
`$ErrorActionPreference = 'Stop'
`$dir = '$escapedUploadDir'
`$expectedChunks = $chunks
`$expectedSha = '$expectedSha'
`$dest = Join-Path `$env:LOCALAPPDATA 'ensemble\bin\ensemble.exe'
New-Item -ItemType Directory -Force -Path (Split-Path `$dest -Parent) | Out-Null
`$files = @(Get-ChildItem -LiteralPath `$dir -Filter 'chunk-*.b64' | Sort-Object Name)
if (`$files.Count -ne `$expectedChunks) { Write-Error "chunk count mismatch: got `$(`$files.Count), expected `$expectedChunks"; exit 91 }
`$b64 = [System.Text.StringBuilder]::new()
`$files | ForEach-Object { [void]`$b64.Append((Get-Content -Raw -LiteralPath `$_.FullName)) }
`$tmp = "`$dest.tmp"
[System.IO.File]::WriteAllBytes(`$tmp, [Convert]::FromBase64String(`$b64.ToString()))
`$actualSha = (Get-FileHash -Algorithm SHA256 -LiteralPath `$tmp).Hash.ToLowerInvariant()
if (`$actualSha -ne `$expectedSha) { Remove-Item -LiteralPath `$tmp -Force -ErrorAction SilentlyContinue; Write-Error "sha256 mismatch after upload"; exit 92 }
Move-Item -LiteralPath `$tmp -Destination `$dest -Force
`$item = Get-Item `$dest
Remove-Item -LiteralPath `$dir -Recurse -Force -ErrorAction SilentlyContinue
Write-Output "installed bytes=`$(`$item.Length) sha256=`$actualSha"
"@
    $encoded = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($ps))
    $installed = Invoke-PhantomShell $HostName "powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $encoded" $Secret
    if ($installed.exit_code -ne 0 -or ([string]$installed.stdout) -notmatch "installed bytes=") {
        throw "remote decode/install failed: $($installed.stdout) $($installed.stderr)"
    }
    return $true
}

function Test-PhantomHealth([string]$HostName) {
    $uri = "http://$HostName`:$PhantomPort/healthz"
    try {
        $resp = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri $uri
        return ($resp.StatusCode -eq 200 -and ([string]$resp.Content).Trim() -eq "ok")
    }
    catch {
        return $false
    }
}

function Test-EnsembleHealth([string]$HostName, [int]$Port) {
    $uri = "http://$HostName`:$Port/health"
    try {
        $resp = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri $uri
        return ($resp.StatusCode -eq 200 -and ([string]$resp.Content) -match '"ok"\s*:\s*true')
    }
    catch {
        return $false
    }
}

function Wait-EnsembleHealth([string]$HostName, [int]$Port, [int]$TimeoutSec = 15) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if (Test-EnsembleHealth $HostName $Port) {
            return $true
        }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Quote-ShSingle([string]$Value) {
    return "'" + $Value.Replace("'", "'\''") + "'"
}

function New-ShellAssignmentValue([string]$Value, [string]$Name) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        Fail "$Name must not be empty"
    }
    if ($Value -match "[`r`n`0]") {
        Fail "$Name must not contain control characters"
    }
    if ($Value -eq '$HOME') {
        return '"$HOME"'
    }
    if ($Value.StartsWith('$HOME/')) {
        return '"$HOME"' + (Quote-ShSingle $Value.Substring(5))
    }
    if ($Value -eq '~') {
        return '"$HOME"'
    }
    if ($Value.StartsWith('~/')) {
        return '"$HOME"' + (Quote-ShSingle $Value.Substring(1))
    }
    return Quote-ShSingle $Value
}

function Test-SafeGitRef([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }
    if ($Value.StartsWith('-') -or $Value.Contains('..') -or $Value -notmatch '^[A-Za-z0-9._/@-]+$') {
        return $false
    }
    return $true
}

function Assert-SafeGitRef([string]$Value, [string]$Name) {
    if (-not (Test-SafeGitRef $Value)) {
        Fail "$Name contains unsupported characters"
    }
}

function Get-RemoteOs([string]$HostName, [string]$Secret) {
    $ver = Invoke-PhantomShell $HostName "ver" $Secret
    $verOut = ([string]$ver.stdout) + ([string]$ver.stderr)
    if ($ver.exit_code -eq 0 -and $verOut -match "Windows") {
        return "windows"
    }
    $uname = Invoke-PhantomShell $HostName "uname -s" $Secret
    $unameOut = (([string]$uname.stdout).Trim()).ToLowerInvariant()
    if ($unameOut -match "darwin") {
        return "macos"
    }
    if ($unameOut -match "linux") {
        return "linux"
    }
    return "unknown"
}

function New-StartCommand([string]$OsName, [int]$Port) {
    if ($OsName -eq "windows") {
        Assert-WindowsExeUrlPolicy
        $urlLiteral = if ([string]::IsNullOrWhiteSpace($WindowsExeUrl)) {
            "''"
        } else {
            "'" + $WindowsExeUrl.Replace("'", "''") + "'"
        }
        $urlShaLiteral = if ([string]::IsNullOrWhiteSpace($WindowsExeSha256)) {
            "''"
        } else {
            "'" + $WindowsExeSha256.ToLowerInvariant() + "'"
        }
        $ps = @"
`$url = $urlLiteral
`$expectedUrlSha = $urlShaLiteral
`$exe = (Get-Command ensemble -ErrorAction SilentlyContinue).Source
if (-not `$exe) { `$exe = Join-Path `$env:LOCALAPPDATA 'ensemble\bin\ensemble.exe' }
if (-not (Test-Path `$exe)) {
    if ([string]::IsNullOrWhiteSpace(`$url)) { Write-Error 'ensemble not found and no WindowsExeUrl was provided'; exit 127 }
    New-Item -ItemType Directory -Force -Path (Split-Path `$exe -Parent) | Out-Null
    `$tmp = "`$exe.download"
    Invoke-WebRequest -UseBasicParsing -Uri `$url -OutFile `$tmp
    `$actualUrlSha = (Get-FileHash -Algorithm SHA256 -LiteralPath `$tmp).Hash.ToLowerInvariant()
    if (`$actualUrlSha -ne `$expectedUrlSha) { Remove-Item -LiteralPath `$tmp -Force -ErrorAction SilentlyContinue; Write-Error 'sha256 mismatch after WindowsExeUrl download'; exit 93 }
    Move-Item -LiteralPath `$tmp -Destination `$exe -Force
}
Start-Process -WindowStyle Hidden -FilePath `$exe -ArgumentList @('up','--port','$Port')
Write-Output 'started'
"@
        $encoded = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($ps))
        return "powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $encoded"
    }
    if ($OsName -eq "macos" -or $OsName -eq "linux") {
        Assert-SafeGitRef $Branch "-Branch"
        $repoValue = New-ShellAssignmentValue $MacRepoPath "-MacRepoPath"
        $branchValue = Quote-ShSingle $Branch
        $script = @"
repo=$repoValue
branch=$branchValue
mkdir -p "`$HOME/.ensemble" "`$HOME/.local/bin"
if command -v ensemble >/dev/null 2>&1; then
  nohup ensemble up --port $Port > "`$HOME/.ensemble/phase2-up-$Port.log" 2>&1 &
  echo started
elif [ -d "`$repo/.git" ]; then
  ( cd "`$repo" && git fetch origin "`$branch" && git checkout "`$branch" && git pull --ff-only origin "`$branch" && cargo build --release --bin ensemble && cp target/release/ensemble "`$HOME/.local/bin/ensemble" && chmod +x "`$HOME/.local/bin/ensemble" && nohup "`$HOME/.local/bin/ensemble" up --port $Port > "`$HOME/.ensemble/phase2-up-$Port.log" 2>&1 & echo started ) > "`$HOME/.ensemble/phase2-bootstrap-$Port.log" 2>&1 &
  echo started
else
  echo "ensemble not found and repo missing: `$repo"
  exit 127
fi
"@
        return "sh -lc " + (Quote-ShSingle $script)
    }
    return ""
}

function Invoke-NodeAction([string]$HostName, [int]$Port, [string]$Secret) {
    $routeHost = Resolve-NodeHost $HostName
    $ensembleBefore = Test-EnsembleHealth $routeHost $Port
    $phantomOk = Test-PhantomHealth $routeHost
    if ($Action -eq "verify") {
        return [pscustomobject]@{
            node = $HostName
            phantom = if ($phantomOk) { "healthy" } else { "unreachable" }
            os = $null
            ensemble = if ($ensembleBefore) { "healthy" } else { "unreachable" }
            action = $Action
            ok = $ensembleBefore
            detail = if ($ensembleBefore) { "ensemble health ok" } else { "ensemble health failed" }
        }
    }
    if (-not $phantomOk) {
        return [pscustomobject]@{
            node = $HostName
            phantom = "unreachable"
            os = $null
            ensemble = if ($ensembleBefore) { "healthy" } else { "unreachable" }
            action = $Action
            ok = $false
            detail = "phantom healthz failed"
        }
    }

    $os = Get-RemoteOs $routeHost $Secret
    if ($Action -eq "probe") {
        $probe = Invoke-PhantomShell $routeHost "echo ensemble-phantom-probe" $Secret
        $probeOk = ($probe.exit_code -eq 0 -and ([string]$probe.stdout) -match "ensemble-phantom-probe")
        return [pscustomobject]@{
            node = $HostName
            phantom = "healthy"
            os = $os
            ensemble = if ($ensembleBefore) { "healthy" } else { "unreachable" }
            action = $Action
            ok = $probeOk
            detail = if ($probeOk) { "admin shell ok" } else { "admin shell probe failed" }
        }
    }
    $cmd = New-StartCommand $os $Port
    if ([string]::IsNullOrWhiteSpace($cmd)) {
        return [pscustomobject]@{
            node = $HostName
            phantom = "healthy"
            os = $os
            ensemble = if ($ensembleBefore) { "healthy" } else { "unreachable" }
            action = $Action
            ok = $false
            detail = "unsupported remote OS"
        }
    }
    if ($PlanOnly) {
        return [pscustomobject]@{
            node = $HostName
            phantom = "healthy"
            os = $os
            ensemble = if ($ensembleBefore) { "healthy" } else { "unreachable" }
            action = $Action
            ok = $true
            detail = "would start ensemble up --port $Port"
        }
    }
    if ($os -eq "windows" -and -not $ensembleBefore -and -not [string]::IsNullOrWhiteSpace($WindowsExePath)) {
        [void](Install-WindowsExeViaToolCall $routeHost $Secret)
    }
    $start = Invoke-PhantomShell $routeHost $cmd $Secret
    $started = ($start.exit_code -eq 0 -and ([string]$start.stdout) -match "started")
    $healthy = if ($started) { Wait-EnsembleHealth $routeHost $Port $BuildWaitSecs } else { $false }
    return [pscustomobject]@{
        node = $HostName
        phantom = "healthy"
        os = $os
        ensemble = if ($healthy) { "healthy" } elseif ($ensembleBefore) { "healthy-before" } else { "unreachable" }
        action = $Action
        ok = ($started -and $healthy)
        detail = if ($started -and $healthy) { "started and healthy" } else { "start failed or health did not become ready" }
    }
}

function Invoke-SelfTest {
    $h = Get-HmacHex "key" "The quick brown fox jumps over the lazy dog"
    if ($h -ne "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8") {
        Fail "self-test hmac mismatch"
    }
    $secret = Read-SecretFromText @"
[cluster]
cluster_secret = "abc123"
"@
    if ($secret -ne "abc123") {
        Fail "self-test secret parser failed"
    }
    $raw = Read-SecretFromText "abc123`n"
    if ($raw -ne "abc123") {
        Fail "self-test raw secret parser failed"
    }
    $port = Get-FleetServicePort ([pscustomobject]@{ nodes = @("m1") })
    if ($port -ne 8788) {
        Fail "self-test expected missing service.port to default to 8788"
    }
    $homePath = New-ShellAssignmentValue '$HOME/Projects/ensemble' '-MacRepoPath'
    if (-not $homePath.StartsWith('"$HOME"')) {
        Fail "self-test shell path escaping failed: $homePath"
    }
    if (-not (Test-SafeGitRef 'phase2-verify-fixes')) {
        Fail "self-test expected normal branch to pass"
    }
    if (Test-SafeGitRef 'bad$(touch-pwn)') {
        Fail "self-test expected unsafe branch to be rejected"
    }
    if (-not (Test-Sha256Hex ("a" * 64))) {
        Fail "self-test expected valid sha256 hex to pass"
    }
    if (Test-Sha256Hex ("z" * 64)) {
        Fail "self-test expected invalid sha256 hex to fail"
    }
    if (Test-ActionRequiresSecret "verify") {
        Fail "self-test expected verify not to require a Phantom secret"
    }
    if (-not (Test-ActionRequiresSecret "start")) {
        Fail "self-test expected start to require a Phantom secret"
    }
    $sampleB64 = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes("chunk-round-trip-check"))
    $parts = New-Object System.Collections.Generic.List[string]
    for ($i = 0; $i -lt $sampleB64.Length; $i += 5) {
        $parts.Add($sampleB64.Substring($i, [Math]::Min(5, $sampleB64.Length - $i)))
    }
    $roundTrip = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(($parts -join "")))
    if ($roundTrip -ne "chunk-round-trip-check") {
        Fail "self-test chunk round-trip failed"
    }
    Write-Host "phase2 phantom bootstrap self-test passed" -ForegroundColor Green
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

if ($PhantomPort -lt 1 -or $PhantomPort -gt 65535) {
    Fail "-PhantomPort must be from 1 to 65535"
}
if ($ServicePort -lt 0 -or $ServicePort -gt 65535) {
    Fail "-ServicePort must be from 1 to 65535"
}
if ($BuildWaitSecs -lt 1) {
    Fail "-BuildWaitSecs must be positive"
}
Assert-WindowsExeUrlPolicy

$fleet = Read-Fleet
$nodes = Select-Nodes (Get-FleetNodes $fleet)
$port = Get-FleetServicePort $fleet
$secret = if (Test-ActionRequiresSecret $Action) { Resolve-Secret } else { "" }

$results = @()
foreach ($n in $nodes) {
    try {
        $results += Invoke-NodeAction $n $port $secret
    }
    catch {
        $results += [pscustomobject]@{
            node = $n
            phantom = "error"
            os = $null
            ensemble = "unknown"
            action = $Action
            ok = $false
            detail = $_.Exception.Message
        }
    }
}

if ($Json) {
    $results | ConvertTo-Json -Depth 4
}
else {
    $results | Format-Table -AutoSize
}

if (@($results | Where-Object { -not $_.ok }).Count -gt 0) {
    exit 1
}
