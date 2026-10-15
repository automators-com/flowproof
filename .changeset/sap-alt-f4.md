**`Alt+F4` closes a SAP window.** Two things stood in the way. When the
model chose the key itself, its `"Alt+F4"` went to the driver as one key name
with no modifiers. A `Press Alt+F4` step split it correctly; the model's own
key press did not. And even split, SAP refused it: its scripting key table
has no Alt chords, so every SAP flow that closed a window that way stopped
with "no virtual key for 'Alt+F4'". The model's key press now goes through
the same chord grammar as a spec step, and on SAP `Alt+F4` closes the active
window natively, the modal on top first, as the keystroke would.
