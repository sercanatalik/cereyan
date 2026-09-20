"""Searchable run attributes: key-values a flow sets on its own run at runtime,
filterable on the Runs page, `GET /api/runs?attributes=key=value`, and the MCP
`list_runs` tool, the same way parameters are."""

from __future__ import annotations

import json
import re
from typing import Any

from . import context
from .exceptions import CereyanError

_NAME = re.compile(r"[A-Za-z0-9_]+")


def set_attributes(**values: Any) -> None:
    """Merge ``values`` into the current run's attributes.

    Names are letters, digits, and underscores; values are any JSON. Later calls
    merge over earlier ones. Offline and served alike.

    Raises:
        CereyanError: Outside a run, or for a name that is not searchable.
    """
    run = context.current_run()
    if run is None or run.backend is None:
        raise CereyanError("set_attributes needs a running flow")
    for name in values:
        if not _NAME.fullmatch(name):
            raise CereyanError(f"attribute name {name!r} must be letters, digits, and underscores")
    run.backend.set_attributes(json.dumps(values, default=str))
