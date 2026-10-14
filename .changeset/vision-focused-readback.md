**A bare `Type X` on `app: vision` fails when the text never appears.** Typing
into whatever has focus was the one kind of typing vision did not read back,
so after a double-click that failed to open its input, `Type Zebra42` typed
into nothing and passed. It is now read back like typing into a named field,
by the value's first characters rather than its last - the ones that sit
against the text cursor, which OCR reads poorly. That also makes a separate
`page shows` for text just typed unnecessary. A value from a `${...}`
reference is still never read back.
