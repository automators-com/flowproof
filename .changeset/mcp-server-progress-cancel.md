**`flowproof mcp` reports progress and can be cancelled.** A client that
sends a progress token with a call gets the engine's progress lines as they
are printed. Cancelling a call stops its engine and everything the engine
started, such as a browser or a suite shell, and closing the server cancels
the calls still running, so no process is left behind.
