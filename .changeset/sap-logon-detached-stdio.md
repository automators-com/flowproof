- **A `record` that started SAP Logon no longer looks busy until SAP is
  closed.** When no SAP Logon was running, flowproof started one that
  inherited flowproof's own stdout and stderr. flowproof finished and exited,
  but anything reading its output (flowproof desktop, a CI step, a `| tee`)
  waits for the pipe to close, and SAP Logon still held it. The run sat at
  "running" with the trace already written, and cancelling could not stop a
  process that had already exited. SAP Logon now starts with none of
  flowproof's handles, so the output closes when flowproof exits.
