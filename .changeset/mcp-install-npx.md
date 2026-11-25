**`flowproof mcp install` works for a project that installs flowproof
itself.** With `npm install --save-dev flowproof`, the binary lives in
`node_modules/.bin`, which coding agents' clients do not search, so the bare
`flowproof` entry could not start. When the project has its own flowproof, the
entry now runs `npx --no flowproof mcp`, which uses that copy and fails rather
than download one if `npm install` has not run yet.
