**A field below its label is clicked in its middle, whatever height OCR gives
the label.** Vision aimed one label-height under a label, and a label's height
is whatever OCR measures it to be: it measured `Describe:` at 10 pixels, and
the click fell in the gap between the label and its box, so the value went
nowhere. It now aims one and a half line-heights under the label, with the
height floored at 14 pixels - the middle of where an input box sits.
