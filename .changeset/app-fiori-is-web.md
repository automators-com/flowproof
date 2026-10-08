- **`app: fiori` records as a web app.** A Fiori launchpad is a web app, and
  the engine only knew it as `web`. The desktop app writes `app: fiori` for
  every Fiori flow and surface, so each one failed before it started: "unknown
  app 'fiori'" for a flow, and "`url:` … means nothing for `app: fiori`" for a
  surface. No Fiori flow made there could be recorded or run. `fiori` is now
  read as `web` wherever `app:` is written, alone or on a surface, and the
  trace records `web`, so replays and existing traces are unaffected.
