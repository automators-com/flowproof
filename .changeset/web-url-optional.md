**A web flow no longer needs a `url:`.** Recording refused a web flow without
one, even when its first step was `Go to https://…`, so a flow that visits
several sites had to pretend one of them was the start. Without `url:` the
browser now opens blank and the steps navigate: `Go to <full address>`, or a
plain step such as "open https://… and sign in". A relative `Go to /path`, or
a step that looks for something before any page is open, says what to write
instead of reporting a missing element.
