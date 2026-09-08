---
title: "Security posture"
description: "Egress containment, filesystem observation, and the assert_no_secret_leak control."
---

The model boundary is small on purpose, and the small surface is the
security property.

- **The proxy binds loopback only.** It answers whatever asks it, with no
  authentication, so it must not be reachable off the machine running the
  test. Both replay and record bind `127.0.0.1`, whichever port they land on.
- **The upstream is fixed when the proxy starts and is NOT request-choosable.**
  Record mode is handed one upstream base URL at construction; a request body
  cannot redirect it. This is load-bearing: a proxy that let a request pick
  its own upstream would be an open relay pointed by whatever the system
  under test sent.
- **Replay has no network client at all.** It serves bytes from the cassette
  over a hand-rolled HTTP/1.1 listener - no TLS stack, no HTTP client, no
  outbound path. The one place flowproof reaches a real model is record mode,
  which touches reality by design and is the only non-hermetic step.
- **There are no dynamic code paths at the boundary.** Dispatch is fixed:
  a chat-completions request is served from the cassette or forwarded to the
  fixed upstream. Nothing in a request selects code to run.
- **Secrets go env -> header, never to disk.** A real-model key is read from
  flowproof's own environment straight into the outbound `Authorization` /
  `x-api-key` header. The trace stores request BODIES only, so a recorded
  cassette carries no key.

## Egress containment

The proxy contains the MODEL boundary. Egress containment is the second
half: a `command:` agent is a black-box process, and a black-box process can
open sockets to anywhere. On Linux, flowproof runs it under a real,
unprivileged, default-deny seccomp filter so a test can DECLARE the network
it is allowed to touch and CERTIFY it touched nothing else.

```yaml
app: agent
agent:
  command: python3 assistant.py
  allow_egress:
    - api.example.com:443          # host:port
    - 198.51.100.9:443             # ip:port
    - 10.0.0.0/8:443               # cidr:port
    - api.example.com              # bare host / ip: any port
    - ${SERVICE_HOST}:443          # ${VAR}, resolved at run, never stored
steps:
  - prompt: Book me a flight to Nairobi
  - assert_tool_call: create_booking
  - assert_no_egress               # certify: nothing undeclared was reached
```

`allow_egress` names the destinations the agent may reach ON LINUX. Say
that part out loud: the enforcement mechanism is Linux-only, so on macOS and
Windows the declaration is inert - it restricts nothing, and the agent
reaches whatever it likes. A flow that declares `allow_egress` WITHOUT an
`assert_no_egress` step therefore still passes on those hosts, which is why
the run record now carries the containment tier the run actually ran under
(see below): the artifact has to distinguish "contained and certified" from
"containment was not available here", because the verdict alone cannot. An
entry is
`host:port`, `ip:port`, `cidr:port`, or a bare `host`/`ip` for any port;
`${VAR}` references resolve at execution and are stored UNRESOLVED (a
resolved allow-list would leak the destination into the trace). Loopback
(`127/8`, `::1`) is exempt WHOLESALE, so the model proxy and any local MCP
server need not be listed. A hostname is resolved to its IP set once at run
start and pinned; the agent's own DNS lookups go to the loopback resolver,
which is exempt.

`assert_no_egress` is a bare step that CERTIFIES the run: the set of
undeclared destinations the agent attempted is empty. It is a CAPABILITY
claim - on any platform or driver where containment is not enforced it fails
outright ("cannot certify"), with no bypass flag, rather than passing
vacuously. Containment is enforced LIVE in both record and replay, so the two
phases share a denial environment and reproduce the same trajectory - a
determinism requirement, not an add-on.

A single-spec agent run prints its containment tier on every platform, and
every run that engages egress RECORDS it in the run record's control row
(`containment:`), where `flowproof audit` surfaces it. The printed line is
stdout on the single-spec path only; the recorded field is the one to read
in CI, and it is the one an auditor should ask for:

