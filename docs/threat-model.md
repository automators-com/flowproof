# flowproof threat model

> Status: v1, maintainer-written, not yet independently reviewed. This
> document is the current answer to "what does flowproof actually protect
> against, and where does that protection stop" — see `SECURITY.md` for how
> to report a gap in it. Tracked against
> [issue #376](https://github.com/automators-com/flowproof/issues/376).

## Why this document exists

flowproof sits at the model boundary and tells an adopter what an agent did:
which tools it called, with which arguments, whether it touched anything
outside an allow-list. An adopter uses that answer to decide whether the same
agent may run against a production system. If flowproof's own boundaries are
weaker than its output implies, the adopter's decision is wrong on the
strength of a tool that told them it was safe. This document exists to make
every "flowproof prevents X" claim traceable to the code that enforces it,
and to say plainly where no such code exists yet.

The method: state the guarantee, name the file and mechanism that enforces
it, and say what is explicitly *not* covered. A "Known limitations" ledger
below collects the gaps found while writing this — real, present-tense facts
about the code as of 2026-08-25, not a wishlist.

## Assets and actors

**Assets**: customer SAP GUI / Fiori credentials and session data used
during recording; model API keys (Anthropic/OpenAI-shaped); recorded traces
and cassettes, which may be committed and shared publicly; evidence
archives (`.flowproof/` run records, screenshots, reports) that customers may
attach to a support case; the integrity of a replay verdict itself.

**Actors**: the person recording a flow against a real system; the CI
identity replaying it later with no credentials at all; a third-party corpus
repository whose cassette flowproof's own test suite replays (untrusted by
design — see Cassette trust, below); a human or LLM reviewing a pull request
that adds or changes a cassette; an external security researcher.

## Trust boundaries

### Model-proxy credential boundary

**Guarantee**: the real model API key used during `flowproof record` never
reaches the recorded trace and never reaches the agent process under test.

