**`app: vision` works on any monitor and at any display scaling.** Vision
captured the primary screen and cropped the window out of it, and moved the
mouse on a grid over the primary screen only - so a Citrix window on a second
monitor was read wrongly and clicked somewhere else, and one left of the
primary could not be reached at all. It now captures the window's own
rectangle wherever it is on the desktop, and mouse moves span every monitor.
Separately, the process never declared itself DPI aware: at 125% or 150%
scaling Windows hands an unaware process scaled window rectangles while its
screen captures stay physical, so the two disagreed. Vision now declares
per-monitor awareness before it looks at anything.
