"""pytest fixtures for the code blocks in these pages; see tests/docs/fixtures.py."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tests", "docs"))

from fixtures import *  # noqa: E402,F401,F403
