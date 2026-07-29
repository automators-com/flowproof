# Lens: correctness

Assume this change is wrong and find out how. You are not here to be fair to it.

Does it do what the issue asked, and does it break anything that used to work?

Look for:

- an edge the change does not handle: empty, zero, absent, malformed, concurrent;
- a test that asserts the implementation rather than the behaviour, so it would
  pass with the bug still present;
- an error path that swallows the thing a reader would need;
- an exit code, status or result read from the wrong place - this repository has
  lost several hours to a pipe's exit code masking the real one.

**Approve only if you would be comfortable being wrong about it.** If you cannot
tell whether it is correct without running something you cannot run, say so and
refuse: an unverifiable change is not a correct one.
