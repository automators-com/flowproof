# Contributing to flowproof

Thanks for your interest in flowproof! This is an early-stage project; expect the
codebase to move quickly.

## Repository layout

- `crates/` — Rust workspace: driver (capture/input/UIA), trace (format + compiler),
  replay (deterministic executor), agent (planner loop + model backends), adapters
  (SAP GUI Scripting COM, web — behind feature flags), cli (`flowproof` binary).
- `sdk/python` — Python SDK (hatchling; will become PyO3/maturin bindings later).
- `sdk/js` — JavaScript SDK placeholder.
- `docs/` — design and format documentation.

## Toolchain

- Rust: stable toolchain. `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. The driver is Windows-native but the workspace must always
  build on Linux/macOS via the stub backend.
- Python (`sdk/python`): [uv](https://docs.astral.sh/uv/) recommended, `ruff check`,
  `pytest`.

## Pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`,
  `docs:`, `chore:`, with optional crate scope, e.g. `feat(trace): …`).
- Keep commits small and incremental.
- CI must pass: fmt, clippy (`-D warnings`), tests on Ubuntu and Windows, ruff + pytest.
- Changes to the trace format require updating both `docs/trace-format.md` and the JSON
  Schema in `crates/flowproof-trace/schema/`.

## License

Apache-2.0. By contributing you agree your contributions are licensed under the same
terms.
