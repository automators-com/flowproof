# Grounding benchmark for the vision adapter on this machine's desktop apps.
#
#   powershell -File scripts/vision-bench/run.ps1 [-Exe <vision_bench.exe>] [-Modes ocrs,windows,auto,auto+uia] [-Misses]
#
# Opens each app, then scores vision against the app's own UIA tree in each
# mode (see crates/flowproof-adapters/src/vision_bench.rs): whether a
# `Click "<name>"` lands inside the control of that name. Modes:
#   ocrs          - the pure-Rust OCR engine alone (the pre-2026-10 engine)
#   windows       - Windows.Media.Ocr alone
#   auto          - both engines merged, pixels only (what a Citrix window gets)
#   auto+uia      - both, fused with the accessibility tree (the default locally)
# Pixels-only modes set FLOWPROOF_VISION_UIA=0, which is also what a Citrix or
# RDP window gets anyway. Notepad is not launched - on
# Windows 11 that opens a tab in whatever Notepad window is already open, and
# someone's unsaved document may be in it - but one already open is scored.
# The apps it opened are closed at the end (File Explorer and Control Panel
# live in explorer.exe, and stay).
param(
    [string]$Exe = "target/release/examples/vision_bench.exe",
    [string[]]$Modes = @("ocrs", "windows", "auto", "auto+uia"),
    [switch]$Misses
)

$extra = @("Notepad")
$apps = @(
    @{ Launch = "calc.exe";        Title = "Calculator";    Process = "CalculatorApp" },
    @{ Launch = "mspaint.exe";     Title = "Paint";         Process = "mspaint" },
    @{ Launch = "charmap.exe";     Title = "Character Map"; Process = "charmap" },
    @{ Launch = "control.exe";     Title = "Control Panel" },
    @{ Launch = "explorer.exe";    Title = "File Explorer" },
    @{ Launch = "ms-settings:";    Title = "Settings";      Process = "SystemSettings" },
    @{ Launch = "ms-clock:";       Title = "Clock";         Process = "Time" }
)

foreach ($app in $apps) {
    Start-Process $app.Launch -ErrorAction SilentlyContinue
}
Start-Sleep -Seconds 10

$titles = @($apps | ForEach-Object { $_.Title }) + $extra
foreach ($mode in $Modes) {
    $engine = $mode.Split("+")[0]
    $env:FLOWPROOF_VISION_OCR = if ($engine -eq "auto") { "" } else { $engine }
    $env:FLOWPROOF_VISION_UIA = if ($mode -like "*+uia") { "1" } else { "0" }
    Write-Host "== $mode =="
    $benchArgs = @($titles)
    if ($Misses) { $benchArgs += "--misses" }
    & $Exe @benchArgs
}

foreach ($app in $apps) {
    if ($app.Process) {
        Get-Process $app.Process -ErrorAction SilentlyContinue | Stop-Process -ErrorAction SilentlyContinue
    }
}
