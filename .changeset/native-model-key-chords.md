**Model-authored shortcuts now reach native input as keys and modifiers.**
`Control+l` was passed as one literal key name, so opening a browser address
bar in Citrix failed with “no virtual key”. Model actions now use the rule
grammar's chord parser and reject malformed shortcuts before execution.
Standalone `Meta`, `Win` and `Windows` also map to the native Windows key.
These fixes do not change Citrix's own local-versus-remote key forwarding.