**Mechanism**: `AgentProxy` (`crates/flowproof-adapters/src/agent_proxy.rs`)
is a loopback-only HTTP server (`agent_proxy.rs:81-83`: "Bound to 127.0.0.1
on purpose... it must not be reachable off the machine") that the
system-under-test's model client is pointed at instead of the real API. The
key is read once from environment (`FLOWPROOF_AGENT_KEY` /
`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) into flowproof's own process and
forwarded as a header to the real upstream on `record`. The cassette types
(`Turn`/`Message` in `flowproof-trace/src/cassette.rs`) have no auth field at
all — the trace stores request bodies only, structurally excluding the key
(comment at `agent_proxy.rs:255-258`). The spawned agent process itself is
stripped of the real credential before launch: `CREDENTIAL_VARS`
(`agent_runner.rs:58`) is removed from its environment in `configure()`
(`agent_runner.rs:438-461`), and given a placeholder value instead — it only
ever needs *a* key-shaped string to satisfy its own SDK, since it's really
talking to the loopback proxy. Regression test:
`the_auth_header_is_forwarded_but_never_captured`
(`agent_proxy.rs:2308`) — spins up a fake upstream, asserts the header
reaches it, asserts the serialized cassette does not contain the secret.

**Not covered**: the authoring LLM client (`flowproof-agent/src/llm.rs`,
used for `record`/`heal` step authoring, distinct from the agent-under-test's
proxy above) redacts its key from echoed error bodies but is a different code
path with a different key source (`FLOWPROOF_AI_API_KEY`) — reviewed
separately if it changes.

### Egress containment

**Guarantee**: `assert_no_egress` either certifies that an agent made no
network call outside its allow-list, or the run fails outright — it never
reports a clean result it cannot back up.

**Mechanism**: `Containment` (`crates/flowproof-adapters/src/egress.rs:17`)
is `Enforced` or `NotContained(String)`, always carrying a reason in the
second case. On **Linux**, `egress_linux.rs` installs a real seccomp-bpf
filter with `SECCOMP_RET_USER_NOTIF` on `connect`/`sendto`/`sendmsg`/
`sendmmsg`/`listen`, serviced by a TOCTOU-safe supervisor
(`process_vm_readv` + `pidfd_getfd`, with an `ID_VALID` re-check) — a real
kernel-enforced boundary, not a client-side check. On **Windows**,
`egress_windows/` implements per-run identity + WFP filters + a Job Object,
but `Containment::command_flow()` never claims `Enforced` as a pre-run
prediction (`egress.rs`, `egress_windows.rs:15-19`: a probe passing doesn't
mean the run will succeed) — the achieved tier is decided *after* the run.
On **macOS and any other platform**, `Containment::command_flow()`
unconditionally returns `NotContained(...)` — a true no-op, no filter of any
kind installed. What makes the no-op safe is `check_egress()`
(`agent_flow.rs:847`): if a flow declares `assert_no_egress` and containment
isn't enforced, record mints no trace and replay fails the flow — "assert_no_egress
cannot certify" (`agent_flow.rs:864`), with **no bypass flag**.

**Not covered — real gap**: `allow_egress` declared *without*
`assert_no_egress` degrades silently to a warning
(`egress_warning()`, `agent_flow.rs:912-940`) on any platform where
containment isn't enforced — the allow-list is not applied, and the agent can
reach anything. This exists so existing macOS/Windows flows that only
declare an allow-list don't all start failing, but it means an allow-list
alone is not an enforcement guarantee outside a successful Linux or Windows
run. See Known limitations, below.

The Linux filter also traps destructive filesystem syscalls
(`unlink(at)`, `rename(at)`, truncating opens) but **only to observe them**,
never to deny them — this is deliberate (module doc,
`egress_linux.rs:57-74`) and explicitly not a filesystem jail.

### Secret handling

**Guarantee (three separate, narrower mechanisms, not one broad scanner)**:

1. **Values never resolve into a trace.** `crates/flowproof-trace/src/secret.rs`'s
   `resolve_refs()` (`secret.rs:47`) means a spec's `${VAR}` reference is the
   only thing ever written to a trace or cassette — resolution happens only
   at the moment of use, on record and on every replay. A committed trace
   has the variable name, never the value. This holds regardless of whether
   a flow author remembers to assert anything.
2. **Screenshots are redacted at capture time, fail-closed.**
   `crates/flowproof-driver/src/redact.rs`'s `resolve_rects()` (line 61) and
   `apply()` (line 85) black out password-field rectangles
   (`driver.password_rects()`, always applied) and any configured `redact:`
   rule before a frame is ever PNG-encoded; if rectangle resolution errors,
   the frame is dropped rather than persisted unredacted.
3. **`assert_no_secret_leak: ${VAR}` is opt-in, per-flow.**
   `crates/flowproof-trace/src/secret_scan.rs`'s `scan_corpus()` (line 64)
   resolves a named variable and substring-searches it across the run's
   observable output (agent trajectory, web surface text, API bodies). This
   is a **declared-value substring match, not automatic secret detection** —
   no entropy analysis, no credential-pattern regex, and nothing is scanned
   unless a flow author names it. Values under `MIN_SECRET_LEN = 4`
   characters are refused outright rather than scanned imprecisely
   (`secret_scan.rs:25,74`). A detected leak fails the run before a trace is
   written — this is a store-guard, not a redaction.

**Not covered — real gap**: there is no automatic, always-on secret scanner
in this repository — not in flowproof's own execution path, and not at the
repository level. GitHub's own free secret scanning is currently **disabled**
on `automators-com/flowproof` (confirmed via the GitHub API while writing this
document). See Known limitations.

### `flowproof config` file at rest

**Guarantee**: `flowproof config sap`/`fiori`/`ai` (`plans/001-credential-config.md`,
`plans/008-ai-authoring-config.md`) write a SAP password, a Fiori password,
and/or an AI provider API key to one per-user file
(`crates/flowproof-cli/src/config.rs`). On Unix, that file is created `0600`
at write time (`config.rs:266-271`) — owner-read/write only.

**Not covered — real gap**: there is no Windows-side equivalent of the Unix
`0600` permission. The code comment at the same write site says so directly:
"No Windows-side equivalent yet — a stated gap, not a silent one." A Windows
user's `%APPDATA%\flowproof\config.yaml` therefore has whatever the OS
default ACL grants, which is typically already scoped to the owning user
account but is not something this codebase asserts or narrows itself. See
Known limitations.

### Debug artifacts (`debug/dom.html`, `debug/console.log`)

**Guarantee**: none currently exists for this specific artifact — stated here
precisely because the screenshot-redaction guarantee above could be mistaken
for covering it.

**Mechanism**: on a step failure, `augment_failure()`
(`crates/flowproof-replay/src/lib.rs:196`) calls `driver.debug_bundle()` and
writes the result to `<run_dir>/debug/dom.html` and `debug/console.log`. The
web adapter's implementation (`crates/flowproof-adapters/src/web.rs:2227`,
`fn debug_bundle`) captures `document.documentElement.outerHTML` and the
accumulated console buffer **verbatim** — neither passes through
`redact::apply` (which only operates on decoded pixel buffers) nor any
text-scrubbing equivalent.

**Not covered — real gap**: if a resolved secret is reflected into the DOM by
the application under test, or logged to the console by that application, it
can land in plaintext in this debug bundle on disk. `.flowproof/` is
gitignored so this does not enter git history by itself, but nothing stops
the bundle being attached to a bug report or a CI artifact upload as
"evidence" — at which point the secret leaves the machine. `docs/recording.md`'s
redaction section documents the screenshot guarantee and does not mention
this bundle. See Known limitations.

### `flowproof capture`

**Guarantee**: none — this command is a deliberately unauthenticated
debugging tool, not a production-safe surface, and says so out loud.

**Mechanism**: `crates/flowproof-cli/src/capture.rs`'s `cmd_capture()`
binds `0.0.0.0` (`capture.rs:351`, `Ipv4Addr::UNSPECIFIED`) — all
interfaces, on purpose, to support a sender on a different machine — logs
every request byte-for-byte to disk, and answers `200 OK`. It prints a
warning on every startup (`capture.rs:359`) and `docs/capture.md`'s "Bind
address and security" section states the accepted risk directly: run it
deliberately for a debugging session and stop it when done; do not leave it
running. This document doesn't relitigate that risk — it is intentional and
already documented at the point of use — but it belongs in the asset
inventory because it's the one place flowproof deliberately opens an
unauthenticated network surface.

### Cassette trust and deterministic re-emission (CHARTER invariant #10)

**Guarantee**: a cassette — recorded model I/O, including cassettes
originating from third-party corpus repositories flowproof's own test suite
does not trust — is never executed as code by flowproof itself.

**Mechanism**: `AgentProxy::serve_one` matches an incoming request against
the cassette via `Cassette::match_turn` and, on a match, serializes the
recorded `Message` back into HTTP/SSE response bytes. That content is only
ever compared byte-for-byte or serialized back out — nothing in
`flowproof-replay` or `flowproof-adapters` parses cassette text as a shell
command, template, or eval target, and replay makes no upstream call at all
(module doc, `agent_proxy.rs:10-16`). This is why CHARTER.md's invariant #10
requires a PR reviewer to judge a cassette by "size, turn count, schema
validity, secret-scan result and lane structure" and never by reading the
captured text: the identity reviewing the diff is itself a model, and
cassette content is exactly the shape of input that could target it. The
policy is a closed boundary, not a filter — "instruction-like content" is not
a specifiable property, since agent prompts *are* instructions.

**Not covered**: the trust boundary this doesn't reach is downstream —
whatever consumes the replayed text (the agent process under test, acting on
attacker-chosen tool-call arguments; or a human/model reading a diff) is
outside flowproof's control. flowproof's guarantee stops at "we don't
interpret it," not "nothing downstream can be affected by it."

### MCP stdio and HTTP stand-ins

**Guarantee**: during `record`, a declared `mocks:` tool is answered locally
and the real MCP server is never asked. During `replay`, zero external MCP
processes and zero network calls occur, for either transport.

**Mechanism**: both transports (`mcp_stdio.rs`, `mcp_http.rs`) share a
matching core (`mcp_core.rs`). `match_call()` (line 91) compares method and,
for `tools/call`, name and arguments; a mismatch or past-the-end call gets an
in-band JSON-RPC error rather than a hang. In `record`, the real external
server does run — a real subprocess for stdio, a real HTTP/SSE endpoint for
HTTP — for every call except ones named under `mocks:` in the spec, which are
answered by `mock_tool_result()` (line 206) without ever reaching the real
server. In `replay`, no external process or connection is started at all.

**Not covered**: server-initiated requests (sampling/elicitation/roots-list)
are explicitly unsupported and fail the recording loudly rather than being
silently mis-recorded (`server_request_named`, `mcp_core.rs:80`) — a named
limitation, not a silent gap.

### Subprocesses, timeouts, and process-tree teardown

**Guarantee**: an agent process that hangs past its deadline is killed; a
Windows-contained run's entire process tree is torn down when the run ends.

**Mechanism**: `wait_to_deadline()` (`agent_runner.rs:506`) polls the child
and kills it at the deadline. On **Windows**, `egress_windows/spawn.rs`
assigns the child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
(line 223) before resuming it — closing the job handle on teardown kills
everything still inside it, including grandchildren. This is the one place
in the repository with real process-tree-wide teardown.

**Not covered — real gap**: outside that Windows-contained path,
`wait_to_deadline()` kills only the direct child PID. A detached or
double-forked grandchild is not reaped when the deadline fires. On Linux, the
seccomp filter is inherited across `fork`/`exec` so a grandchild's *network
and destructive-filesystem syscalls* remain constrained — but that is not the
same guarantee as the process being terminated. `env_from` (suite manifests)
and `agent.command` are documented in-repo as an execute-arbitrary-code trust
surface equivalent to spec-authored commands, run via `sh -c` with no
timeout at all (`flowproof-cli/src/lib.rs`, `apply_env_from`) — not sandboxed,
by design, since a suite author is trusted to write their own data-setup
command.

### HTML report rendering

**Guarantee**: application and model text captured into a run's HTML/JUnit
report cannot inject markup into the report itself.

**Mechanism**: `crates/flowproof-replay/src/report.rs` has no templating
engine — hand-built strings with an explicit `escape()` (line 456) and
`xml_escape()` (line 464) applied at every interpolation site (step intent,
detail, run name, trace ID, file paths). Backed by tests that assert the
negative directly: `!html.contains("<two>")`, commented "raw input must
never reach HTML" (line 513), and the XML equivalent (line 728). The report
is self-contained — inline CSS, no external resources, no JS — closing off
supply-chain concerns from the report itself.

**Not covered — real gap**: this safety property is enforced by convention
and tests, not structurally. There is no templating engine or type system
guarantee that a future call site calls `escape()` before interpolating —
forgetting to would silently reintroduce an injection risk, and only a test
addition would catch it. See Known limitations.

### Evidence and artifact retention

**Guarantee**: `.flow.yaml` + `.trace.jsonl` are the committed, shareable
contract (traces hold only `${VAR}` references, never resolved values,
verified end to end per `docs/getting-started.md`). Everything else —
run records, screenshots, reports, debug bundles — lives under `.flowproof/`,
is gitignored, and is pruned locally to the most recent 10 records per suite
(`docs/design.md`).

**Not covered — real gap**: this is disk hygiene, not a data-governance
policy. Nothing in the docs states who owns a customer's recorded flow or
evidence archive, how long it should be retained once handed to Automators
(e.g. attached to a support case), or how it should be handled once it
leaves the customer's machine. This is a genuine policy gap, distinct from
the architecture above, which is safe-by-construction for the trace itself
but says nothing about the debug bundle (which is not — see above) or about
archives once exported. See Known limitations.

### Supply chain and package publication

**Guarantee**: released packages are traceable to this repository's CI, not
to a maintainer's local machine or a stored long-lived token.

**Mechanism**: PyPI publication (`.github/workflows/publish.yml`) uses PyPI
Trusted Publishing over OIDC — no stored token — gated by a tag-format check
and a `guard` job that fails if the tag doesn't match `Cargo.toml`'s version
or if that version is already published. npm publication
(`.github/workflows/publish-npm.yml`) uses `npm publish --provenance` (lines
165, 192) — genuine Sigstore-backed provenance, recorded on the public Rekor
transparency log. Both are manual/tag-triggered only.

**Not covered — real gap**: there is no `cargo-deny` configuration and no
automated `cargo audit` (or equivalent) CI job — confirmed absent from
`.github/workflows/`. Dependency vulnerability handling is currently manual
and reactive: the CHANGELOG records at least three ad hoc advisory fixes
(a `cryptography` GHSA in a dev lock file, two PyO3 RUSTSEC advisories fixed
by a version bump), plus two accepted-but-undocumented-outside-the-CHANGELOG
unmaintained-crate warnings (`paste` RUSTSEC-2024-0436, `ttf-parser`
RUSTSEC-2026-0192, both dev-dependencies). No Rust crates are published to
crates.io. See Known limitations.

## Known limitations

Collected here so they're visible in one place rather than only inside their
own sections above. None of these are fixed by this document — they are
named honestly, per the principle this document opened with.

1. **Debug bundle is unredacted.** `debug/dom.html` / `debug/console.log`
   can carry a resolved secret in plaintext if the application under test
   reflects or logs it at the moment a step fails. No redaction pass exists
   for this artifact today, and it isn't mentioned in `docs/recording.md`.
2. **`allow_egress` without `assert_no_egress` is silently unenforced**
   outside a working Linux or Windows containment run — a warning, not a
   failure.
3. **Secret scanning is opt-in and narrow.** No automatic/entropy-based
   detection exists in flowproof's own execution path, and GitHub's free
   secret scanning is currently disabled on this repository.
4. **HTML-report escaping is convention-enforced, not structural.** A future
   call site could forget `escape()`; only a test would catch it.
5. **No stated data-ownership/retention policy** for customer-provided flows
   and evidence archives once they leave local disk — distinct from (and not
   fixed by) the trace format's safe-by-construction design.
6. **No automated dependency-vulnerability gate.** `cargo-deny`/`cargo audit`
   are not run in CI; remediation has so far been manual and reactive.
7. **Uncontained-path timeouts kill only the direct child.** A detached
   grandchild process can outlive `wait_to_deadline()`'s kill outside the
   Windows Job Object path.
8. **`flowproof config`'s file has no Windows permission hardening.** The
   `0600` mode set on Unix (`config.rs:266-271`) has no counterpart on
   Windows, where the file can hold a plaintext SAP/Fiori password and an AI
   API key with only the OS's default ACL protecting it.

## Open questions — for a human, not a loop

Per `CHARTER.md` §8's own escalation pattern, and per `SECURITY.md`, these
are named rather than guessed:

- **Independent review scope, budget, and reviewer selection.** Tracked in
  issue #376; not decided here.
- **The acknowledgement/remediation windows in `SECURITY.md`** are a draft
  proposal pending sign-off, not a committed policy yet.
- **A v1.0 findings-blocking policy** — which severities block a release
  versus ship with a named, human-accepted residual risk — is undecided.
- **Whether, and in what order, the seven items above get fixed** rather
  than just documented. This document's job was to make them visible; fixing
  them is separate follow-up work, tracked as the team decides to take it on.
