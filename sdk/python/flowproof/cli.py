"""Console entry point: delegates to the Rust CLI inside the extension
module so `flowproof` behaves identically however the engine is invoked."""

from __future__ import annotations

import sys

from flowproof import _native


def main() -> None:
    sys.exit(_native.cli_main(sys.argv[1:]))


if __name__ == "__main__":
    main()
