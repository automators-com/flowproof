- **`author-from-doc` drafts several test cases of a test-set report in one
  go.** Repeat `--test-case`, or give `--test-case all`. `--out` is then a
  folder, and each test case gets its own flow, named after it: the name
  without the ID, unless two would then be the same. Before, each test case
  of a 17-run report took a separate command, each re-reading the PDF.
  - A test case the model fails on is reported, and the rest are still
    drafted. The command exits 1 when some failed, 2 when none were drafted.
  - Nothing is drafted if any file it would write already exists, so a rerun
    can't replace drafts someone has started editing.
  - Progress reads `drafting test case 3 of 17, step 2 of 6`.
  - `--json` returns `{"drafts": [...], "failed": [...]}`.
  - One `--test-case`, or none, works as before.
