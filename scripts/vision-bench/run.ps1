# Grounding benchmark for the vision adapter on this machine's desktop apps.
#
#   powershell -File scripts/vision-bench/run.ps1 [-Exe <vision_bench.exe>] [-Modes ocrs,windows,windows+uia]
#
# Opens each app, then scores vision against the app's own UIA tree in each
# mode (see crates/flowproof-adapters/src/vision_bench.rs): whether a
# `Click "<name>"` lands inside the control of that name. Modes:
#   ocrs          - the pure-Rust OCR engine alone (the pre-2026-10 engine)
#   windows       - Windows.Media.Ocr alone
#   windows+uia   - Windows OCR fused with the accessibility tree (default)
# Pixels-only modes set FLOWPROOF_VISION_UIA=0, which is also what a Citrix or
# RDP window gets anyway. Apps are left open.
param(
    [string]$Exe = "target/release/examples/vision_bench.exe",
    [string[]]$Modes = @("ocrs", "windows", "windows+uia"),
    [switch]$Misses
)

$apps = @(
    @{ Launch = "calc.exe";        Title = "Calculator" },
    @{ Launch = "notepad.exe";     Title = "Notepad" },
    @{ Launch = "mspaint.exe";     Title = "Paint" },
    @{ Launch = "charmap.exe";     Title = "Character Map" },
    @{ Launch = "control.exe";     Title = "Control Panel" },
    @{ Launch = "explorer.exe";    Title = "File Explorer" },
    @{ Launch = "ms-settings:";    Title = "Settings" },
    @{ Launch = "ms-clock:";       Title = "Clock" }
)

foreach ($app in $apps) {
    Start-Process $app.Launch -ErrorAction SilentlyContinue
}
Start-Sleep -Seconds 6

$titles = $apps | ForEach-Object { $_.Title }
foreach ($mode in $Modes) {
    $env:FLOWPROOF_VISION_OCR = $mode.Split("+")[0]
    $env:FLOWPROOF_VISION_UIA = if ($mode -like "*+uia") { "1" } else { "0" }
    Write-Host "== $mode =="
    $benchArgs = @($titles)
    if ($Misses) { $benchArgs += "--misses" }
    & $Exe @benchArgs
}
