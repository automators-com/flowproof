<#
  Run before and after sap_e2e on the self-hosted `sap` runner. Unattended
  (nightly) runs have nobody at the keyboard, so this closes the small set
  of modal dialogs known to block SAP GUI Scripting - a multiple-logon
  prompt or a license-information notice left open stalls FindById forever
  otherwise - and makes sure SAP Logon itself is running.

  Also closes the idle/connection-lost notice ("TS3: auto logout (maximum
  user idle time exceeded)", see #495) SAP GUI raises when the backend
  session it was attached to has already timed out server-side. That one is
  a plain Win32 message box the GUI process pops up itself, not a
  GuiModalWindow the scripting object model exposes - FindById can't see it
  and connect()'s login-screen recovery never gets a chance to run while
  it's sitting open. It appears as TWO dialogs in a row sharing the exact
  same title ("SAP GUI for Windows 760") - a Yes/No prompt ("do you want to
  see the detailed error description?"), and if Yes is pressed, a second
  screen with structured error detail and a single OK button (confirmed
  live, see #499). "Yes" and "OK" are each the default button on their
  screen, so Enter clears both without needing to tell them apart.
  Dismissing the chain closes the dead session entirely and lands on the
  bare SAP Logon connection picker, not an in-place login screen -
  connect()'s existing OpenConnection() call in sap_com.rs is the COM
  equivalent of clicking "Log On" there, so nothing further is needed here.

  Dialogs can be STACKED or repeated - the two idle-timeout screens above
  are exactly that. `Get-Process().MainWindowTitle` reports exactly one
  window per process, and SAP GUI hosts its main session frame AND every
  modal dialog under one `saplogon.exe` process - so that lookup can only
  ever see one of several open windows, no matter how many times it's
  called. Detection below instead enumerates every top-level window the
  process owns and loops until none of the known ones remain.

  Best-effort: the dialog titles below are the common English ones seen in
  practice, not an exhaustive list. If a future dialog isn't covered, this
  script now fails loudly with its captured title/class/child-control text
  and a screenshot (see `sap-bootstrap-diagnostics/`) instead of silently
  doing nothing - add it here once its real text is known, rather than
  guessing.
#>

$ErrorActionPreference = 'Stop'

Add-Type -Namespace SapBootstrap -Name NativeMethods -MemberDefinition @'
    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool EnumChildWindows(IntPtr hWndParent, EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll", CharSet = CharSet.Auto)]
    public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Auto)]
    public static extern int GetClassName(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
'@

function Get-Win32WindowText([IntPtr]$Handle) {
    $sb = New-Object System.Text.StringBuilder 512
    [SapBootstrap.NativeMethods]::GetWindowText($Handle, $sb, $sb.Capacity) | Out-Null
    return $sb.ToString()
}

function Get-Win32ClassName([IntPtr]$Handle) {
    $sb = New-Object System.Text.StringBuilder 256
    [SapBootstrap.NativeMethods]::GetClassName($Handle, $sb, $sb.Capacity) | Out-Null
    return $sb.ToString()
}

function Get-Win32WindowArea([IntPtr]$Handle) {
    $rect = New-Object SapBootstrap.NativeMethods+RECT
    if ([SapBootstrap.NativeMethods]::GetWindowRect($Handle, [ref]$rect)) {
        return ([math]::Max(0, $rect.Right - $rect.Left)) * ([math]::Max(0, $rect.Bottom - $rect.Top))
    }
    return 0
}

# Every visible top-level window owned by any process with the given
# name(s) - see the file header for why this replaces a
# Get-Process().MainWindowTitle lookup.
function Get-TopLevelWindows([string[]]$ProcessNames) {
    $ownerPids = @()
    foreach ($name in $ProcessNames) {
        Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object { $ownerPids += $_.Id }
    }
    if (-not $ownerPids) { return @() }

    $results = New-Object System.Collections.Generic.List[object]
    [SapBootstrap.NativeMethods+EnumWindowsProc]$callback = {
        param([IntPtr]$hWnd, [IntPtr]$lParam)
        if ([SapBootstrap.NativeMethods]::IsWindowVisible($hWnd)) {
            $windowPid = [uint32]0
            [SapBootstrap.NativeMethods]::GetWindowThreadProcessId($hWnd, [ref]$windowPid) | Out-Null
            if ($ownerPids -contains [int]$windowPid) {
                $title = Get-Win32WindowText $hWnd
                if ($title) {
                    $results.Add([PSCustomObject]@{
                        Handle    = $hWnd
                        Title     = $title
                        ClassName = Get-Win32ClassName $hWnd
                        Area      = Get-Win32WindowArea $hWnd
                    }) | Out-Null
                }
            }
        }
        return $true
    }
    [SapBootstrap.NativeMethods]::EnumWindows($callback, [IntPtr]::Zero) | Out-Null
    return $results
}

function Get-ChildControlTexts([IntPtr]$Handle) {
    $texts = New-Object System.Collections.Generic.List[string]
    [SapBootstrap.NativeMethods+EnumWindowsProc]$callback = {
        param([IntPtr]$childHwnd, [IntPtr]$lParam)
        $text = Get-Win32WindowText $childHwnd
        if ($text) { $texts.Add($text) | Out-Null }
        return $true
    }
    [SapBootstrap.NativeMethods]::EnumChildWindows($Handle, $callback, [IntPtr]::Zero) | Out-Null
    return $texts
}

function Save-DiagnosticScreenshot([string]$Path) {
    Add-Type -AssemblyName System.Drawing, System.Windows.Forms
    $bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

# The idle-notice title covers TWO screens in a row (see file header) -
# "Yes" and "OK" are each the default button on their respective screen, so
# Enter (~) clears both without needing to distinguish them.
$blockingDialogs = @(
    @{ Title = 'License Information for Multiple Logons'; Keys = '~' }
    @{ Title = 'Information'; Keys = '~' }
    @{ Title = 'SAP GUI for Windows 760'; Keys = '~' }
)

$shell = New-Object -ComObject WScript.Shell

# Dismiss every known blocking dialog, looping because closing one can
# reveal another - either a different dialog stacked underneath, or (the
# idle-notice case) a second screen sharing the same title. The main SAP
# frame is identified as the largest window this process owns, rather than
# hardcoding a main-window title that varies by locale/screen/version -
# everything else this process owns is a dialog candidate. Anything left
# that isn't in $blockingDialogs is now a loud, diagnosable failure
# (captured title, class, every child control's text, and a screenshot
# under sap-bootstrap-diagnostics/) instead of a silent no-op, so the next
# unknown dialog is a five-minute fix instead of a multi-day investigation.
$maxRounds = 5
for ($round = 1; $round -le $maxRounds; $round++) {
    $windows = Get-TopLevelWindows -ProcessNames @('saplogon')
    if (-not $windows -or $windows.Count -eq 0) { break }

    $mainFrame = $windows | Sort-Object Area -Descending | Select-Object -First 1
    $dialogCandidates = $windows | Where-Object { $_.Handle -ne $mainFrame.Handle }
    if (-not $dialogCandidates -or $dialogCandidates.Count -eq 0) { break }

    $anyDismissed = $false
    foreach ($window in $dialogCandidates) {
        $known = $blockingDialogs | Where-Object { $_.Title -eq $window.Title } | Select-Object -First 1
        if ($known) {
            if ($shell.AppActivate($window.Title)) {
                Start-Sleep -Milliseconds 300
                $shell.SendKeys($known.Keys)
                Write-Host "Dismissed blocking dialog: $($window.Title)"
                $anyDismissed = $true
            }
        } else {
            $childTexts = Get-ChildControlTexts $window.Handle
            $diagDir = Join-Path $PSScriptRoot '..\sap-bootstrap-diagnostics'
            New-Item -ItemType Directory -Force -Path $diagDir | Out-Null
            $stamp = Get-Date -Format 'yyyyMMddTHHmmssZ'
            $screenshotPath = Join-Path $diagDir "$stamp-unrecognized-dialog.png"
            try {
                Save-DiagnosticScreenshot $screenshotPath
            } catch {
                Write-Host "Could not save diagnostic screenshot: $_"
            }
            Write-Error ("An unrecognized blocking window is on screen and this script does not know how to dismiss it. " + `
                "Title: '$($window.Title)' | Class: '$($window.ClassName)' | " + `
                "Child control text: $($childTexts -join ' | ') | Screenshot: $screenshotPath")
        }
    }
    if (-not $anyDismissed) { break }
    Start-Sleep -Milliseconds 500
}

if (-not (Get-Process -Name saplogon -ErrorAction SilentlyContinue)) {
    if (-not $env:SAP_CONNECTION) {
        Write-Error "saplogon.exe is not running and SAP_CONNECTION is not set - cannot open a session unattended."
    }
    Write-Host "SAP Logon is not running - starting it."
    Start-Process 'saplogon.exe'
    Start-Sleep -Seconds 5
}

# COM automation doesn't need window focus to fire actions, but Windows can
# defer actual client-side rendering for a background window - complex
# screens (many fields) may never finish laying out if SAP GUI isn't the
# foreground window. Bring it forward here; nothing else should steal focus
# back during an unattended run.
#
# AppActivate can return false for reasons that have nothing to do with SAP
# GUI: right after the preceding cargo step exits, Windows' foreground-lock
# timeout briefly refuses to let an unrelated automation process steal focus
# from the console that just had it (see #499 - this is what silently
# poisoned every field wait in the replay step that follows). A single
# best-effort attempt isn't enough - retry, releasing the lock between
# attempts with a throwaway Alt keystroke (the standard workaround: it resets
# Windows' "user is providing input" state without doing anything to whatever
# currently has focus). If we still can't get foreground after that, fail
# loudly now rather than let the replay step run for 20 minutes against a
# window that will never finish rendering.
#
# 10 attempts / ~5s total, not 5 / 2.5s: live evidence (run 32730737479)
# showed 5 attempts over 2.5s fail, then the exact same call succeed on the
# very next invocation about 5s later - the lock is real but short-lived,
# and the old budget was cutting it close rather than genuinely stuck.
$sapProcess = Get-Process -Name saplogon -ErrorAction SilentlyContinue
if ($sapProcess -and $sapProcess.MainWindowHandle -ne 0) {
    $activated = $false
    for ($attempt = 1; $attempt -le 10; $attempt++) {
        if ($shell.AppActivate($sapProcess.Id)) {
            $activated = $true
            break
        }
        $shell.SendKeys('%')
        Start-Sleep -Milliseconds 500
    }
    if ($activated) {
        Write-Host "Brought SAP GUI to the foreground (attempt $attempt)."
    } else {
        Write-Error 'Could not bring SAP GUI to the foreground after 10 attempts (AppActivate kept returning false). Failing now instead of running the suite against a window that will never finish rendering.'
    }
}

Write-Host 'SAP session bootstrap complete.'
