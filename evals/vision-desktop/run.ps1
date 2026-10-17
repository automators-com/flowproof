# Record, then replay, every vision flow here against the live desktop.
#
#   powershell -File evals/vision-desktop/run.ps1 [-Engine <flowproof.exe>] [-Uia 0|1]
#
# Needs a model backend for the plain-English steps (`flowproof config ai`).
param(
    [string]$Engine = "flowproof",
    [string]$Uia = "1"
)
$env:FLOWPROOF_VISION_UIA = $Uia
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$launch = @{ "Calculator" = "calc.exe"; "Character Map" = "charmap.exe"; "Clock" = "ms-clock:"; "Settings" = "ms-settings:" }
$results = @()
foreach ($spec in Get-ChildItem $here -Filter *.flow.yaml) {
    $title = (Select-String -Path $spec.FullName -Pattern "^\s+title:\s*(.+)$").Matches[0].Groups[1].Value.Trim()
    if ($launch.ContainsKey($title)) { Start-Process $launch[$title]; Start-Sleep -Seconds 4 }
    $trace = [IO.Path]::ChangeExtension($spec.FullName, $null).TrimEnd('.') + ".trace.jsonl"
    Remove-Item $trace -ErrorAction SilentlyContinue
    $t = Measure-Command { & $Engine record $spec.FullName --out $trace *> "$trace.record.log" }
    $recorded = $LASTEXITCODE -eq 0
    $replayed = $false
    if ($recorded) {
        & $Engine run $trace *> "$trace.run.log"
        $replayed = $LASTEXITCODE -eq 0
    }
    $results += [pscustomobject]@{ Flow = $spec.Name; Recorded = $recorded; Replayed = $replayed; Seconds = [int]$t.TotalSeconds }
}
$results | Format-Table -AutoSize
