**The model recording a step is told what the earlier steps did, not only
what they said.** A computer-use model keeps the actions it took in its
context. flowproof told the model authoring a step only the wording of the
ones before it - "Work out 7 plus 8" - so a follow-on step such as "now
multiply it by 3" or "close it" had to be worded as if nothing had happened.
Each earlier step is now listed with what it did: which controls were
clicked, what was typed, what was remembered (by name; a `${...}` value and a
remembered value are never shown), at most twelve actions per step. Only
recording changes: the trace format is the same and replay still makes no
model calls.
