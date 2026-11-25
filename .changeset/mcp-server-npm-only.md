**`flowproof mcp` no longer points at a `cargo install` that does not exist.**
Under the Python package, the server's startup error and its docs suggested
installing flowproof with cargo, but flowproof is not published to crates.io.
They now point at the npm package, whose `flowproof` binary the server can
spawn.
