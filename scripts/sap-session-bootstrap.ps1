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
  it's sitting open. Dismissing it (Alt+N, not the default "Yes", which
  would only open a second "detailed description" window needing its own
  dismissal) reveals whatever is actually underneath - usually SAP's login
  screen, which the existing auto-login path already knows how to handle.

  Best-effort: the dialog titles below are the common English ones seen in
  practice, not an exhaustive list. If a future dialog isn't covered, add
  it here rather than reworking the loop. The idle-notice title carries the
  SAP GUI version ("SAP GUI for Windows 760"); a different client version
  will need its own entry.
#>

$ErrorActionPreference = 'Stop'

$blockingDialogs = @(
    @{ Title = 'License Information for Multiple Logon'; Keys = '~' }
    @{ Title = 'Information'; Keys = '~' }
    @{ Title = 'SAP GUI for Windows 760'; Keys = '%n' }
)

$shell = New-Object -ComObject WScript.Shell

foreach ($dialog in $blockingDialogs) {
    Get-Process | Where-Object { $_.MainWindowTitle -eq $dialog.Title } | ForEach-Object {
        if ($shell.AppActivate($_.Id)) {
            Start-Sleep -Milliseconds 300
            $shell.SendKeys($dialog.Keys)
            Write-Host "Dismissed blocking dialog: $($dialog.Title)"
        }
    }
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
$sapProcess = Get-Process -Name saplogon -ErrorAction SilentlyContinue
if ($sapProcess -and $sapProcess.MainWindowHandle -ne 0) {
    $activated = $false
    for ($attempt = 1; $attempt -le 5; $attempt++) {
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
        Write-Error 'Could not bring SAP GUI to the foreground after 5 attempts (AppActivate kept returning false). Failing now instead of running the suite against a window that will never finish rendering.'
    }
}

Write-Host 'SAP session bootstrap complete.'
