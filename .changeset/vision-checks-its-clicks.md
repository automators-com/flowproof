**`app: vision` clicks again when its click went nowhere.** A computer-use
model looks at the screen after every action and tries again when nothing
happened. Replayed over pixels, a click a slow remote session swallowed
passed silently, and the step after it failed for a reason that was not its
own. Now the pointer rests on the target first, so its hover tint is in the
frame before the click, and for 1.5s after the click anything more than a
blinking caret changing counts as the click answered. If nothing changed,
the click is made once more. Only once, and it never fails the step: `0` on
a Calculator already showing 0 rightly changes nothing, and pressing a
toggle again before a slow app draws it would undo it. Such a click costs
about three seconds and is logged under `FLOWPROOF_VISION_LOG`. Double and
right clicks are not repeated.
