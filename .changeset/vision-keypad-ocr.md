**`app: vision` reads a keypad key by key.** OCR grouped every word on a row
into one line, so Windows Calculator's keypad row `7  8  9  ×` - four buttons
with gaps several glyphs wide - was recognised as `8 9X`: the `7` lost across
the gaps, and the `8` reachable only as part of a phrase. A row is now split
wherever its words stand further apart than any words of one phrase, so a
keypad's keys, a toolbar's buttons and a grid's columns are each a line of
their own. A thin glyph alone on a key, like Calculator's `1`, is too slight
for the text detector at screen size: a target that is missed is now looked
for again in the same frame read at two and then three times the size, each
lone glyph given the margin a line of text would have. And a frame already
read is not read again - one step reads the screen several times (reveal,
wait, act), which cost a replayed Calculator flow a minute and a half of
re-reading; it now replays in eleven seconds.
