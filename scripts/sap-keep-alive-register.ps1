<#
  Registers sap-keep-alive.ps1 as a Windows Scheduled Task, correctly this
  time (#495). The version that shipped alongside #453 was registered ad
  hoc, without a -Principal, which ties task execution to whichever
  interactive logon happened to exist at registration time - if that
  session later disconnects (an RDP session going from active to
  disconnected does not log the account off, but does detach it from the
  console's window station), the task either stops firing or runs with no
  attached desktop to interact with, and SendKeys/AppActivate calls
  silently miss the real SAP GUI window while the heartbeat still logs
  "ok".

  Also drops the interval from 5 minutes to 1: the observed idle timeout on
  the reference system is closer to ~2 minutes (#495), so a 5-minute
  heartbeat can never beat it even when the task does fire correctly.

  Run this ONCE, interactively, logged on as the account whose desktop
  actually shows SAP GUI (elevated PowerShell). It replaces any existing
  registration of the same task name.

  Precondition: SAP_CONNECTION, SAP_USER, SAP_PASSWORD (and optionally
  SAP_CLIENT) must already be set as persistent environment variables for
  this account (System Properties > Environment Variables, or
  [Environment]::SetEnvironmentVariable(name, value, 'User')) - a scheduled
  task action has no field for them, and putting credentials on the task's
  command line would leave them readable via `schtasks /query` and Task
  Scheduler's own UI.
#>

param(
    [string]$UserName = $env:USERNAME,
    [string]$ScriptPath = (Join-Path $PSScriptRoot 'sap-keep-alive.ps1'),
    [int]$IntervalMinutes = 1
)

$ErrorActionPreference = 'Stop'

$taskName = 'flowproof-sap-keep-alive'

Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue |
    Unregister-ScheduledTask -Confirm:$false

$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$ScriptPath`""

$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date) `
    -RepetitionInterval (New-TimeSpan -Minutes $IntervalMinutes)
$trigger.Repetition.Duration = ''  # repeat indefinitely, never stop re-firing

# Interactive, bound to a named account rather than whoever happens to run
# this registration - so the task keeps a stable identity across a reboot,
# and can be inspected afterwards to confirm it's actually bound to the
# account whose desktop shows SAP GUI:
#   schtasks /query /tn flowproof-sap-keep-alive /v /fo list
# (check "Run As User"). LogonType Interactive still requires that account
# to have an ACTIVE console session when the trigger fires - an RDP
# session that's merely disconnected (not logged off) does not count.
$principal = New-ScheduledTaskPrincipal -UserId $UserName -LogonType Interactive -RunLevel Highest

Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal | Out-Null

Write-Host "Registered '$taskName': $UserName, every $IntervalMinutes minute(s), running $ScriptPath"
Write-Host 'Verify with: schtasks /query /tn flowproof-sap-keep-alive /v /fo list'
Write-Host "Then check %LOCALAPPDATA%\flowproof\keep-alive.log after a couple of minutes for 'ok' lines."
