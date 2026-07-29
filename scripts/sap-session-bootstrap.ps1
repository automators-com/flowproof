<#
  Run before and after sap_e2e on the self-hosted `sap` runner. Unattended
  (nightly) runs have nobody at the keyboard, so this closes the small set
  of modal dialogs known to block SAP GUI Scripting - a multiple-logon
  prompt or a license-information notice left open stalls FindById forever
  otherwise - and makes sure SAP Logon itself is running.

  Best-effort: the dialog titles below are the common English ones seen in
  practice, not an exhaustive list. If a future dialog isn't covered, add
  its title here rather than reworking the loop.
#>

$ErrorActionPreference = 'Stop'

$blockingTitles = @(
    'License Information for Multiple Logon',
    'Information'
)

$shell = New-Object -ComObject WScript.Shell

foreach ($title in $blockingTitles) {
    Get-Process | Where-Object { $_.MainWindowTitle -eq $title } | ForEach-Object {
        if ($shell.AppActivate($_.Id)) {
            Start-Sleep -Milliseconds 300
            $shell.SendKeys('~')  # Enter - accept the dialog's default button
            Write-Host "Dismissed blocking dialog: $title"
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

Write-Host 'SAP session bootstrap complete.'
