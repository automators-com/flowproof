- **A machine with no browser says so.** A web flow on a machine without
  Chrome, Edge or Chromium stopped with `Could not auto detect a chrome
  executable`, which names a library's search rather than the person's next
  step, and `record --json` filed it under `flow_setup` as if the flow were
  wrong. It now starts with `no browser found`, names the browsers that work
  and the two ways to fix it (install one, or set `CHROME`), and `record --json`
  reports it as `failure.kind` `no_browser`. A `CHROME` that names a file that
  is not there is reported the same way instead of failing at launch. Chrome,
  Edge and Chromium installed per user (without admin rights), and Brave, are
  now found too; where a browser was already found, the same one is used.
