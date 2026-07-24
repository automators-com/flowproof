# Changelog

All notable changes to flowproof are recorded here. Versions follow the
workspace version (Rust crates, the Python wheel, and the npm package move
together).

## 0.3.2

### Added

- **Egress containment for `app: agent` (Linux).** A `command:` agent flow
  can now declare the network it is allowed to reach and certify that it
  reached nothing else:
  - `agent.allow_egress`: a list of allowed destinations (`host:port`,
    `ip:port`, `cidr:port`, or a bare `host`/`ip` for any port). `${VAR}`
    references resolve at execution and are stored unresolved. Loopback is
    exempt wholesale, so the model proxy and local MCP servers need not be
    listed.
  - `assert_no_egress`: a step that certifies the set of undeclared
    destinations the agent attempted is empty. It is a capability claim - on
    any platform or driver where containment is not enforced it fails
    ("cannot certify") rather than passing vacuously.
  - On Linux, enforcement is a real, unprivileged, default-deny seccomp
    user-notification filter with a parent supervisor, live in both record
    and replay so the phases share a denial environment. The supervisor
    performs allowed connections itself over a `pidfd_getfd` dup of the
    child's socket and never uses `SECCOMP_USER_NOTIF_FLAG_CONTINUE` for
    address-bearing syscalls, closing the check-then-reuse race.
  - Every agent run prints its containment tier (enforced / not contained,
    with the reason) on every platform. macOS, Windows, `url:` services, and
    kernels older than 5.6 are reported "not contained".
  - The trace gains an additive egress audit lane (containment tier, the
    unresolved allow-list, and any denied attempts). A flow that does not use
    the feature serializes byte-identical to before.

See [docs/agent-testing.md](docs/agent-testing.md) for the grammar, the
per-platform honesty table, and the v1 limitations.
