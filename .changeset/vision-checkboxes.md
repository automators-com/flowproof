**`app: vision` can check and uncheck a box.** Pixels give no checkbox state
to read, so vision refused every checkbox step. It now sets a box by watching
it: it clicks the label and looks at the box beside it - a tick, dot or fill
appearing is checked, disappearing is unchecked - and if the click went the
wrong way, because the box already held the wanted state, it clicks again and
confirms it came back. A click that changes nothing visible is refused with
the reason rather than assumed, since a disabled box, a radio button already
selected and a missed click look the same. Asserting a box's state is still
not something pixels can answer on their own.
