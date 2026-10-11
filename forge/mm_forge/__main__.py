"""`python -m mm_forge <step>`: arguments as JSON on stdin, events as JSON lines on stdout (see `protocol`)."""

from __future__ import annotations

import sys
import traceback

from . import __version__, steps
from .protocol import capture_stdout, read_args


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if not argv or argv[0] in ("-h", "--help", "--list"):
        print(f"mm_forge {__version__}; steps:")
        for name, (_, doc) in sorted(steps.STEPS.items()):
            print(f"  {name:<14} {doc}")
        return 0 if argv else 2
    if argv[0] == "--version":
        print(__version__)
        return 0
    name = argv[0]
    with capture_stdout() as em:
        try:
            mod = steps.load(name)
            args = read_args(first_line=getattr(mod, "INTERACTIVE", False))
        except Exception as e:  # noqa: BLE001
            em.error(str(e), code="bad_request")
            return 2
        try:
            result = mod.run(args, em)
        except KeyboardInterrupt:
            em.error("interrupted", code="cancelled")
            return 130
        except Exception as e:  # noqa: BLE001
            traceback.print_exc()
            em.error(f"{type(e).__name__}: {e}", code="failed")
            return 1
        em.result(result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
