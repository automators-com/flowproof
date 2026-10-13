**`app: vision` reads past a blinking caret and keeps its clicks inside the
window.** A text caret blinking beside freshly typed text merges with its last
characters - `Quokka77` read as `Quokka/` - so a check for the value just
typed failed against a field that held it. Text checks and the typed-value
read-back now capture twice, half a blink apart, and judge both readings; a
screen that is not blinking still costs one OCR pass. Separately, a search
stops as soon as a label is on screen, which could leave the field below it
past the window's bottom edge and the click outside the page; an action point
off the frame now scrolls a couple of lines and looks again.
