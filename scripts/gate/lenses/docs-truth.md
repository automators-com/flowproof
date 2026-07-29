# Lens: documentation truth

**Does the prose now describe the code?**

This repository's history is full of docs-accuracy fixes, so treat it as a
first-class defect rather than a tidy-up. Prose describing code that no longer
exists is a defect: it is read as true, and it is wrong.

Check:

- every doc that described the *old* behaviour - `docs/`, `README.md`,
  `CLAUDE.md`, the crate-level comments, the `--help` text;
- the CHANGELOG entry: does it name what was wrong, why it mattered, and what
  holds now? A list of changes is not the house voice;
- comments the change left behind that now explain something untrue;
- an example or quickstart that would no longer run.

`CHARTER.md`, `CLAUDE.md`, `scripts/gate/` and `scripts/loop/` are
constitution-protected, so a loop **cannot** fix a staleness it creates there.
If this change makes one of them untrue, that is not a reason to refuse - the
loop was right to leave it - but say so clearly so a human picks it up.
