- **`author-from-doc` reads an ALM report that went through Word.** Exported
  to Word and printed to PDF, an HP ALM run report numbers its headings
  (`1.1.1.1 Step Name : Step 1`). The parser only looked for `Step Name :` at
  the start of a line, so it found no steps and refused a real ALM export as
  having none. Numbered headings now read, the page header that
  lands mid-step is dropped, a field mentioning "Actual" in its prose is no
  longer cut short, and steps come out in step-number order. A test-set report
  holds a run per test case, and drafting all of them into one flow would mix
  unrelated tests, so such a report is now read as separate test cases (the
  latest run of each). For now it is refused with the number of test cases it
  holds.
