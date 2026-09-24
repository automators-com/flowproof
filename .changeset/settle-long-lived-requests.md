**A check no longer stalls a web flow that keeps a connection open.** Before
judging a text check, and before authoring a step while recording, the web
driver waits for the page to finish loading, and it treated any open request
as loading. A long-poll, an event stream, or a request whose finish event
never arrived stayed open for the life of the page, so every text check waited
out the 30-second ceiling while re-reading the whole page, and a recording
could take minutes. An open request now counts as loading only for its first
5 seconds, and event streams, sockets and media never count.
