**`app: vision` clicks the name it was asked for before one that merely
begins with it.** In File Explorer, `Click "ssis_pc_data"` opened the
project file `ssis_pc_data.dtproj` instead of the folder: OCR had read the
folder as `ssis_ pc_ data`, with spaces after its underscores, and a prefix
match on the name as written was tried before any looser whole match. Every
whole match - as written, ignoring case, ignoring edge punctuation, ignoring
the spaces OCR inserts - is now tried before any prefix, so a prefix is
reached only when nothing on screen is the name itself.
