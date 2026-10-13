**A vision type step fails when its value never reaches the field.** Pixels
give no way to read a field's value, so a click that missed the box - a label
above its field, an autofill popup holding focus - typed into nothing, and the
step passed anyway; one such flow replayed green while its field sat empty.
After typing, `app: vision` now checks that the value appears on screen more
often than it did before, so a page that already prints the value elsewhere
cannot vouch for it, and fails the step with the reason when it does not. A
value typed from a `${...}` reference is not read back, because a masked
password field never shows what was typed.
