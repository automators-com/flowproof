# Lens: API compatibility

flowproof ships to npm and PyPI. **A revert does not help once someone has
installed it.**

Refuse, and say a human must decide, if the change alters:

- a public Rust API in any crate;
- the **trace format** or its JSON Schema - a trace is a contract with every
  cassette already committed, in this repository and in adopters';
- the shape of a committed cassette, or what replay accepts;
- a CLI flag, subcommand, or its output where something could be parsing it;
- a YAML spec key, or the meaning of an existing one;
- an environment variable's name or behaviour.

Silent behaviour changes matter as much as signature changes. A function that
keeps its type and returns something different is the harder break, because
nothing fails at compile time.

If the change is additive and nothing existing moves, say so plainly and
approve.
