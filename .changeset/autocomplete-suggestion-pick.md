**A typed value is picked from its field's suggestion list.** An address
field that only accepts a value chosen from its autocomplete list (a
postcode, a street) was typed into and never picked: the list stayed open
over the next button, whose click was refused as occluded, and the fields
that depend on the pick stayed disabled. When recording, after typing, a
suggestion list that belongs to the field and offers exactly the typed text
once is clicked, and that click is written into the trace as its own step,
so replay is a plain type and a plain click and older traces replay as
before. The list is the field's `aria-controls`/`aria-owns` target, or a
`role=listbox` beside it, since many such fields carry no ARIA at all. No
near match is ever picked, search boxes are left alone, and a `${...}`
value is never written into the trace as a click target.
