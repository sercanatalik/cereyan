"""Public view of the current run: ``cereyan.runtime.run``, ``task_run``, ``flow``.

Each attribute is ``None`` outside a run.
"""

from __future__ import annotations

from . import context

__all__ = ["run", "task_run", "flow"]


def __getattr__(name: str):
    if name == "run":
        return context.current_run()
    if name == "task_run":
        return context.current_task_run()
    if name == "flow":
        run = context.current_run()
        return run.flow if run is not None else None
    raise AttributeError(name)
