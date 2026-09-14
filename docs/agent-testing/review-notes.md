---
title: "Settled in review"
description: "Design calls settled during review of the agent-testing surface."
---

The three questions this design left open have answers, and they are the
same answer three times: a test that quietly tolerates drift stops being a
test.

**Cassette matching is strict by BODY, and every turn is consumed exactly
once.** The sketch proposed matching a structural envelope plus a normalized
prompt hash, with named holes for volatile spans. Still rejected: an edited
prompt template is exactly what this feature exists to catch, so a matcher
with holes in it would be excused from catching the main case. A replayed
call must match a recorded turn byte-for-byte, and an extra call is a
failure.

*Position* was the contract in v1, and it has been dropped, because it
assumed something real agents do not provide: a strictly sequential
trajectory. goose issues its task call and a session-title call
CONCURRENTLY and does not wait for the second. Record sees whichever
lands first, replay serves from the cassette instantly and sees the other,
and a positional matcher reported a divergence when nothing about the
agent had changed. Order between concurrent calls is therefore not
asserted - the agent does not guarantee it, so a recording cannot either.
A sequential trajectory is unaffected: the earliest unconsumed match wins,
so turn K still matches turn K, and its divergence message is unchanged.

This is the "reordering tolerance" the first version of this section
deferred with "nothing has [demanded it]". The first third-party agent
tried demanded it.

Envelope comparison survived, but as a REPORTING rule rather than a
matching one: model, tool names and message roles are compared and
reported before any message body, because a byte diff of two 8000-token
prompts is unreadable and "you added a tool" is a one-line answer.

**Divergence fails at the first bad turn.** No searching forward for a
turn that fits. Once a trajectory has diverged its later turns say
nothing about the system under test, and continuing would report a
cascade whose only real cause was the first failure. (Reordering
tolerance, which this bullet once deferred, is now part of matching - see
above. Failing fast is unaffected: it is about not searching PAST a
genuine divergence, not about the order of concurrent calls.)

**`reply` is the final assistant message of the conversation the flow is
about.** Not the process's stdout, which this document originally
suggested. Stdout is whatever a harness chose to print - a banner, a
spinner, nothing at all - and it differs per driver.

"The trajectory's last assistant message" was the v1 rule, and it is not
enough, because an agent may talk to the model about something other than
the task. goose asks it to name the session, in a call with its own system
prompt, issued concurrently and not waited for. Its answer is an assistant
message, so `reply` became a coin flip: whichever call landed second won,
and `record` succeeded roughly two times in three.

A side conversation is recognisable by its system prompt, since turns that
continue one conversation share one. Turns are grouped by system prompt and
the thread with the most turns wins; ties go to the thread carrying the
most request text, because the conversation doing the work carries the
agent's real system prompt and its tool schemas while a housekeeping call
is small. Both halves are order-independent, which is the point.

It is a heuristic, and the limit is worth stating: an agent whose side
conversation is BIGGER than its real one would defeat the tie-break. A
cassette with a single system prompt - the ordinary case - takes the
identical path it always did.

A trajectory whose last turn is a tool call has not replied yet, which is a
real state and reads as absent rather than as empty text.
