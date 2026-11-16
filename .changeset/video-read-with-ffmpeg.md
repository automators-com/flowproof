**Drafting from a screen recording no longer needs a browser.**
`author-from-video` read a recording by playing it in Chrome or Edge, so it
read only what that browser could play: a Chromium build without proprietary
codecs could not open an H.264 `.mp4` at all (#838), OBS `.mkv` and `.avi`
files were hit and miss, and the error advised re-exporting as H.264 even when
the file already was. It also tied a recording of SAP GUI or a desktop app to
a browser it had nothing to do with. Recordings are now read by a decode-only
ffmpeg that ships with flowproof: H.264, VP8/VP9, AV1 and ProRes in `.mp4`,
`.mov`, `.webm`, `.mkv` or `.avi`, decoded in one pass instead of one seek and
screenshot per sample. HEVC (H.265) is left out of that build on purpose and
says to export as H.264; `--check` now fails on a file it can describe but not
decode. `FLOWPROOF_FFMPEG` points at another ffmpeg.
