**The repair model is told which steps already ran.** For any recording
failure other than a clarification request - an element not found, an
assertion that did not hold - the repair prompt said no steps had completed
before the failure, even when several had. The model took the flow for one
that never got going and declined to patch the failing step on that basis.
The prompt now names every flow step that passed.
