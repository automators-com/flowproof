<#
  Heartbeat for a SAP GUI session kept logged in on a self-hosted VM for
  interactive recording/running (see #438). Between flow runs, nothing
  touches that session - if it idles out ("Maximum idle time exceeded"),
  nobody notices until the next flow fails, possibly hours later.

  This does not duplicate the recovery logic: connect() in sap_com.rs
  already auto-logs in whenever it finds a live session sitting at SAP's
  login screen, given SAP_USER/SAP_PASSWORD (see #226 / commit 0e32af0).
  The nightly sap-e2e.yml workflow already benefits from this on every
  scheduled run. What's missing for a human's interactive session is
  something that ATTACHES on a timer, so the same self-healing kicks in
  before the next real flow needs the session - so this script bootstraps
  the session and then runs the existing "nothing to get wrong" ping flow.

  Intended to be registered as a Windows Scheduled Task - see
  sap-keep-alive-register.ps1, which registers it correctly. Observed idle
  timeout on the reference system is closer to ~2 minutes than the 5-10
  originally assumed here (#495), so the interval has to comfortably clear
  that, not a round "every few minutes" number. The task's action must
  supply SAP_CONNECTION/SAP_USER/SAP_PASSWORD (and optionally SAP_CLIENT)
  as environment variables - this script does not read or store them
  itself beyond passing them through.

  Exits non-zero on failure (visible in Task Scheduler's own history) and
  appends one timestamped line per run to the log below, so "when did this
  last succeed" doesn't require digging through Task Scheduler at all.
#>

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$pingFlow = Join-Path $repoRoot 'examples\sap\session-status.flow.yaml'

$logDir = Join-Path $env:LOCALAPPDATA 'flowproof'
$logFile = Join-Path $logDir 'keep-alive.log'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

function Write-Log([string]$line) {
    $timestamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    Add-Content -Path $logFile -Value "$timestamp $line"
}

try {
    & (Join-Path $PSScriptRoot 'sap-session-bootstrap.ps1')

    $output = & npx flowproof run $pingFlow 2>&1
    $exitCode = $LASTEXITCODE

    if ($exitCode -eq 0) {
        Write-Log "ok"
    } else {
        $tail = ($output | Select-Object -Last 10) -join ' | '
        Write-Log "FAILED (exit $exitCode): $tail"
        exit $exitCode
    }
} catch {
    Write-Log "FAILED (exception): $($_.Exception.Message)"
    exit 1
}
