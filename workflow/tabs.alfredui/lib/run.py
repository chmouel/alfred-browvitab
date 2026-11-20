"""Entry point: `python3 -I run.py ARGS`."""

import os
import sys

if sys.version_info < (3, 9):  # noqa: UP036 (runs before anything else can fail)
    sys.stderr.write("browvitab: Python 3.9 or later is required\n")
    raise SystemExit(1)

# -I leaves this directory off sys.path.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
# Keep bytecode out of the workflow folder, which may be synced.
sys.pycache_prefix = os.path.expanduser("~/Library/Caches/browvitab/pycache")

from browvitab.cli import main  # noqa: E402

raise SystemExit(main(sys.argv))
