**`app: vision` scrolls to find what is below the fold.** Pixels-only mode
read the one frame the window showed and never scrolled, so every field
below the first screen of a form was "not present on the current screen" -
five of six flows translated from Cypress's kitchensink failed on it
before reaching anything else, and a long Citrix form fails the same way.
A click, type, clear or read now looks for its text by turning the mouse
wheel a page at a time, down to the bottom and back up to the top, and acts
where it finds it; replay runs the same search from the same starting
page, so it stops in the same place. Existence checks still read only the
current frame, because auto-wait polls them and a poll that scrolled would
move the page every tick. `scroll to top`, `to bottom` and `into view` now
work on vision surfaces as well. Recording does the same before the model
is asked: a step's quoted label that is off screen is scrolled into view
first, because a model shown only the visible frame reported the field
missing and stopped the recording before the driver could look.
