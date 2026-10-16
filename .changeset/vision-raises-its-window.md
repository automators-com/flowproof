**`app: vision` reads and clicks its own window, not the one in front of
it.** Vision captured the screen where its window was and injected input
there, but never confirmed the window was in front: Windows refuses
`SetForegroundWindow` to a process the user is not working in, which is
flowproof whenever it is started from a terminal or an IDE, and the refusal
was ignored. A Calculator flow started from a window covering Calculator
OCR'd that window's sidebar and reported `5` as not on screen - a likely
reading of "vision struggles with a simple calculator". Vision now lifts the
foreground lock when Windows refuses, waits for the window to finish painting
before its first read, raises it again whenever another program takes the
foreground mid-run (its own popups and menus are left alone), and fails
naming the window in the way when it still cannot. Capture also stops taking
in the strip behind a window's invisible resize border, where OCR read stray
letters that were not the window's own.
