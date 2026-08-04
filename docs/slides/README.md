# Flowproof presentation decks

These are the editable, design-system-aligned sources for the Flowproof 0.12.2
training material.

- `flowproof-training-0.12.2.pptx` and `.pdf`: the teaching deck.
- `flowproof-solutions-0.12.2.pptx` and `.pdf`: the verified obstacle-course
  solutions and honest refusals.
- `deck-source.mjs`: the editable content and layout source used to generate the
  two PowerPoint files.

The source follows `DESIGN.md`, which pins Automators design system 1.0.1. It
does not overlay or rasterize an older PDF. Every visible element in the PPTX is
native and editable; PDFs are exported from those PPTX files.

Plain scalar action steps are written as human intent. `assert:`, `repeat:` and
`when:` remain structured because Flowproof 0.12.2 gives those forms explicit,
deterministic semantics. A build-time validation rejects selectors and
`rules:` syntax from every action designated as human-authored.
