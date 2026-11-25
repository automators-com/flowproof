**`author-from-video --frames-dir <dir>` drafts from frames chosen earlier.**
Drafting the same recording twice meant decoding it twice, and a change to
frame selection between the two runs made them incomparable. `--frames-dir`
takes the folder `--frames-out` wrote and drafts from exactly those frames,
with no ffmpeg involved. The video-to-flow evals use it to score prompt and
model changes against committed frames.
