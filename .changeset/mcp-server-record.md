**`flowproof mcp` can record flows too.** `flowproof_record` performs a flow
against the live app and writes its trace, the same as `flowproof record
--json`. When a step cannot be authored, the agent gets `needs_clarification`
(what was ambiguous and what the screen offered) as an ordinary result, not as
an error, so it can reword the step and record again instead of stopping. It
runs headless unless asked otherwise, even though `record` on its own shows the
browser.
