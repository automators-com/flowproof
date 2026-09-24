**A plain step that needed a second screen now ends only when the model says it is complete, and a missing `${VAR}` fails without a repair run.**
A login step clicked "Send code", the page refused it, the model sent it again
and stopped, and the step recorded green without ever signing in. Once a step
has been continued, the engine now reads the screen once more and asks whether
anything remains, so a reply that simply stops no longer ends the step. A
recording that failed on an unset `${VAR}` (or an app it cannot drive) also ran
the repair loop and then a full fresh retry, doubling the wait for an error no
retry can fix; those failures are now reported at once.
