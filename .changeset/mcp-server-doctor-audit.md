**`flowproof mcp` can check its own setup and report control coverage.**
`flowproof_doctor` answers whether an authoring model is configured, or whether
SAP GUI can be reached, and gives a failed check back as a result with its fix.
`flowproof_audit` returns the control-coverage report from the last run. `doctor
--fiori` and `doctor --agent` are not offered, and asking for them returns the
reason: one logs in to a live system for real, and the other runs a command the
caller supplies.