| Platform / driver | Tier |
|---|---|
| Linux, `command:` | **enforced** (seccomp) |
| macOS / Windows, `command:` | not contained (mechanism is Linux-only) |
| any `url:` service | not contained (flowproof did not start it, so it cannot contain it) |
| kernel < 5.6 | not contained (no seccomp user-notification / `pidfd_getfd`) |

The tier is recorded, not just printed. A control-bearing flow that engages
egress writes it into `.flowproof/runs/<id>/report.json`:

```yaml
control:
  id: sec.egress.declared
  verdict: pass
  lanes: [egress]                       # what the flow ASSERTED
  containment: not contained (egress containment is Linux-only; this platform is not contained)
```

`lanes` says what was asserted; `containment` says what was ENFORCED. A pass
on a host without containment is still a pass of the flow's other
assertions, but it is no longer indistinguishable from a certified one.
Blocked destinations travel in `evidence.blocked` only when THIS run was
contained: they are read from the recorded trace, so a Linux recording
replayed on a host without containment would otherwise present destinations
another machine blocked, on another day, as evidence for an uncontained run.

**How it works (Linux).** The child installs the filter in `pre_exec`
(`no_new_privs` then `seccomp(SECCOMP_SET_MODE_FILTER,
SECCOMP_FILTER_FLAG_NEW_LISTENER)`), and passes the notify fd to a parent
supervisor over a socketpair. For an address-bearing syscall the supervisor
copies the sockaddr out of child memory with `process_vm_readv`, checks
`SECCOMP_IOCTL_NOTIF_ID_VALID` AFTER the read, and decides on the COPY. An
allowed destination is connected by the supervisor itself (`pidfd_getfd`
dups the child's socket, same file description); it NEVER replies
`SECCOMP_USER_NOTIF_FLAG_CONTINUE` for connect/sendto/sendmsg, which would
let the kernel re-read child memory a sibling thread can rewrite between
check and use. `io_uring_setup` and `socket(AF_PACKET)` are refused at the
filter; a non-loopback listener is denied.

**Punts (v1).** Off-host unconnected UDP is denied rather than vetted
(loopback UDP, e.g. a local DNS resolver, is performed). DNS to `:53`
off-host, `io_uring`, and raw/packet sockets are refused, not proxied.
Inbound `listen` off loopback is denied but not otherwise brokered. A
local-relay exfil (writing to a loopback process that itself egresses) is
NOT caught - loopback is trusted wholesale. `AF_UNIX` is exempt on the same
terms, so a local socket bus is reachable. Containment is **network only**:
the filter's default action is allow, nothing outside the network syscalls
is ever denied, and `execve` is not examined at all. Destructive filesystem
syscalls ARE examined, but only to report them - see [Filesystem
observation](#filesystem-observation) below, which stops nothing.
`no_new_privs` breaks a setuid child. A
`url:` service and any non-Linux host are "not contained" by construction.
There is no runtime or production mode: this is a testing sandbox that fails
a test, not a jail that protects a host.

**`allow_egress` without `assert_no_egress` is not enforcement.** Declaring an
allow-list says which destinations the agent may reach; it is
`assert_no_egress` that turns the declaration into a claim, and it is the only
step that fails outright where containment is unavailable. A flow with the
declaration and no assertion still PASSES on macOS and Windows, uncontained,
having reached whatever it liked. Since 0.11 that run prints a warning naming
the allow-list, the reason it was not applied, and the step to add - but a
warning is what it is, and the assertion is what makes it a control.

## Filesystem observation

**This is not a control.** It asserts nothing, fails nothing, and has no
spec surface at all - there is no step to add and no key to declare. It is a
report, and it exists because a `command:` agent is a black-box process that
can delete a file without asking anyone.

Any flow that already engages containment gets it for free, because it is the
same seccomp filter. On Linux the report prints to stderr when, and only
when, a run destroyed something:

```
filesystem observation: observed (linux seccomp); 2 destructive syscall(s)
  unlinkat /home/u/exports/2025.csv at 412ms
  openat [O_WRONLY|O_CREAT|O_TRUNC] /home/u/db.sqlite at 899ms
```

Trapped: `unlink`, `unlinkat` (including `AT_REMOVEDIR`), `rmdir`,
`rename`/`renameat`/`renameat2`, `truncate`, `ftruncate`, `creat`, `openat2`,
and the open family **only when the flags carry `O_TRUNC`** - which is what
clobbering a file in place looks like, and what `>` redirection does. That
last test happens in-kernel via BPF `JSET`, so an ordinary read or an append
never reaches the supervisor and a contained run keeps its speed.

**The vocabulary is deliberately disjoint from containment's.** The tag is
`observation`, never `containment`; the value is `observed`, never
`enforced`. Nothing here is prevented: every trap replies
`SECCOMP_USER_NOTIF_FLAG_CONTINUE` and the syscall runs. That is also why the
paths can be trusted less than the events - on CONTINUE the kernel re-reads
child memory after the supervisor decided, so a sibling thread can rewrite a
path between the two. The trap fires on syscall NUMBER, which nothing can
race, so a path may be stale but a destructive syscall cannot hide.

A path the supervisor could not read is reported as unresolved rather than
dropped, since the trap already proved the syscall happened. Only a syscall
whose *destructiveness* could not be adjudicated - an `openat2` whose
`open_how` was unreadable - is a fault.

**It prints, and - since issue #465 - it is also recorded, redacted.** A
lane was designed once before and declined, and the objection deserves
keeping in its original words: a trace is a COMMITTED artifact and these
paths are absolute - `/home/alice/exports/acme-corp-2025.csv` would be
baked into a file that is reviewed and diffed forever. That is the same
argument that keeps `execve` out of the trap set for its argv. Issue #465
is the human act that reversed the decline - the lane's availability, not
the judgment about paths, which still holds: an absolute path never enters
a trace. An observed run now writes a `side_effects` lane whose records
keep a path only when it is workspace-relative by construction -
`./`-prefixed, the name the syscall used minus the workspace prefix and
any bare `.` components, no component rewritten - and redact everything
else (traversal forms included, never normalized-and-kept) to a
`sha256:` fragment of the captured path; the exact rules and the
confirmation-oracle residual are in
[trace-format.md](trace-format.md#side-effect-lane-app-agent). The lane is
scanned by the `assert_no_secret_leak` store-guard before the trace is
minted, and the stderr report above keeps full absolute-path fidelity
either way. So "what has this flow destroyed since March" finally has an
answer: names inside the workspace, hashes outside it.

**Punts, and they are real.** These are ATTEMPTS, not outcomes: the reply
goes out before the kernel runs the call, so an `rmdir` of a directory that
was not there reads exactly like one that removed a tree. `open(path,
O_WRONLY)` without `O_TRUNC` followed by a write at offset 0 corrupts a file
and fires nothing; catching it needs a trap on every `write`, which would put
a supervisor round-trip on every log line. Nothing is observed on macOS or
Windows, or on a flow that engages no containment. The recorded lane
inherits every one of these limits plus one of its own: a kept `./` target
is the NAME the syscall used, never a resolution claim - a symlinked
component can carry the actual victim elsewhere.

## Secret-leak control (`assert_no_secret_leak`)

A second agent-boundary control shares egress's honesty rules: a declared
secret must never appear in the agent's output. In v1 the scanned corpus is
the model-boundary trajectory (the cassette's request and response bodies)
plus each MCP lane; the step also works on `app: web` and `app: api` flows,
over the page surface text and `assert_api` response bodies. Only the variable
NAME travels in the trace, and because
the record-time scan runs before the trace is minted, a leak writes no trace
(a store-guard on flowproof's own cassette). The full form, its limits, and
how it folds into `flowproof audit` are documented with the rest of the
control grammar in
[authoring.md](../authoring/security-controls.md#assert_no_secret_leak-var-v1).
