**A trace's recording-bundle path now resolves the same on every OS.** A trace
header's `recording.dir` is `/`-delimited by contract, which the format now
says, and `heal` reads it one `/`-separated segment at a time, joined with
the host's separator, instead of handing the raw string to `Path::join`. A
trace recorded on Windows finds its filmstrip on macOS or Linux, and the
reverse, without depending on how each OS parses the string. A `dir` that
leaves the trace directory still resolves to no frames.
