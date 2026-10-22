**`record --reuse` reuses web steps again.** Since web recording began
putting an accessibility selector (role and name) first in each step, reuse
rebuilt a step's target from that selector alone, could not, and quietly
asked the model to author the step again. Every web step with an accessible
name was re-authored on reuse, costing a model call each and saving nothing.
Reuse now reads past that hint to the selector the step was recorded with, so
a step whose target is still on screen replays with no model call.
