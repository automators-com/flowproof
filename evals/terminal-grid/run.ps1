# Record, then replay, every flow here against the terminal replica, under
# each Citrix-like profile, and score what the replica holds afterwards.
#
#   powershell -File evals/terminal-grid/run.ps1 [-Engine <flowproof.exe>]
#       [-Replica <flowproof-terminal-replica.exe>] [-Out <dir>] [-Only <flow>]
#       [-Profile clean|citrix-light|citrix-heavy]
#
# No model is needed: flows are recorded with --author rules. A flow passes
# only when the engine says it passed AND the replica's own state matches
# <flow>.expect.json, so a step that typed into the wrong field is caught
# even when every assertion was green.
param(
    [string]$Engine = "flowproof",
    [string]$Replica = "",
    [string]$Out = (Join-Path $env:TEMP "flowproof-terminal-grid"),
    [string]$Only = "",
    [string]$Profile = ""
)
# Not "Stop": Windows PowerShell turns a native command's stderr into a
# terminating error, and the engine reports progress on stderr.
$ErrorActionPreference = "Continue"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Replica) {
    $root = Resolve-Path (Join-Path $here "..\..")
    $Replica = @("release", "debug") | ForEach-Object { Join-Path $root "target\$_\flowproof-terminal-replica.exe" } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $Replica) { throw "build the replica first: cargo build -p flowproof-terminal-replica" }
}
$layout = Join-Path $here "leistung.yaml"
New-Item -ItemType Directory -Force $Out | Out-Null
# Over Citrix there is no accessibility tree; make the local window match.
$env:FLOWPROOF_VISION_UIA = "0"

$profiles = [ordered]@{
    "clean"        = @()
    "citrix-light" = @("--jpeg", "70")
    "citrix-heavy" = @("--jpeg", "35", "--scale", "0.8", "--bits", "16")
}

function Start-Replica([string[]]$extra, [string]$state) {
    Remove-Item $state -ErrorAction SilentlyContinue
    $p = Start-Process $Replica -ArgumentList (@("`"$layout`"", "--state", "`"$state`"") + $extra) -PassThru
    for ($i = 0; $i -lt 50 -and -not (Test-Path $state); $i++) { Start-Sleep -Milliseconds 100 }
    Start-Sleep -Milliseconds 700
    $p
}

# Every "path" in the expect file, walked through the state JSON, must hold
# its value. Returns the mismatches.
function Compare-State([string]$state, [string]$expect) {
    if (-not (Test-Path $state)) { return @("no state written") }
    $actual = Get-Content $state -Raw -Encoding UTF8 | ConvertFrom-Json
    $wanted = Get-Content $expect -Raw -Encoding UTF8 | ConvertFrom-Json
    $misses = @()
    foreach ($prop in $wanted.PSObject.Properties) {
        $node = $actual
        foreach ($part in $prop.Name.Split('.')) {
            if ($null -eq $node) { break }
            $node = if ($node -is [array]) { $node[[int]$part] } else { $node.$part }
        }
        if ("$node" -ne "$($prop.Value)") { $misses += "$($prop.Name): wanted '$($prop.Value)', got '$node'" }
    }
    $misses
}

$results = @()
foreach ($name in $profiles.Keys) {
    if ($Profile -and $name -ne $Profile) { continue }
    foreach ($spec in Get-ChildItem (Join-Path $here "flows") -Filter *.flow.yaml) {
        $flow = $spec.Name -replace '\.flow\.yaml$', ''
        if ($Only -and $flow -ne $Only) { continue }
        $expect = Join-Path $spec.DirectoryName "$flow.expect.json"
        $base = Join-Path $Out "$name.$flow"
        $trace = "$base.trace.jsonl"
        Remove-Item $trace -ErrorAction SilentlyContinue

        $p = Start-Replica $profiles[$name] "$base.record.json"
        # --no-repair: a failure is the measurement, and repair would edit the spec.
        & $Engine record $spec.FullName --author rules --no-repair --out $trace *> "$base.record.log"
        $recorded = $LASTEXITCODE -eq 0
        Stop-Process $p -ErrorAction SilentlyContinue
        $recordMisses = Compare-State "$base.record.json" $expect

        $replayed = $false; $replayMisses = @("not recorded")
        if ($recorded) {
            $p = Start-Replica $profiles[$name] "$base.run.json"
            & $Engine run $trace *> "$base.run.log"
            $replayed = $LASTEXITCODE -eq 0
            Stop-Process $p -ErrorAction SilentlyContinue
            $replayMisses = Compare-State "$base.run.json" $expect
        }
        $results += [pscustomobject]@{
            Profile  = $name
            Flow     = $flow
            Record   = if ($recorded -and -not $recordMisses) { "pass" } elseif ($recorded) { "wrong state" } else { "fail" }
            Replay   = if ($replayed -and -not $replayMisses) { "pass" } elseif ($replayed) { "wrong state" } else { "fail" }
            Mismatch = (@($recordMisses) + @($replayMisses) | Where-Object { $_ -and $_ -ne "not recorded" } | Select-Object -Unique) -join "; "
        }
    }
}
$results | Format-Table -AutoSize -Wrap
$results | Export-Csv (Join-Path $Out "results.csv") -NoTypeInformation -Encoding UTF8
"Logs, traces and states: $Out"
