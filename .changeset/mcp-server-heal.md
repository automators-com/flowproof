**`flowproof mcp` can propose repairs.** `flowproof_heal` re-drives a flow
whose app has drifted and writes the proposal beside the trace, as `flowproof
heal --json` does. The committed trace is left alone unless the call passes
`apply` or `apply_proposal`, and the tool's description tells the agent to pass
those only after the person has reviewed the proposal.
